//! AICRM 客户端主窗口。
//!
//! 之前客户端"看得见"的东西几乎为零：服务在不在、登录没有、微信能不能采、
//! 云端连的是谁 —— 全靠猜。这个窗口把它们收成一屏：
//!
//! - 四张状态卡（服务 / 登录 / 微信 / 云端），每张都带能直接用的按钮
//! - 最近采集记录（扫本地 `data/jobs`）
//! - 启动 / 停止服务、去登录、打开数据目录
//!
//! **为什么直接读文件而不调本地接口**：这个窗口的职责之一就是"服务没跑时把服务
//! 拉起来"，所以它不能依赖服务本身。文件是两端都认的事实来源。
//!
//! 用法：
//! ```text
//! aicrm-console.exe                  打开主窗口（已有一个实例时交给它）
//! aicrm-console.exe --hidden         只进托盘，不弹窗口（开机自启用）
//! aicrm-console.exe --start-service  只启动后台服务，不显示窗口
//! ```
//!
//! **关闭窗口不等于退出程序**：点 × 只是把视口设为不可见，托盘图标留在通知区，
//! 点它就能把窗口叫回来。真正的退出在托盘菜单里 —— 这是微信 / Slack / Discord
//! 的通行做法，客户不会"关掉之后就找不回来了"。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cloud;
mod shell;
mod single;
mod theme;

use eframe::egui;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

/// 服务默认监听地址，与 `install.ps1` 的 `-Bind` 默认值一致。
const BIND: &str = "127.0.0.1:8765";
/// 不留控制台窗口地启动服务（与 `install.ps1` 的 `-WindowStyle Hidden` 等价）。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 状态自动刷新间隔：只读本地文件，开销可以忽略。
const REFRESH: Duration = Duration::from_secs(5);

fn main() -> eframe::Result {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    // 托盘「启动服务」走这条：起完就退，不弹窗口。
    if arguments.iter().any(|item| item == "--start-service") {
        match service_command().spawn() {
            Ok(_) => std::process::exit(0),
            Err(error) => {
                eprintln!("启动服务失败：{error}");
                std::process::exit(1);
            }
        }
    }

    // 开机自启走这条：只进托盘，不弹窗口
    let hidden = arguments.iter().any(|item| item == "--hidden");
    // 单实例：已经有一个界面在跑就把它叫到前台，自己退出 —— 不静默消失，
    // 也不许并排开第二个窗口（主界面与安装器共用同一把锁，所以也不会同时出现）。
    if !single::acquire() {
        single::activate_existing();
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // 加了左侧导航栏，改成横向比例：620 宽的竖窗塞不下侧栏 + 双列卡片
            .with_inner_size([780.0, 604.0])
            // 固定尺寸：两列卡片的布局在窄窗口下会把 CJK 文本压成竖排，
            // 控制面板本来也不需要缩放
            .with_resizable(false)
            .with_visible(!hidden)
            .with_title("AICRM"),
        ..Default::default()
    };
    eframe::run_native(
        "AICRM",
        options,
        Box::new(move |cc| {
            install_cjk_font(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(Console::new(!hidden)))
        }),
    )
}

/// egui 自带字体不含 CJK，不加载中文字体整个界面是方框。
fn install_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: [&str; 4] = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\Deng.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ];
    let Some(bytes) = CANDIDATES.iter().find_map(|path| std::fs::read(path).ok()) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "cjk".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("cjk".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// 安装目录：与服务、登录窗口用的是同一个约定。
fn install_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("AICRM-Windows-API")
}

/// 启动后台服务的命令。参数与 `install.ps1` 写进自启项的完全一致，
/// 云端三项凭据齐全时才接云端命令通道（跟服务自己的判断保持一致）。
/// 服务的启动参数。三处共用：直接拉起服务、写开机自启项、改云端地址时重写自启项。
///
/// 云端三项凭据齐全才接命令通道（跟服务自己的判断保持一致）；网页来源白名单跟着
/// `cloud.url` 走，否则云端换了地址之后网页端就再也拿不到本机令牌。
fn service_args() -> Vec<String> {
    let dir = install_dir();
    let mut args = vec![
        "--tools-dir".to_owned(),
        dir.join("tools").to_string_lossy().into_owned(),
        "--key-dir".to_owned(),
        dir.join("accounts").to_string_lossy().into_owned(),
        "--data-dir".to_owned(),
        dir.join("data").to_string_lossy().into_owned(),
        "--token-file".to_owned(),
        dir.join("api.token").to_string_lossy().into_owned(),
        "--bind".to_owned(),
        BIND.to_owned(),
    ];
    let (cloud, token, device) = (
        dir.join("cloud.url"),
        dir.join("device.token"),
        dir.join("device.id"),
    );
    if cloud.is_file() && token.is_file() && device.is_file() {
        args.push("--cloud-url-file".to_owned());
        args.push(cloud.to_string_lossy().into_owned());
        args.push("--device-token-file".to_owned());
        args.push(token.to_string_lossy().into_owned());
        args.push("--device-id".to_owned());
        args.push(read_trimmed(&device));
    }
    let url = read_trimmed(&cloud);
    if !url.is_empty() {
        args.push("--allow-origin".to_owned());
        args.push(url);
    }
    args
}

fn service_command() -> Command {
    let mut command = Command::new(install_dir().join("aicrm_windows_api.exe"));
    command
        .args(service_args())
        .creation_flags(CREATE_NO_WINDOW);
    command
}

/// 把开机自启项重写成当前参数（HKCU Run）。
///
/// 改云端地址时必须同步它 —— 自启项是"下次开机用什么参数启动"的唯一来源，
/// 只改 `cloud.url` 的话，重启后网页端白名单还停在旧地址上。
fn write_run_entry() -> Result<(), String> {
    let exe = install_dir().join("aicrm_windows_api.exe");
    let quoted = service_args()
        .iter()
        .map(|arg| format!("\"{arg}\""))
        .collect::<Vec<_>>()
        .join(" ");
    let value = format!("\"{}\" {}", exe.display(), quoted);
    // 走 -File 而不是 -Command：`powershell -Command "..."` 会把命令行再解析一遍，
    // 值里的双引号会被吃掉（实测过）。写进脚本文件就没有这层解析。
    let script = std::env::temp_dir().join("aicrm-set-run.ps1");
    // PowerShell 单引号串里反斜杠是字面量，只需把单引号翻倍转义
    std::fs::write(
        &script,
        format!(
            "Set-ItemProperty -Path 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run' \
             -Name 'AICRM-Windows-API' -Value '{}' -Force",
            value.replace('\'', "''")
        ),
    )
    .map_err(|error| format!("无法写临时脚本：{error}"))?;
    let _ = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            &script.to_string_lossy(),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("无法更新自启项：{error}"))?;
    Ok(())
}

fn read_trimmed(path: &Path) -> String {
    std::fs::read_to_string(path)
        .map(|text| text.trim().to_owned())
        .unwrap_or_default()
}

/// 打开登录窗口；云端地址已配置时直接带上，省得用户再输。
///
/// 登录界面是**独立进程**（aicrm-login.exe），所以这里必须先查重 ——
/// 否则用户点几次「重新登录」就会堆出几个一模一样的窗口。
fn open_login() {
    const TITLE: &str = "AICRM 登录";
    let wide: Vec<u16> = TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 传入以 NUL 结尾的宽字符串；窗口类传空表示只按标题查找。
    let existing = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW(std::ptr::null(), wide.as_ptr())
    };
    if !existing.is_null() {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(
                existing,
                windows_sys::Win32::UI::WindowsAndMessaging::SW_RESTORE,
            );
            windows_sys::Win32::UI::WindowsAndMessaging::SetForegroundWindow(existing);
        }
        return;
    }

    let dir = install_dir();
    let mut command = Command::new(dir.join("aicrm-login.exe"));
    let url = read_trimmed(&dir.join("cloud.url"));
    if !url.is_empty() {
        command
            .arg("--endpoint")
            .arg(format!("{}/api/auth/login", url.trim_end_matches('/')))
            .arg("--token-file")
            .arg(dir.join("api.token"))
            .arg("--device-file")
            .arg(dir.join("device.id"));
    }
    let _ = command.spawn();
}

fn stop_service() {
    let _ = Command::new("taskkill")
        .args(["/IM", "aicrm_windows_api.exe", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

/// 结束除自己以外的全部客户端进程（本地服务、登录器、托盘）。
///
/// 托盘「退出」要的是"干净地关掉"，而不只是关掉界面 —— 本地服务是独立进程，
/// 不显式收掉就会留在后台占着 8765：用户以为退出了、其实还在跑，重启或重装
/// 都得先去任务管理器手动杀进程。调用者自己不在这个列表里，由调用处 process::exit 退出。
fn stop_other_client_processes() {
    for name in ["aicrm_windows_api.exe", "aicrm-login.exe"] {
        let _ = Command::new("taskkill")
            .args(["/IM", name, "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
}

/// 是否已经有另一个实例在跑（自己也在进程列表里，所以数到 2 才算重复）。
///
/// 没有用互斥量：托盘每次点击都会拉起一次程序，开出一串窗口是必然的事故，
/// 这个检查足够挡住，而且不引入额外的依赖。
/// 连接云端：登录 → 申领配对码 → 交给 install.ps1 完成配对。
///
/// 配对这一步复用安装脚本而不是在 Rust 里重写：令牌生成（`New-HexToken`）、
/// 收紧文件权限、**重写开机自启项里的 `--device-id`**、重启服务 —— 这些都在那边
/// 验证过了。漏掉"重写自启项"这一条，下次开机就会用回旧身份、恒定 401。
fn connect_task(base: &str, username: &str, password: &str) -> Result<String, String> {
    let session = cloud::login(&format!("{base}/api/auth/login"), username, password, "console")
        .map_err(|error| error.message().to_owned())?;
    let provision = cloud::create_provision(base, &session.access_token)?;

    let script = install_dir().join("install.ps1");
    if !script.is_file() {
        return Err("找不到安装脚本，无法完成配对。请重新运行安装程序。".to_owned());
    }
    let output = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-CloudUrl")
        .arg(base)
        .arg("-DeviceId")
        .arg(&provision.device_id)
        .arg("-ProvisionCode")
        .arg(&provision.provision_code)
        .arg("-Endpoint")
        .arg(format!("{base}/api/auth/login"))
        .arg("-AllowOrigin")
        .arg(base)
        // 别让脚本把调用它的我们停掉（脚本默认会停 console 以便覆盖文件）。
        .arg("-KeepConsole")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("无法运行安装脚本：{error}"))?;

    if output.status.success() {
        Ok(format!(
            "已连接云端，设备 {}。以后打开直接进主界面。",
            provision.device_id
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

/// 这台机器是否已经和云端配过对。
///
/// 判据是 `device.token` 在不在 —— 它是设备身份的凭据，没有它服务上报会被云端
/// 直接拒（401），所以首次打开必须先把它办下来。
fn paired() -> bool {
    let path = install_dir().join("device.token");
    std::fs::read_to_string(&path)
        .map(|text| text.trim().len() >= 32)
        .unwrap_or(false)
}

/// 云端基地址：装的时候写进 `cloud.url`，改服务器只改这个文件。
fn cloud_base() -> String {
    let url = read_trimmed(&install_dir().join("cloud.url"));
    if url.is_empty() {
        "http://127.0.0.1:8081".to_owned()
    } else {
        url.trim_end_matches('/').to_owned()
    }
}

fn service_up() -> bool {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8765);
    TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok()
}

/// 云端可达性探针：后台线程每 15 秒读一次 `cloud.url` 探一次，绘制线程只读缓存。
///
/// 必须放后台 —— 在绘制线程里做网络请求会直接把界面卡住。
fn spawn_probe(slot: Arc<Mutex<Option<bool>>>) {
    std::thread::spawn(move || loop {
        if let Ok(mut value) = slot.lock() {
            *value = probe_cloud();
        }
        std::thread::sleep(Duration::from_secs(15));
    });
}

/// 探一下云端根路径：能拿到任何 HTTP 响应就算通（404 也算，说明服务活着）。
fn probe_cloud() -> Option<bool> {
    let url = read_trimmed(&install_dir().join("cloud.url"));
    if url.is_empty() {
        return None;
    }
    let agent: ureq::Agent = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .http_status_as_error(false)
        .build()
        .into();
    Some(agent.get(&url).call().is_ok())
}

fn ago(time: Option<SystemTime>) -> String {
    let Some(elapsed) = time.and_then(|time| SystemTime::now().duration_since(time).ok()) else {
        return "—".to_owned();
    };
    match elapsed.as_secs() {
        seconds if seconds < 60 => "刚刚".to_owned(),
        seconds if seconds < 3600 => format!("{} 分钟前", seconds / 60),
        seconds if seconds < 86_400 => format!("{} 小时前", seconds / 3600),
        seconds => format!("{} 天前", seconds / 86_400),
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 3] = ["B", "KB", "MB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 一次状态快照。全部来自本地文件与一次端口探测，不依赖服务在线。
#[derive(Default)]
struct Snapshot {
    service_up: bool,
    /// 令牌类型（云端会话 / 本地令牌）+ 写入时间
    token: Option<(String, Option<SystemTime>)>,
    accounts: Vec<String>,
    /// 真正可用的微信号：密钥本体与解密缓存（cache/）都在。
    /// 只有密钥没有 cache 时导出读不到库，必须区分出来。
    accounts_ready: Vec<String>,
    credential_ready: bool,
    /// 微信号（昵称），从导出的 messages.json 里找；找不到回落显示 wxid
    wechat_name: Option<String>,
    cloud: Option<(String, String)>,
    jobs: Vec<Job>,
    /// 已登录的账号名，来自 `account.name`（登录窗口写入）。None = 未登录。
    account: Option<String>,
    /// 当前账号在本机绑定的微信号，由服务从云端拉取后缓存。None = 还没拉到。
    bound_wxid: Option<String>,
    /// 绑定微信号的昵称（云端记录），用于优先显示。
    bound_name: Option<String>,
    /// 正在跑的服务版本。界面右下角显示它 —— 见 [`snapshot`]。
    service_version: Option<String>,
}

/// 侧边栏的两个页面。
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Page {
    #[default]
    Main,
    Config,
    /// 首次打开、还没和云端配对过时的引导页。配对完成后就不再出现。
    Connect,
}

/// 连接引导页的状态。配对是网络操作，必须放后台线程，否则界面会卡死。
struct Connect {
    username: String,
    password: String,
    /// 正在进行：非空表示忙碌，界面锁输入
    step: Option<String>,
    error: Option<String>,
    pending: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
}

impl Default for Connect {
    fn default() -> Self {
        Self {
            username: String::new(),
            password: String::new(),
            step: None,
            error: None,
            pending: None,
        }
    }
}

#[derive(Default, Clone)]
struct Job {
    name: String,
    size: u64,
    modified: Option<SystemTime>,
}

/// 解密缓存（聊天数据库）是否已经生成。
///
/// 位置是 `<安装目录>/accounts/cache/*.db` —— 见账号目录里的 config.json：
/// `"decrypted_dir": "cache"`，相对密钥目录解析。（老布局在 wxid 子目录下，一并认。）
///
/// 服务端那份同源实现在 `backend::service::decrypted_cache_ready`；界面是独立程序
/// （不挂 backend 模块），所以这里保留一份等价实现。改的时候两边都要改。
fn decrypted_cache_ready(accounts: &std::path::Path, wxid: &str) -> bool {
    [accounts.join("cache"), accounts.join(wxid).join("cache")]
        .iter()
        .any(|dir| {
            std::fs::read_dir(dir).is_ok_and(|entries| {
                entries.flatten().any(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("db"))
                })
            })
        })
}

fn snapshot() -> Snapshot {    let dir = install_dir();
    let mut snap = Snapshot {
        service_up: service_up(),
        ..Default::default()
    };

    let token_path = dir.join("api.token");
    if let Ok(text) = std::fs::read_to_string(&token_path) {
        let text = text.trim();
        if !text.is_empty() {
            let kind = if text.starts_with("eyJ") {
                "云端会话"
            } else {
                "本地令牌"
            };
            snap.token = Some((
                kind.to_owned(),
                std::fs::metadata(&token_path)
                    .ok()
                    .and_then(|meta| meta.modified().ok()),
            ));
        }
    }

    // 账号名由登录窗口写进 account.name；文件不在就是没登录
    snap.account = std::fs::read_to_string(dir.join("account.name"))
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|name| !name.is_empty());

    // 服务版本：由服务启动时写进 data/state/version.json。
    //
    // 界面**不能**显示自己的 `CARGO_PKG_VERSION`：自更新换的是服务端程序，
    // 界面程序没被换过，于是"升完级版本号还是旧的"，看起来就像更新失败。
    // 版本号只有一个真相源 —— 正在跑的这份服务。
    if let Ok(raw) = std::fs::read_to_string(dir.join("data").join("state").join("version.json"))
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
            snap.service_version = value
                .get("version")
                .and_then(|item| item.as_str())
                .map(str::to_owned)
                .filter(|item| !item.is_empty());
        }
    }

    // 当前账号绑定的微信号：由服务从云端拉取后写在这里。界面只读缓存，不发网络请求。
    if let Ok(raw) = std::fs::read_to_string(dir.join("data").join("state").join("bound-wechat.json"))
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
            snap.bound_wxid = value
                .get("wxid")
                .and_then(|item| item.as_str())
                .map(str::to_owned)
                .filter(|item| !item.is_empty());
            snap.bound_name = value
                .get("name")
                .and_then(|item| item.as_str())
                .map(str::to_owned)
                .filter(|item| !item.is_empty());
        }
    }

    if let Ok(entries) = std::fs::read_dir(dir.join("accounts")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("wxid_") && entry.path().is_dir() {
                // 一个微信号的密钥要「真的能用」得两样都在：
                //   credentials.local.json —— 抓到的密钥本体
                //   cache/                 —— 用它解密出来的聊天数据库
                // 只有密钥没有 cache 时，导出程序读不到聊天库会一直空等
                // （实测卡到云端 15 分钟超时），界面却写着「密钥就绪」。
                //
                // 缓存的真实位置是 **key-dir/cache/**（见账号目录的 config.json：
                // `"decrypted_dir": "cache"`，相对 key-dir 解析），不是 wxid 子目录下。
                // 查错路径会把已经就绪的机器显示成「密钥不完整」。
                let has_credential = entry.path().join("credentials.local.json").is_file();
                let has_cache = decrypted_cache_ready(&dir.join("accounts"), &name);
                if has_credential {
                    snap.credential_ready = true;
                }
                if has_credential && has_cache {
                    snap.accounts_ready.push(name.clone());
                }
                snap.accounts.push(name);
            }
        }
        snap.accounts.sort();
        snap.accounts_ready.sort();
    }
    if !snap.accounts.is_empty() {
        snap.wechat_name = self_name_from_exports(&dir.join("data").join("jobs"));
    }

    let url = read_trimmed(&dir.join("cloud.url"));
    let device = read_trimmed(&dir.join("device.id"));
    if !url.is_empty() && !device.is_empty() {
        snap.cloud = Some((url, device));
    }

    let jobs_root = dir.join("data").join("jobs");
    if let Ok(entries) = std::fs::read_dir(&jobs_root) {
        let mut jobs: Vec<Job> = entries
            .flatten()
            .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
            .map(|entry| Job {
                name: entry.file_name().to_string_lossy().into_owned(),
                size: dir_size(&entry.path()),
                modified: entry.metadata().ok().and_then(|meta| meta.modified().ok()),
            })
            .collect();
        jobs.sort_by(|a, b| b.modified.cmp(&a.modified));
        jobs.truncate(5);
        snap.jobs = jobs;
    }
    snap
}

/// 从本地导出的 messages.json 里找机主自己的微信号（昵称）。
///
/// 与后台服务心跳里的 `wechat.name` 同源：`sender.is_self == true` 的那条消息，
/// 其 `sender.display_name` 就是微信号（如 "flow state"）。只翻 20 份导出，
/// 找不到回落显示 wxid。
fn self_name_from_exports(root: &Path) -> Option<String> {
    let mut stack = vec![root.to_path_buf()];
    let mut opened = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if entry.file_name() == "messages.json" {
                opened += 1;
                if opened > 20 {
                    return None;
                }
                if let Some(name) = self_name_in_export(&path) {
                    return Some(name);
                }
            }
        }
    }
    None
}

/// 读一份 messages.json，返回机主的 display_name；没有就给 None，继续翻下一份。
fn self_name_in_export(path: &Path) -> Option<String> {
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    for message in value.get("messages")?.as_array()? {
        let sender = message.get("sender")?;
        if sender.get("is_self") == Some(&serde_json::Value::Bool(true)) {
            let name = sender.get("display_name").and_then(|name| name.as_str());
            if let Some(name) = name.filter(|name| !name.is_empty()) {
                return Some(name.to_owned());
            }
        }
    }
    None
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => dir_size(&entry.path()),
            _ => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
        })
        .sum()
}

struct Console {
    snap: Snapshot,
    refreshed: Option<Instant>,
    note: Option<String>,
    /// 正在编辑的云端地址；None = 非编辑态
    editing: Option<String>,
    /// 云端可达性，由后台线程刷新
    reachable: Arc<Mutex<Option<bool>>>,
    /// 通知区托盘；Windows 上要等事件循环起来才建得出来，所以第一帧才创建
    tray: Option<shell::Tray>,
    /// 建不出来就别每帧重试
    tray_failed: bool,
    /// 窗口是否可见 —— 视口隐藏后 egui 不会回头告诉我们，自己记一份
    visible: bool,
    /// 是否由本窗口托管本地服务：开着就"没起就拉起、掉了就重启"。
    ///
    /// 用户手动点「停止服务」时置 false —— 那是明确的手停，不该被自动拉起来
    /// 打脸；再点「启动服务」或重开窗口才恢复托管。
    auto_service: bool,
    /// 当前页，由侧边栏切换
    page: Page,
    /// 右上角账号菜单是否展开
    account_menu: bool,
    /// 连接引导页的表单与后台任务。只在未配对时用得到。
    connect: Connect,
}

impl Console {
    fn new(visible: bool) -> Self {
        let reachable = Arc::new(Mutex::new(None));
        spawn_probe(reachable.clone());
        Self {
            snap: Snapshot::default(),
            refreshed: None,
            note: None,
            editing: None,
            reachable,
            tray: None,
            tray_failed: false,
            visible,
            // 默认托管：打开窗口就等于"服务该在跑"，掉了自动拉回来。
            auto_service: true,
            // 没配对过就先引导连接：设备身份是与云端通信的前提，
            // 少了它服务起来也只会一直 401。
            page: if paired() { Page::Main } else { Page::Connect },
            account_menu: false,
            connect: Connect::default(),
        }
    }

    fn refresh(&mut self) {
        self.snap = snapshot();
        self.refreshed = Some(Instant::now());
        self.keep_service_alive();
    }

    /// 开始连接：后台跑登录 → 申领配对码 → 交给安装脚本完成配对。
    fn start_connect(&mut self, ctx: &egui::Context) {
        if self.connect.step.is_some() {
            return;
        }
        if self.connect.username.trim().is_empty() || self.connect.password.is_empty() {
            self.connect.error = Some("请填写账号和密码".to_owned());
            return;
        }
        let base = cloud_base();
        let username = self.connect.username.trim().to_owned();
        let password = std::mem::take(&mut self.connect.password);
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = sender.send(connect_task(&base, &username, &password));
            ctx.request_repaint();
        });
        self.connect.pending = Some(receiver);
        self.connect.step = Some("正在连接云端…".to_owned());
        self.connect.error = None;
    }

    /// 收后台连接任务的结果。
    fn poll_connect(&mut self) {
        let Some(receiver) = &self.connect.pending else {
            return;
        };
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("连接线程异常退出".to_owned()),
        };
        self.connect.pending = None;
        self.connect.step = None;
        match outcome {
            Ok(message) => {
                self.page = Page::Main;
                self.note = Some(message);
                self.refresh();
            }
            Err(message) => self.connect.error = Some(message),
        }
    }

    /// 托管本地服务：没在跑就拉起来，跑着就什么都不做。
    ///
    /// 这是每 5 秒一次的常规刷新里顺带做的 —— 服务是采集和心跳的载体，它掉了
    /// 云端立刻把这台设备标成离线，网页端也就点不动"绑定微信"。以前只能靠用户
    /// 自己发现状态栏变红再手点启动，等于把可用性押在用户盯着看上。
    fn keep_service_alive(&mut self) {
        if !self.auto_service || self.snap.service_up {
            return;
        }
        if let Err(error) = service_command().spawn() {
            self.note = Some(format!("本地服务拉起失败：{error}"));
        } else {
            self.note = Some("本地服务已自动拉起".to_owned());
        }
    }

    /// 显示 / 隐藏主窗口。隐藏不是退出，进程和托盘都还在。
    fn set_visible(&mut self, ctx: &egui::Context, visible: bool) {
        self.visible = visible;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(visible));
        if visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn cloud_reachable(&self) -> Option<bool> {
        self.reachable.lock().ok().and_then(|value| *value)
    }

    /// 退出登录：丢掉云端会话，换一把新的本地接口令牌。
    ///
    /// 不是删掉令牌文件 —— 服务启动时要求令牌至少 32 位可见字符，空文件会让服务
    /// 起不来；`device.token`（设备身份）更不能动，换了它就得重新配对。
    fn logout(&mut self) {
        let dir = install_dir();
        let mut fresh = String::new();
        while fresh.len() < 64 {
            fresh.push_str(&uuid::Uuid::new_v4().simple().to_string());
        }
        fresh.truncate(64);
        // 顺手清掉账号名，右上角随即回到"未登录"
        let _ = std::fs::remove_file(dir.join("account.name"));
        match std::fs::write(dir.join("api.token"), fresh) {
            Ok(()) => {
                // 服务把令牌读在内存里，重启才换得掉
                if self.snap.service_up {
                    stop_service();
                }
                self.note = Some("已退出登录；启动服务后使用新令牌".to_owned());
            }
            Err(error) => self.note = Some(format!("写入失败：{error}")),
        }
        self.refresh();
    }

    /// 保存云端地址。
    ///
    /// 服务每次请求都重读 `cloud.url`，所以下一轮心跳（≤20 秒）就用新地址 ——
    /// 不用重启服务，也不需要重新配对：设备身份在 `device.id` / `device.token` 里，
    /// 只要云端还是同一个库，客户端换地址后照样被认出来。
    fn save_cloud(&mut self) {
        let Some(draft) = self.editing.take() else {
            return;
        };
        let url = draft.trim().trim_end_matches('/').to_owned();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            self.note = Some("地址要以 http:// 或 https:// 开头".to_owned());
            self.editing = Some(draft);
            return;
        }
        if let Err(error) = std::fs::write(install_dir().join("cloud.url"), &url) {
            self.note = Some(format!("写入失败：{error}"));
            self.editing = Some(draft);
            return;
        }
        self.note = Some(match write_run_entry() {
            Ok(()) => "已保存：下一轮心跳即用新地址，自启项同步更新".to_owned(),
            Err(error) => format!("地址已生效，但{error}"),
        });
        self.refresh();
    }
}

impl eframe::App for Console {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.refreshed.is_none_or(|last| last.elapsed() >= REFRESH) {
            self.refresh();
        }
        // 收后台连接任务的结果：不轮询的话连上了界面也不会跳进主界面
        self.poll_connect();

        // 托盘惰性创建：事件循环没起来之前建不出来
        if self.tray.is_none() && !self.tray_failed {
            match shell::Tray::new(ctx) {
                Ok(tray) => self.tray = Some(tray),
                Err(error) => {
                    self.tray_failed = true;
                    // 同时写 stderr：托盘建不出来时界面上那行提示很容易被忽略，
                    // 而它直接导致"点 × 就是退出、没有托盘可回"。
                    eprintln!("托盘创建失败：{error}");
                    self.note = Some(format!("托盘创建失败：{error}"));
                }
            }
        }

        if let Some(tray) = &mut self.tray {
            match tray.poll(self.snap.service_up, self.visible) {
                Some(shell::Action::Toggle) => {
                    let next = !self.visible;
                    self.set_visible(ctx, next);
                }
                Some(shell::Action::StartService) => {
                    match service_command().spawn() {
                        Ok(_) => self.note = Some("正在启动服务…".to_owned()),
                        Err(error) => self.note = Some(format!("启动失败：{error}")),
                    }
                    self.refresh();
                }
                Some(shell::Action::StopService) => {
                    stop_service();
                    self.note = Some("已发送停止指令".to_owned());
                    self.refresh();
                }
                // 直接退出进程：走视口 Close 会再触发一次下面的关闭拦截
                Some(shell::Action::Quit) => {
                    // 托盘「退出」= 完全退出：先收掉本地服务等子进程，再退自己。
                    // 只关界面会把 aicrm_windows_api.exe 留在后台占着 8765。
                    stop_other_client_processes();
                    std::process::exit(0);
                }
                None => {}
            }
        }

        // 点 × = 隐藏到托盘，不退出（真正的退出在托盘菜单里）。
        // 托盘建不出来时不能拦截 —— 那就真成了"关掉就找不回来"。
        if self.tray.is_some() && ctx.input(|input| input.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.set_visible(ctx, false);
        }

        ctx.request_repaint_after(REFRESH);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let height = ui.available_height();
        let width = ui.available_width();
        egui::Frame::NONE.fill(theme::CONTENT).show(ui, |ui| {
            ui.set_width(width);
            ui.set_height(height);
            // 还没连上云端时不画侧边栏：那会儿"主界面 / 配置"两个入口都没有意义，
            // 摆出来只会让人以为已经能用了。连接引导页自己占满整宽。
            if self.page == Page::Connect {
                self.connect_page(ui);
                return;
            }
            ui.horizontal_top(|ui| {
                self.sidebar(ui, height);
                self.content(ui, height);
            });
        });
    }
}

impl Console {
    /// 左侧导航栏。总宽 130（含左右各 10 的内边距），内容宽 110。
    ///
    /// 130 是收窄后的取值：三字导航（"主界面"）还放得下，再窄就得换图标，
    /// 而只有两个入口时图标反而更难认。
    fn sidebar(&mut self, ui: &mut egui::Ui, height: f32) {
        egui::Frame::NONE
            .fill(theme::SIDEBAR)
            .inner_margin(egui::Margin::symmetric(10, 0))
            .show(ui, |ui| {
                ui.set_width(110.0);
                ui.set_height(height);
                ui.vertical(|ui| {
                    ui.add_space(18.0);
                    ui.label(egui::RichText::new("AICRM").size(14.0).strong());
                    ui.add_space(16.0);

                    if theme::nav_item(ui, "主界面", self.page == Page::Main) {
                        self.page = Page::Main;
                    }
                    ui.add_space(2.0);
                    if theme::nav_item(ui, "配置", self.page == Page::Config) {
                        self.page = Page::Config;
                    }

                    // 界面不显示版本号（对外只叫 AICRM）。版本仍随心跳上报，
                    // 需要时在云端「设备」页看。
                });
            });
    }

    /// 右侧：一条顶栏 + 当前页内容。顶栏横跨整宽，内容区左右留 20。
    fn content(&mut self, ui: &mut egui::Ui, height: f32) {
        let width = ui.available_width().max(1.0);
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.set_height(height);
            self.top_bar(ui, width);
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 18))
                .show(ui, |ui| {
                    ui.set_width(width - 40.0);
                    match self.page {
                        Page::Main => self.main_page(ui),
                        Page::Config => self.config_page(ui),
                        // 连接引导在主内容区里也要能画：它跟主界面共用顶栏
                        Page::Connect => self.connect_page(ui),
                    }
                    // 版本号放窗口内右下角：对外只叫 AICRM，版本信息在程序内部可见。
                    // 显示的是**服务实际在跑的版本**（服务不在时才回落到界面自己的），
                    // 否则自更新换掉服务端之后，这里会一直显示界面程序的旧版本号。
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::RIGHT), |ui| {
                        ui.add_space(4.0);
                        let version = self
                            .snap
                            .service_version
                            .clone()
                            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
                        ui.label(theme::faint(&format!("v{version}")));
                    });
                });
        });
    }

    /// 顶栏。右上角只放账号名 —— "谁登录了"是这里唯一值得说的信息，
    /// 头像、登录状态、下拉箭头都是噪音。
    fn top_bar(&mut self, ui: &mut egui::Ui, width: f32) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 56.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::ZERO, theme::CARD);
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            egui::Stroke::new(1.0, theme::LINE),
        );

        let label = self
            .snap
            .account
            .clone()
            .unwrap_or_else(|| "未登录".to_owned());
        let font = egui::FontId::proportional(13.0);
        let galley = ui
            .painter()
            .layout_no_wrap(label.clone(), font.clone(), theme::TEXT);
        let text_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 20.0 - galley.size().x, rect.center().y - 9.0),
            egui::vec2(galley.size().x, 18.0),
        );

        let response = ui.interact(
            text_rect.expand2(egui::vec2(8.0, 7.0)),
            ui.id().with("account"),
            egui::Sense::click(),
        );
        let color = if response.hovered() {
            theme::ACCENT
        } else {
            theme::TEXT
        };
        ui.painter().text(
            egui::pos2(text_rect.right(), rect.center().y),
            egui::Align2::RIGHT_CENTER,
            &label,
            font,
            color,
        );

        if response.clicked() {
            self.account_menu = !self.account_menu;
        }
        if self.account_menu {
            let anchor = egui::pos2(rect.right() - 20.0, rect.bottom() + 4.0);
            self.account_popup(ui, anchor);
        }
    }

    /// 账号菜单。收起时完全不占位置，只有点了账号名才出现。
    fn account_popup(&mut self, ui: &mut egui::Ui, anchor: egui::Pos2) {
        let logged_in = self.snap.account.is_some();
        let mut close = false;

        egui::Area::new(ui.id().with("account-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor - egui::vec2(160.0, 0.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(theme::CARD)
                    .stroke(egui::Stroke::new(1.0, theme::LINE))
                    .corner_radius(10)
                    .inner_margin(egui::Margin::symmetric(6, 6))
                    .show(ui, |ui| {
                        ui.set_width(148.0);
                        if theme::ghost_button(ui, "重新登录").clicked() {
                            open_login();
                            self.note = Some("已打开登录窗口（服务需重启才读新令牌）".to_owned());
                            close = true;
                        }
                        ui.add_space(4.0);
                        if theme::danger_button(ui, "退出登录").clicked() && logged_in {
                            self.logout();
                            close = true;
                        }
                    });
            });

        if close {
            self.account_menu = false;
        }
    }

    /// 主界面。只回答一个问题：现在通不通。
    fn main_page(&mut self, ui: &mut egui::Ui) {
        const GAP: f32 = 12.0;
        let inner = ((ui.available_width() - GAP) / 2.0 - theme::PAD * 2.0).max(200.0);
        ui.horizontal(|ui| {
            self.cloud_card(ui, inner);
            self.wechat_card(ui, inner);
        });
        ui.add_space(12.0);
        self.service_row(ui);
        ui.add_space(18.0);
        self.jobs_section(ui);
    }

    /// 本地服务压成一行：状态、地址、启停按钮横着排，不单独占一张卡片的高度。
    fn service_row(&mut self, ui: &mut egui::Ui) {
        let (color, status) = if self.snap.service_up {
            (theme::OK, "运行中")
        } else {
            (theme::BAD, "已停止")
        };
        let width = ui.available_width() - theme::PAD * 2.0;
        theme::card(ui, width, |ui| {
            ui.horizontal(|ui| {
                theme::pill(ui, color, status);
                ui.label(theme::muted("本地服务"));
                ui.label(egui::RichText::new(BIND).size(13.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.snap.service_up {
                        if theme::danger_button(ui, "停止服务").clicked() {
                            stop_service();
                            // 手停就是手停，别再自动把它拉回来。
                            self.auto_service = false;
                            self.note = Some("已停止服务（不再自动拉起）".to_owned());
                            self.refresh();
                        }
                    } else if theme::primary_button(ui, "启动服务").clicked() {
                        self.auto_service = true;
                        match service_command().spawn() {
                            Ok(_) => self.note = Some("正在启动服务…".to_owned()),
                            Err(error) => self.note = Some(format!("启动失败：{error}")),
                        }
                        self.refresh();
                    }
                });
            });
        });
    }

    /// 连接引导页：还没和云端配对过时才出现。
    ///
    /// 独立于主界面，不显示侧边栏 —— 这会儿"主界面/配置"都还没有意义，
    /// 连不上什么都是空的。
    fn connect_page(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(46.0);
            ui.label(egui::RichText::new("连接云端").size(18.0).strong());
        });
        ui.add_space(26.0);

        let busy = self.connect.step.is_some();
        let width = (ui.available_width() - theme::PAD * 2.0).min(420.0);
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            theme::card(ui, width, |ui| {
                ui.label(theme::muted("云端地址"));
                ui.add_space(5.0);
                let mut base = cloud_base();
                ui.add_enabled(
                    false,
                    egui::TextEdit::singleline(&mut base).desired_width(f32::INFINITY),
                );

                ui.add_space(15.0);
                ui.label(theme::muted("账号"));
                ui.add_space(5.0);
                ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.connect.username)
                        .hint_text("网页端用的同一个账号")
                        .desired_width(f32::INFINITY),
                );

                ui.add_space(15.0);
                ui.label(theme::muted("密码"));
                ui.add_space(5.0);
                let _ = ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.connect.password)
                        .password(true)
                        .desired_width(f32::INFINITY),
                );

                ui.add_space(18.0);
                if busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(theme::muted(
                            self.connect.step.as_deref().unwrap_or("正在连接…"),
                        ));
                    });
                } else if theme::primary_button(ui, "连接").clicked() {
                    let ctx = ui.ctx().clone();
                    self.start_connect(&ctx);
                }

                if let Some(error) = &self.connect.error {
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new(error).size(12.0).color(theme::BAD));
                }
            });
        });
    }

    /// 配置页。只干一件事：改与云端的连接地址。
    ///
    /// 设备 ID 是只读展示 —— 它决定云端认不认这台机器，改了就得重新配对，
    /// 所以只给"复制"，不给编辑。
    fn config_page(&mut self, ui: &mut egui::Ui) {
        ui.label(theme::section("云端连接"));
        ui.add_space(10.0);

        let cloud = self.snap.cloud.clone();
        let width = ui.available_width() - theme::PAD * 2.0;
        let mut save = false;
        let mut cancel = false;
        let mut start_edit: Option<String> = None;

        theme::card(ui, width, |ui| {
            ui.label(theme::muted("服务器地址"));
            ui.add_space(6.0);
            match &mut self.editing {
                Some(draft) => {
                    ui.add(egui::TextEdit::singleline(draft).desired_width(f32::INFINITY));
                    ui.add_space(5.0);
                    ui.label(theme::faint("以 http(s) 开头，结尾不带斜杠"));
                    ui.add_space(12.0);
                    theme::divider(ui);
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::primary_button(ui, "保存").clicked() {
                            save = true;
                        }
                        if theme::ghost_button(ui, "取消").clicked() {
                            cancel = true;
                        }
                    });
                }
                None => {
                    let current = cloud
                        .as_ref()
                        .map(|(url, _)| url.clone())
                        .unwrap_or_else(|| "未设置".to_owned());
                    ui.add(egui::Label::new(theme::value(&current)).truncate());
                    ui.add_space(12.0);
                    theme::divider(ui);
                    ui.add_space(12.0);
                    if theme::ghost_button(ui, "修改地址").clicked() {
                        start_edit = Some(if current == "未设置" {
                            "https://".to_owned()
                        } else {
                            current.clone()
                        });
                    }
                }
            }
        });

        ui.add_space(12.0);
        ui.label(theme::section("设备"));
        ui.add_space(10.0);
        let device = cloud.as_ref().map(|(_, d)| d.clone()).unwrap_or_default();
        theme::card(ui, width, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(theme::muted("设备 ID"));
                    ui.add_space(4.0);
                    if device.is_empty() {
                        ui.label(theme::value("尚未配对"));
                    } else {
                        ui.label(egui::RichText::new(&device).size(13.0));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !device.is_empty() && theme::ghost_button(ui, "复制").clicked() {
                        ui.ctx().copy_text(device.clone());
                        self.note = Some("设备 ID 已复制".to_owned());
                    }
                });
            });
        });

        if let Some(draft) = start_edit {
            self.editing = Some(draft);
        }
        if save {
            self.save_cloud();
        }
        if cancel {
            self.editing = None;
        }
    }

    /// 最近采集列表。文件名占主位、时间与体积靠右对齐，中间留空不做装饰线 ——
    /// 记录多的时候线会变成噪声。
    fn jobs_section(&mut self, ui: &mut egui::Ui) {
        let count = self.snap.jobs.len();
        ui.horizontal(|ui| {
            ui.label(theme::section("最近采集"));
            if count > 0 {
                ui.label(theme::faint(&format!("共 {count} 条")));
            }
        });
        ui.add_space(6.0);
        theme::card(ui, ui.available_width() - theme::PAD * 2.0, |ui| {
            if self.snap.jobs.is_empty() {
                ui.label(theme::muted("还没有采集记录"));
                ui.label(theme::faint("在云端网页发起采集后，这里会出现归档"));
            } else {
                for job in self.snap.jobs.clone() {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&job.name).size(13.0)).truncate(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(theme::muted(&human_size(job.size)));
                            ui.add_space(4.0);
                            ui.label(theme::faint(&ago(job.modified)));
                        });
                    });
                }
            }
        });
    }
}

impl Console {
    fn wechat_card(&mut self, ui: &mut egui::Ui, width: f32) {
        // 只认「**当前登录账号**在本机绑定的那个微信号」。
        // 本机 accounts 目录里可能躺着别的账号采集过的密钥，拿第一个顶上会让客户
        // 以为已经绑好了（实测踩过：登录 liang，卡片却显示 test2 的微信号）。
        let bound = self.snap.bound_wxid.clone();
        // 「就绪」= 密钥与解密缓存都在；只有密钥时是「密钥不完整」——
        // 此时导出读不到聊天库，必须让客户看见，而不是写「就绪」。
        let bound_ready = bound
            .as_ref()
            .map(|wxid| self.snap.accounts_ready.iter().any(|item| item == wxid))
            .unwrap_or(false);
        let bound_has_credential = bound
            .as_ref()
            .map(|wxid| self.snap.accounts.iter().any(|item| item == wxid))
            .unwrap_or(false);
        // 云端没登记时，**不要**直接写"尚未绑定微信号" —— 那会让客户以为本机没有密钥，
        // 而实际上 accounts/ 里可能躺着能用的密钥（实测：本机导出能跑通 14 条消息，
        // 卡片却写"尚未绑定"）。如实分开说：本机有没有密钥是一回事，
        // 云端有没有把这个微信号登记到这台电脑是另一回事。
        let local_only = if bound.is_none() {
            self.snap.accounts_ready.first().cloned()
        } else {
            None
        };
        let (color, status) = match (bound.is_some(), bound_ready, bound_has_credential) {
            // 密钥在就是「已绑定」——判定依据是本机密钥（核对过），不是云端那条记录。
            (true, true, _) => (theme::OK, "已绑定"),
            (false, _, _) if local_only.is_some() => (theme::OK, "已绑定"),
            (true, false, true) => (theme::WARN, "密钥不完整"),
            (true, false, false) => (theme::WARN, "缺密钥"),
            (false, _, _) => (theme::IDLE, "未绑定"),
        };
        theme::card(ui, width, |ui| {
            theme::card_head_rev(ui, "密钥获取", color, status);
            ui.add_space(8.0);
            match bound.as_deref() {
                // 优先显示微信号（昵称）；没有导出数据时回落到 wxid（密钥目录名）
                Some(wxid) => {
                    let display = self
                        .snap
                        .bound_name
                        .clone()
                        .or_else(|| self.snap.wechat_name.clone())
                        .unwrap_or_else(|| wxid.to_owned());
                    ui.add(egui::Label::new(theme::value(&display)).truncate());
                    if bound_ready {
                        ui.label(theme::faint(wxid));
                    } else if bound_has_credential {
                        ui.label(theme::faint(&format!("已抓到 {wxid} 的密钥，但缺少解密缓存")));
                        ui.label(theme::faint("请在微信已登录时重新绑定以获取密钥"));
                    } else {
                        ui.label(theme::faint(&format!("尚未采集 {wxid} 的密钥")));
                    }
                    // 这里原本还有一行"本机另有 N 个其他账号的密钥，当前账号不可用"——
                    // 卡片上明明写着「密钥就绪」，它却读起来像"当前账号不能用"，客户直接
                    // 被绕晕。别的声音去掉：只显示当前绑定的那个号的状态。
                }
                None => {
                    match local_only.as_deref() {
                        // 本机有可用密钥、云端没登记：这是**两件事**，得分开说清楚。
                        Some(wxid) => {
                            let display = self
                                .snap
                                .wechat_name
                                .clone()
                                .unwrap_or_else(|| wxid.to_owned());
                            ui.add(egui::Label::new(theme::value(&display)).truncate());
                            ui.label(theme::faint(&format!("本机已抓到 {wxid} 的密钥，解密缓存也在")));
                            ui.label(theme::faint("云端还没把这个微信号登记到本机：在网页点「绑定微信账号」即可"));
                        }
                        None => {
                            ui.label(theme::value("尚未绑定微信号"));
                            ui.label(theme::faint("在云端网页「绑定微信」后回到这里"));
                        }
                    }
                }
            }
            ui.add_space(10.0);
            theme::divider(ui);
            ui.add_space(10.0);
            if theme::ghost_button(ui, "打开密钥目录").clicked() {
                let _ = Command::new("explorer")
                    .arg(install_dir().join("accounts"))
                    .spawn();
            }
        });
    }

    /// 云端连接：只读展示 + 重新探测。
    ///
    /// 改地址的入口在「配置」页 —— 主界面的职责是回答"现在通不通"，
    /// 把编辑框放在这里会让两种用途互相干扰。
    fn cloud_card(&mut self, ui: &mut egui::Ui, width: f32) {
        let (color, status) = match self.cloud_reachable() {
            Some(true) => (theme::OK, "已连接"),
            Some(false) => (theme::BAD, "不可达"),
            None => (theme::IDLE, "未配置"),
        };
        let cloud = self.snap.cloud.clone();
        let reachable = self.cloud_reachable();
        theme::card(ui, width, |ui| {
            theme::card_head_rev(ui, "云端连接", color, status);
            ui.add_space(8.0);
            match &cloud {
                Some((url, _)) => {
                    ui.add(egui::Label::new(theme::value(url)).truncate());
                    ui.label(theme::faint(match reachable {
                        Some(true) => "连接正常",
                        Some(false) => "连不上，检查「配置」里的地址",
                        None => "正在探测…",
                    }));
                }
                None => {
                    ui.label(theme::value("未配置"));
                    ui.label(theme::faint("到「配置」页填入云端地址"));
                }
            }
            ui.add_space(10.0);
            theme::divider(ui);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if cloud.is_some() && theme::ghost_button(ui, "重新连接").clicked() {
                    // 不等后台那 15 秒的轮询，立刻再探一次
                    spawn_probe(self.reachable.clone());
                    self.note = Some("正在重新探测云端…".to_owned());
                }
                if theme::ghost_button(ui, "去配置").clicked() {
                    self.page = Page::Config;
                }
            });
        });
    }
}
