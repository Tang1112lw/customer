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

#[path = "../shared/mod.rs"]
mod shared;

#[path = "../shared/font.rs"]
mod font;

use shared::{
    cache::decrypted_cache_ready,
    credentials,
    paths::{self, install_dir},
    process,
    self_name::self_name_from_exports,
};

use eframe::egui;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    os::windows::process::CommandExt,
    path::Path,
    process::Command,
    sync::mpsc,
    time::{Duration, Instant, SystemTime},
};

/// 不留控制台窗口地启动服务（与 `install.ps1` 的 `-WindowStyle Hidden` 等价）。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 状态自动刷新间隔：只读本地文件，开销可以忽略。
const REFRESH: Duration = Duration::from_secs(5);

fn main() {
    // 随包的软件 OpenGL（Mesa）就在安装目录里（应用本地装配，见 install.ps1）。
    // 这里指定 llvmpipe 后端：少了这一条，Mesa 会去找硬件驱动并当场失败；有了它，
    // 远程桌面会话 / 没装显卡驱动的机器（本机只给 OpenGL 1.1）也能正常打开窗口 ——
    // 界面框架要求 OpenGL 2.0+，硬件给不了，就用纯软件渲染顶上。
    // SAFETY: 这是进程里执行的第一段代码，还没有其它线程会并发读写环境变量。
    unsafe {
        std::env::set_var("GALLIUM_DRIVER", "llvmpipe");
        std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1");
    }
    let log = console_error_log();
    // 界面是 GUI：出错时没有控制台可看，静默退出就是"双击没反应"。
    // 先装兜底（panic 也算），再把启动失败报出来。
    shared::dialog::install_panic_reporter(log.clone(), "AICRM 客户端");
    if let Err(error) = run() {
        shared::dialog::report_failure(
            &log,
            "AICRM 客户端启动失败",
            &format!("{error}\n\n请把这条信息连同日志一起发给技术支持。"),
        );
    }
}

/// 界面程序自己的错误日志。放在安装目录里，客户发日志时只需一个位置。
fn console_error_log() -> std::path::PathBuf {
    install_dir().join("console.err.log")
}

fn run() -> eframe::Result {
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
        // 留痕：重复双击图标走这条路（正常行为，但出事时要能从日志看出来）。
        shared::dialog::write_failure(
            &console_error_log(),
            "AICRM 客户端未启动",
            "本机已有客户端在运行，已把它叫到前台",
        );
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // 560×480（2026-10-13 按用户要求改小）：登录卡片（账号/密码/登录）
            // 与主界面（侧栏 + 最近采集单列）都按"一屏一件事"布局，更宽的窗口
            // 只会让内容孤零零漂在中间。主界面撤下双列卡片后不再需要 780 的横向比例。
            .with_inner_size([560.0, 480.0])
            // 固定尺寸：布局在窄窗口下会把 CJK 文本压成竖排，
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
            font::install_cjk_font(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(Console::new(!hidden)))
        }),
    )
}

/// 启动后台服务的命令。**不带任何参数** —— 服务默认从自己所在目录读
/// tools/accounts/data/凭据文件，云端地址（cloud.url）与登录令牌也现读现用。
/// 参数拼装与"改一处漏一处"的自启项同步在这里整体消失。
fn service_command() -> Command {
    let mut command = Command::new(install_dir().join("aicrm_windows_api.exe"));
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn read_trimmed(path: &Path) -> String {
    std::fs::read_to_string(path)
        .map(|text| text.trim().to_owned())
        .unwrap_or_default()
}

/// 是否已经有另一个实例在跑（自己也在进程列表里，所以数到 2 才算重复）。
///
/// 没有用互斥量：托盘每次点击都会拉起一次程序，开出一串窗口是必然的事故，
/// 这个检查足够挡住，而且不引入额外的依赖。
/// 登录云端：换取会话令牌并写进本地。
///
/// **只有这一步，没有配对。** 2026-10-10 取消设备绑定后，客户端与云端的关系就是
/// 一句「这个账号在我这里登录着」：令牌落盘 → 服务带它心跳 → 同一账号的网页端
/// 立刻看得见这台客户端，并能绑微信、点采集。首次使用与「重新登录」走同一条路。
fn connect_task(base: &str, username: &str, password: &str) -> Result<String, String> {
    // 客户端实例标识随登录上送：同一个账号在多台电脑登录时，云端靠它分辨是谁，
    // 并把先登录的那台请下线（一个账号只能登录一个客户端）。
    let client_id = std::fs::read_to_string(paths::client_id())
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "console".to_owned());
    let session = cloud::login(
        &format!("{base}/api/auth/login"),
        username,
        password,
        &client_id,
    )
    .map_err(|error| error.message().to_owned())?;

    if let Some(name) = session.display_name.as_deref() {
        let _ = credentials::save_account(&paths::account_name(), name);
    }
    let warning = match credentials::save_token(&paths::api_token(), &session.access_token) {
        Ok(notice) => notice,
        Err(error) => return Err(format!("令牌写入失败：{error}")),
    };
    // 刷新令牌一并落盘：access token 只有 2 小时，服务靠它自动续期。
    if let Some(refresh) = session.refresh_token.as_deref() {
        let _ = credentials::save_token(&paths::api_refresh(), refresh);
    }

    let who = session
        .display_name
        .unwrap_or_else(|| username.to_owned());

    // 服务启动时读一次令牌，重启才换得掉。
    if service_up() {
        process::stop_service();
    }
    let mut message = format!("已登录：{who}。服务将用新令牌重启");
    if let Some(notice) = warning {
        message.push_str(&format!("（{notice}）"));
    }
    Ok(message)
}

/// 本机是否已经登录云端账号。
///
/// 判据是 `api.token` 是不是**还没过期**的云端会话 JWT（登录或续期写入）。
/// 安装时生成的本地随机令牌不算登录；过期的令牌也不算 —— 它连不上云端，
/// 只判"看起来像令牌"会让界面一直显示已登录、用户却什么都点不动。
fn logged_in() -> bool {
    std::fs::read_to_string(paths::api_token())
        .map(|text| shared::session::token_is_live(&text))
        .unwrap_or(false)
}

/// 云端基地址：装的时候写进 `cloud.url`，改服务器只改这个文件。
fn cloud_base() -> String {
    let url = read_trimmed(&paths::cloud_url());
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

// 云端可达性探针已随「云端连接」卡片一起撤下（2026-10-13）：主界面不再展示
// 云端通不通，探针失去了唯一消费者。云端地址仍在「配置」页（入口暂隐藏）
// 与安装目录的 cloud.url 里管理，服务每轮心跳重读它。

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

/// 顶栏放得下的显示名。
///
/// 昵称是用户自己在微信里起的，长度没有保证；顶栏可用宽约 450（560 减去
/// 侧栏 110），胶囊约 52，超长名字会把左侧标题顶出窗口。24 个字符
/// （≈312px）是留了余量的上限，真实昵称远够。
fn short_name(name: &str) -> String {
    let mut out: String = name.chars().take(24).collect();
    if name.chars().count() > 24 {
        out.push('…');
    }
    out
}

/// 一次状态快照。全部来自本地文件与一次端口探测，不依赖服务在线。
#[derive(Default)]
struct Snapshot {
    service_up: bool,
    accounts: Vec<String>,
    /// 真正可用的微信号：密钥本体与解密缓存（cache/）都在。
    /// 只有密钥没有 cache 时导出读不到库，必须区分出来。
    accounts_ready: Vec<String>,
    /// 微信号（昵称），从导出的 messages.json 里找；找不到回落显示 wxid
    wechat_name: Option<String>,
    /// 云端地址（`cloud.url` 的内容）。None = 还没配置。
    cloud: Option<String>,
    jobs: Vec<Job>,
    /// 已登录的账号名，来自 `account.name`（登录流程写入）。None = 未登录。
    account: Option<String>,
    /// 当前账号在本机绑定的微信号，由服务从云端拉取后缓存。None = 还没拉到。
    bound_wxid: Option<String>,
    /// 绑定微信号的昵称（云端记录），用于优先显示。
    bound_name: Option<String>,
    /// 正在跑的服务版本。界面右下角显示它 —— 见 [`snapshot`]。
    service_version: Option<String>,
}

/// 侧边栏的页面。
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Page {
    #[default]
    Main,
    /// 「配置」页：侧边栏入口已按用户要求隐藏（2026-10-13），页面内容本身
    /// 保留不变；放开侧边栏里注释掉的那两行即可恢复入口。
    #[allow(dead_code)]
    Config,
    /// 首次打开、还没登录过账号时的引导页（登录成功后就不再出现）。
    Connect,
}

/// 登录引导页的状态。登录是网络操作，必须放后台线程，否则界面会卡死。
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

fn snapshot() -> Snapshot {
    let dir = install_dir();
    let mut snap = Snapshot {
        service_up: service_up(),
        ..Default::default()
    };

    // 账号名由登录流程写进 account.name；文件不在就是没登录
    snap.account = std::fs::read_to_string(paths::account_name())
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

    let url = read_trimmed(&paths::cloud_url());
    if !url.is_empty() {
        snap.cloud = Some(url);
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
    /// 登录引导页的表单与后台任务。只在未登录时用得到。
    connect: Connect,
}

impl Console {
    fn new(visible: bool) -> Self {
        Self {
            snap: Snapshot::default(),
            refreshed: None,
            note: None,
            editing: None,
            tray: None,
            tray_failed: false,
            visible,
            // 默认托管：打开窗口就等于"服务该在跑"，掉了自动拉回来。
            auto_service: true,
            // 没登录过就先引导登录：登录令牌是与云端通信的唯一前提，
            // 没有它命令通道根本不会启动。
            page: if logged_in() { Page::Main } else { Page::Connect },
            account_menu: false,
            connect: Connect::default(),
        }
    }

    fn refresh(&mut self) {
        self.snap = snapshot();
        self.refreshed = Some(Instant::now());
        self.keep_service_alive();
    }

    /// 开始登录：后台用账号密码向云端换取会话令牌并落盘。
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
    /// 云端立刻把这个账号的客户端标成离线，网页端也就点不动"绑定微信"。以前只能靠用户
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

    /// 退出登录：丢掉云端会话，换一把新的本地接口令牌。
    ///
    /// 不是删掉令牌文件 —— 服务启动时要求令牌至少 32 位可见字符，空文件会让服务
    /// 起不来。换成本地随机令牌同时有两个效果：命令通道随即停用（未登录），
    /// 本机接口仍能继续用（令牌非空）。
    fn logout(&mut self) {
        let mut fresh = String::new();
        while fresh.len() < 64 {
            fresh.push_str(&uuid::Uuid::new_v4().simple().to_string());
        }
        fresh.truncate(64);
        // 顺手清掉账号名，右上角随即回到"未登录"
        let _ = std::fs::remove_file(paths::account_name());
        // 刷新令牌必须一起删：续期任务只看 `api.refresh`，留着它的话，
        // 30 分钟内服务会拿它换回一个有效令牌写进 `api.token` —— 退出登录会自己"登回来"。
        let _ = std::fs::remove_file(paths::api_refresh());
        match std::fs::write(paths::api_token(), fresh) {
            Ok(()) => {
                // 服务把令牌读在内存里，重启才换得掉
                if self.snap.service_up {
                    process::stop_service();
                }
                // 退回登录页：令牌已经换成本地随机值，命令通道不该再连云端。
                self.page = Page::Connect;
                self.connect.error = None;
                self.connect.password.clear();
                self.note = Some("已退出登录；启动服务后使用新令牌".to_owned());
            }
            Err(error) => self.note = Some(format!("写入失败：{error}")),
        }
        self.refresh();
    }

    /// 保存云端地址。
    ///
    /// 服务每轮心跳都重读 `cloud.url`，所以下一轮（≤20 秒）就用新地址 ——
    /// 不用重启服务、不需要动自启项（它只写 exe 路径，没有地址参数）、
    /// 也不需要重新登录：账号身份在登录令牌里，只要云端还是同一个库，
    /// 客户端换地址后照样被认出来。
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
        if let Err(error) = std::fs::write(paths::cloud_url(), &url) {
            self.note = Some(format!("写入失败：{error}"));
            self.editing = Some(draft);
            return;
        }
        self.note = Some("已保存：下一轮心跳即用新地址".to_owned());
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
                    // 恢复托管：keep_service_alive 只认 auto_service，不置 true
                    // 的话服务下次掉了不会再被拉回来。
                    self.auto_service = true;
                    match service_command().spawn() {
                        Ok(_) => self.note = Some("正在启动服务…".to_owned()),
                        Err(error) => self.note = Some(format!("启动失败：{error}")),
                    }
                    self.refresh();
                }
                Some(shell::Action::StopService) => {
                    // 手停就是手停：不置 false 的话，下一轮 5 秒刷新的
                    // keep_service_alive 会立刻把服务拉回来，停止等于没停
                    // （主界面「停止服务」按钮撤下后，托盘是唯一入口，
                    // 这个置位不能再省）。
                    self.auto_service = false;
                    process::stop_service();
                    self.note = Some("已停止服务（不再自动拉起）".to_owned());
                    self.refresh();
                }
                // 直接退出进程：走视口 Close 会再触发一次下面的关闭拦截
                Some(shell::Action::Quit) => {
                    // 托盘「退出」= 完全退出：先收掉本地服务等子进程，再退自己。
                    // 只关界面会把 aicrm_windows_api.exe 留在后台占着 8765。
                    process::stop_all_except("aicrm-console.exe");
                    std::process::exit(0);
                }
                None => {}
            }
        }

        // 点 × = 隐藏到托盘，不退出（真正的退出在托盘菜单里）。
        // 托盘建不出来时不能拦截 —— 那就真成了"关掉就找不回来"。
        //
        // 注意：这不是 bug —— 后台服务要继续跟云端通信（心跳 / 命令通道），
        // × 一退全退设备就离线了。要完全退出：右键通知区图标 →「退出」。
        // （曾考虑首次隐藏时弹气泡说明去向，tray-icon 0.26 不支持通知，
        // 裸调 Shell_NotifyIcon 不划算，先靠菜单文案与左键切换。）
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
    /// 「配置」入口已按用户要求隐藏（2026-10-13），现在只剩「主界面」一项。
    /// 栏宽保持 110 不动：窗口总宽是两列卡片时代定下的，收窄侧边栏只会让
    /// 内容区左右失衡，而一个入口不需要更窄的栏。
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
                    // 「配置」入口按用户要求隐藏（2026-10-13）：改云端地址暂时走
                    // 重装，或直接编辑安装目录下的 cloud.url（服务每轮心跳重读它，
                    // 不用重启）。页面代码原样保留，放开下面两行即可恢复入口。
                    // if theme::nav_item(ui, "配置", self.page == Page::Config) {
                    //     self.page = Page::Config;
                    // }

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

    /// 右上角的绑定状态：(颜色, 状态, 微信号)。
    ///
    /// 判定口径与已下线的「密钥获取」卡片一致：本机密钥与解密缓存都就绪才算
    /// 「已绑定」（云端没登记但本机有可用密钥时同样算）—— 网页端对缺密钥的
    /// 账号也按未绑定处理，这里如实同步。按用户要求（2026-10-13）只保留
    /// 「已绑定 / 未绑定」两态，中间态（密钥不完整等）一律显示为未绑定。
    fn binding(&self) -> (egui::Color32, &'static str, Option<String>) {
        let ready = |wxid: &String| self.snap.accounts_ready.iter().any(|item| item == wxid);
        if let Some(wxid) = self.snap.bound_wxid.as_ref().filter(|wxid| ready(wxid)) {
            let name = self
                .snap
                .bound_name
                .clone()
                .or_else(|| self.snap.wechat_name.clone())
                .unwrap_or_else(|| wxid.clone());
            return (theme::OK, "已绑定", Some(short_name(&name)));
        }
        // 云端没登记，但本机躺着能用的密钥：同样是已绑定，别让人白担心。
        if self.snap.bound_wxid.is_none()
            && let Some(wxid) = self.snap.accounts_ready.first()
        {
            // wechat_name 取自最近一次导出的 messages.json，Snapshot 里没有
            // 它和哪个 wxid 对应的记录：多个就绪账号时它很可能属于另一个账号，
            // 拿来配 accounts_ready.first() 会张冠李戴（旧卡片同病）。只有
            // 恰好一个就绪账号时才归得准；否则回落 wxid —— 密钥目录名至少
            // 不会指错人。
            let name = if self.snap.accounts_ready.len() == 1 {
                self.snap.wechat_name.clone().unwrap_or_else(|| wxid.clone())
            } else {
                wxid.clone()
            };
            return (theme::OK, "已绑定", Some(short_name(&name)));
        }
        (theme::IDLE, "未绑定", None)
    }

    /// 顶栏。右上角只放绑定状态：已绑定（带上微信号）/ 未绑定。
    ///
    /// 点它展开账号菜单（重新登录 / 退出登录）—— 菜单入口与状态同一个位置，
    /// 不再单独显示账号名：客户需要知道的是"绑没绑"，不是"谁登录了"。
    fn top_bar(&mut self, ui: &mut egui::Ui, width: f32) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 56.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::ZERO, theme::CARD);
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            egui::Stroke::new(1.0, theme::LINE),
        );

        let (status_color, status, name) = self.binding();
        // 先按中性色量尺寸（颜色不影响排版宽度），交互区定了再定色重排。
        let status_font = egui::FontId::proportional(12.0);
        let name_font = egui::FontId::proportional(13.0);
        let status_width = ui
            .painter()
            .layout_no_wrap(status.to_owned(), status_font.clone(), theme::TEXT)
            .size()
            .x;
        let name_width = name
            .as_ref()
            .map(|name| {
                ui.painter()
                    .layout_no_wrap(name.clone(), name_font.clone(), theme::TEXT)
                    .size().x
                    + 6.0
            })
            .unwrap_or(0.0);
        let pill_size = egui::vec2(status_width + 16.0, 21.0);

        let pill = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 20.0 - pill_size.x - name_width, rect.center().y - pill_size.y / 2.0),
            pill_size,
        );
        let response = ui.interact(
            egui::Rect::from_min_size(pill.min, egui::vec2(pill_size.x + name_width, pill_size.y))
                .expand2(egui::vec2(8.0, 7.0)),
            ui.id().with("account"),
            egui::Sense::click(),
        );
        let (status_ink, name_ink) = if response.hovered() {
            (theme::ACCENT, theme::ACCENT)
        } else {
            (status_color, theme::TEXT)
        };

        // 胶囊底：同色系淡底 + 圆角（已下线卡片的视觉语言，挪到顶栏）。
        ui.painter().rect_filled(
            pill,
            egui::CornerRadius::same(10),
            egui::Color32::from_rgba_unmultiplied(status_ink.r(), status_ink.g(), status_ink.b(), 26),
        );
        let status_galley = ui
            .painter()
            .layout_no_wrap(status.to_owned(), status_font, status_ink);
        let status_pos = pill.center() - status_galley.size() * 0.5;
        ui.painter().galley(status_pos, status_galley, status_ink);
        if let Some(name) = name {
            let name_galley = ui.painter().layout_no_wrap(name, name_font, name_ink);
            let name_pos =
                egui::pos2(pill.right() + 6.0, rect.center().y - name_galley.size().y * 0.5);
            ui.painter().galley(name_pos, name_galley, name_ink);
        }

        if response.clicked() {
            self.account_menu = !self.account_menu;
        }
        if self.account_menu {
            let anchor = egui::pos2(rect.right() - 20.0, rect.bottom() + 4.0);
            self.account_popup(ui, anchor);
        }
    }

    /// 账号菜单。收起时完全不占位置，只有点了右上角状态才出现。
    ///
    /// 按用户要求（2026-10-13）做小：只放两个按钮，收紧内边距与按钮 padding。
    fn account_popup(&mut self, ui: &mut egui::Ui, anchor: egui::Pos2) {
        let logged_in = self.snap.account.is_some();
        let mut close = false;

        egui::Area::new(ui.id().with("account-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor - egui::vec2(122.0, 0.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(theme::CARD)
                    .stroke(egui::Stroke::new(1.0, theme::LINE))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::symmetric(5, 5))
                    .show(ui, |ui| {
                        ui.set_width(112.0);
                        ui.spacing_mut().button_padding = egui::vec2(8.0, 3.0);
                        if theme::ghost_button(ui, "重新登录").clicked() {
                            // 登录在主控台内的连接页完成：登录成功后令牌落盘，
                            // 服务自动用新令牌重启，不需要再开第二个窗口。
                            self.connect.error = None;
                            self.connect.step = None;
                            self.connect.pending = None;
                            self.connect.password.clear();
                            self.page = Page::Connect;
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

    /// 主界面。只保留「最近采集」（2026-10-13 按用户要求精简）：
    /// 云端连接卡片、密钥卡片、本地服务卡片全部撤下 —— 绑定状态移到右上角，
    /// 服务由托盘与后台自动托管（掉了自动拉起），不再需要客户盯着。
    fn main_page(&mut self, ui: &mut egui::Ui) {
        self.jobs_section(ui);
    }

    /// 登录引导页：还没登录过账号时才出现。
    ///
    /// 独立于主界面，不显示侧边栏 —— 这会儿"主界面/配置"都还没有意义，
    /// 连不上什么都是空的。
    ///
    /// 玻璃质感 + 左侧浅绿玻璃品牌栏（2026-10-13 用户选定方案）：只保留
    /// 账号、密码与登录按钮 —— 云端地址归「配置」页管理，客户不需要知道它；
    /// 提示文字与明文警告一并去掉，这一屏只让人做一件事：登录。
    fn connect_page(&mut self, ui: &mut egui::Ui) {
        // 玻璃底：盖住外层 Frame 的 CONTENT 填充
        ui.painter()
            .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, theme::GLASS_PAGE);
        let height = ui.available_height();
        let busy = self.connect.step.is_some();

        ui.horizontal_top(|ui| {
            // 左：浅绿玻璃品牌栏（整幅纯色，见 theme::glass_side）
            theme::glass_side(ui, 200.0, height, |ui| {
                ui.label(
                    egui::RichText::new("AICRM")
                        .size(17.0)
                        .strong()
                        .color(theme::GLASS_GREEN_INK),
                );
            });

            // 右：玻璃表单卡
            ui.vertical(|ui| {
                ui.set_width(ui.available_width().max(1.0));
                ui.add_space(52.0);
                let pad = 30.0;
                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    let width = (ui.available_width() - pad).max(200.0);
                    theme::glass_card(ui, width, |ui| {
                        ui.label(theme::muted("账号"));
                        ui.add_space(5.0);
                        let _ = theme::glass_input(
                            ui,
                            &mut self.connect.username,
                            false,
                            !busy,
                        );

                        ui.add_space(15.0);
                        ui.label(theme::muted("密码"));
                        ui.add_space(5.0);
                        let _ = theme::glass_input(
                            ui,
                            &mut self.connect.password,
                            true,
                            !busy,
                        );

                        ui.add_space(18.0);
                        if busy {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(theme::muted(
                                    self.connect.step.as_deref().unwrap_or("正在连接…"),
                                ));
                            });
                        } else {
                            let button = egui::Button::new(
                                egui::RichText::new("登 录")
                                    .size(13.0)
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(theme::ACCENT)
                            .corner_radius(8)
                            .min_size(egui::vec2(ui.available_width(), 36.0));
                            if ui.add(button).clicked() {
                                let ctx = ui.ctx().clone();
                                self.start_connect(&ctx);
                            }
                        }

                        if let Some(error) = &self.connect.error {
                            ui.add_space(12.0);
                            ui.label(egui::RichText::new(error).size(12.0).color(theme::BAD));
                        }
                    });
                });
            });
        });
    }

    /// 配置页。只干一件事：改与云端的连接地址。
    ///
    /// 账号与客户端标识都是只读展示：账号决定云端认不认这台客户端，标识只用于
    /// 云端分辨同一账号的多台电脑，两者都不需要用户编辑。
    ///
    /// 侧边栏入口已按用户要求隐藏（2026-10-13），页面内容**原样保留**：
    /// 放开侧边栏里注释掉的那两行即可恢复。
    #[allow(dead_code)]
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
                    let current = cloud.clone().unwrap_or_else(|| "未设置".to_owned());
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
        ui.label(theme::section("账号"));
        ui.add_space(10.0);
        let account = self
            .snap
            .account
            .clone()
            .unwrap_or_else(|| "未登录".to_owned());
        let instance = read_trimmed(&paths::client_id());
        let mut copied = false;
        theme::card(ui, width, |ui| {
            ui.label(theme::muted("当前登录账号"));
            ui.add_space(4.0);
            ui.add(egui::Label::new(theme::value(&account)).truncate());
            ui.add_space(12.0);
            theme::divider(ui);
            ui.add_space(12.0);
            ui.label(theme::muted("本机客户端标识"));
            ui.add_space(4.0);
            if instance.is_empty() {
                ui.label(theme::faint("尚未生成（服务首次运行后出现）"));
            } else {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&instance).size(12.0)).truncate(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::ghost_button(ui, "复制").clicked() {
                            ui.ctx().copy_text(instance.clone());
                            copied = true;
                        }
                    });
                });
                ui.label(theme::faint("仅用于云端区分同一账号的多台电脑，排查问题时提供"));
            }
        });
        if copied {
            self.note = Some("客户端标识已复制".to_owned());
        }

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
