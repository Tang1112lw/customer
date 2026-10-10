//! AICRM 一键安装（图形界面）。
//!
//! 为什么要做成图形安装程序：每台设备都要和云端配对才能被识别，而配对需要一个一次性
//! 配对码。让客户去开 PowerShell、拼三个参数、还要在 10 分钟内用完 —— 那不是安装体验。
//! 这里把它收敛成一个窗口：
//!
//! ```text
//! 填云端地址 + 账号 + 密码 → 点「一键安装」
//!   → 登录云端 → 自动为本机签发配对码 → 配对 → 调用 install.ps1 装盘并注册自启
//! ```
//!
//! 配对码由本程序自动申领，用户全程不需要看到它。
//!
//! 用法：
//! ```text
//! aicrm-setup.exe [--endpoint https://云端地址]
//! ```

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// 共享模块里存在本窗口不引用的项是正常的：登录 / 配对那条路只在主控台里用，
// 这里只用 transport_warning 提示"地址是 http 明文"。
#[allow(dead_code)]
mod cloud;
mod single;
// 共用外观模块：本窗口只用到其中一部分（卡片 / 按钮 / 状态胶囊），
// 侧边栏那套是主窗口在用；共享模块里存在本窗口不引用的项是正常的。
#[allow(dead_code)]
mod theme;

#[path = "../shared/mod.rs"]
mod shared;

#[path = "../shared/font.rs"]
mod font;

use eframe::egui;
use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, TryRecvError},
};

/// 默认云端地址。打包时按实际部署改这里，用户就不用填了。
const DEFAULT_ENDPOINT: &str = "http://162.14.83.205";

fn main() -> eframe::Result {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    // 自检用：只把内嵌载荷解出来，不安装。
    if let Some(position) = arguments.iter().position(|item| item == "--extract-only") {
        let target = arguments
            .get(position + 1)
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("aicrm-payload"));
        match unpack_payload_to(&target) {
            Ok(count) => println!("已解出 {count} 个文件到 {}", target.display()),
            Err(message) => {
                eprintln!("解包失败：{message}");
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }

    // 与主界面共用一把锁：安装器和客户端不许并排出现。已经有一个界面在跑就
    // 把它叫到前台，自己退出。
    if !single::acquire() {
        single::activate_existing();
        return Ok(());
    }

    let endpoint = arguments
        .windows(2)
        .find(|pair| pair[0] == "--endpoint")
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // 重做后表单放在卡片里、状态区也加宽了，420×300 会把内容挤出去
            .with_inner_size([520.0, 546.0])
            .with_resizable(false)
            .with_title("AICRM 安装"),
        ..Default::default()
    };
    eframe::run_native(
        "AICRM 安装",
        options,
        Box::new(move |cc| {
            font::install_cjk_font(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(SetupApp::new(endpoint)))
        }),
    )
}

/// 内嵌载荷的页脚：`[offset u64][len u64]["AICRMSFX"]`，贴在 exe 末尾 24 字节。
/// 由 `aicrm-pack` 拼包时写入；直接 `cargo build` 出来的本体没有它。
const PAYLOAD_MAGIC: &[u8; 8] = b"AICRMSFX";

/// 从自身末尾切出载荷（一个 zip：安装脚本 + 三个程序 + tools\）。
///
/// 只读页脚再按偏移取一段，不把整包读进内存 —— 成品有几十兆。
fn payload_zip() -> Result<zip::ZipArchive<std::io::Take<std::fs::File>>, String> {
    const MISSING: &str = "这不是完整的安装程序：请使用打包后的单文件版本（aicrm-pack 产物）";

    let exe = std::env::current_exe().map_err(|error| format!("取不到自身路径：{error}"))?;
    let mut file = std::fs::File::open(&exe).map_err(|error| format!("打开自身失败：{error}"))?;
    let size = file
        .metadata()
        .map_err(|error| format!("读取自身属性失败：{error}"))?
        .len();
    if size < 24 {
        return Err(MISSING.to_owned());
    }

    file.seek(SeekFrom::End(-24))
        .map_err(|error| format!("定位页脚失败：{error}"))?;
    let mut footer = [0u8; 24];
    file.read_exact(&mut footer)
        .map_err(|error| format!("读取页脚失败：{error}"))?;
    if &footer[16..] != PAYLOAD_MAGIC {
        return Err(MISSING.to_owned());
    }

    let offset = u64::from_le_bytes(footer[0..8].try_into().expect("页脚固定 8 字节"));
    let length = u64::from_le_bytes(footer[8..16].try_into().expect("页脚固定 8 字节"));
    if offset.saturating_add(length).saturating_add(24) > size {
        return Err("安装包载荷越界，文件可能被截断".to_owned());
    }

    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("定位载荷失败：{error}"))?;
    zip::ZipArchive::new(file.take(length)).map_err(|error| format!("载荷不是合法的 zip：{error}"))
}

/// 把载荷解到指定目录（已存在则先清空）。返回解出的文件数。
fn unpack_payload_to(dir: &Path) -> Result<usize, String> {
    let mut archive = payload_zip()?;
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|error| format!("创建 {} 失败：{error}", dir.display()))?;

    let mut count = 0usize;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("读取载荷第 {index} 项失败：{error}"))?;
        // enclosed_name 会挡掉 `..` 之类的路径穿越
        let Some(relative) = entry.enclosed_name() else {
            return Err("载荷里含有非法路径".to_owned());
        };
        let out = dir.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)
                .map_err(|error| format!("创建 {} 失败：{error}", out.display()))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建 {} 失败：{error}", parent.display()))?;
        }
        let mut target =
            std::fs::File::create(&out).map_err(|error| format!("写 {} 失败：{error}", out.display()))?;
        std::io::copy(&mut entry, &mut target)
            .map_err(|error| format!("写 {} 失败：{error}", out.display()))?;
        count += 1;
    }
    if count == 0 {
        return Err("载荷是空的".to_owned());
    }
    Ok(count)
}

/// 解到本次安装专属的临时目录，装完即删。
fn unpack_payload() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("aicrm-install-{}", std::process::id()));
    unpack_payload_to(&dir)?;
    Ok(dir)
}

/// 装完把自己删掉。
///
/// 正在运行的 exe 删不掉自己，交给一个延迟执行的命令：等本进程退出后再删。
/// 用 ping 当延时 —— 不依赖控制台，也不会像 timeout 那样在被重定向时报错。
fn remove_self_after_exit() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let script = format!("ping -n 4 127.0.0.1 >nul & del /f /q \"{}\"", exe.display());
    let mut command = Command::new("cmd.exe");
    command
        .args(["/c", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = command.spawn();
}

/// 调用载荷里的 install.ps1 完成落盘、注册自启与启动服务。
///
/// 复用安装脚本而不是在 Rust 里重写一遍：拷贝文件、icacls 收紧权限、写 HKCU Run
/// 这些逻辑已经在那边验证过了，重复实现只会多一处会走样的地方。
///
/// 安装是**静默**的：不问账号密码，设备身份留给客户端首次「连接云端」时
/// 由主控台申领并签发（install.ps1 会先落一个占位 id）。
fn run_installer(base_url: &str, work: &Path) -> Result<String, String> {
    let script = work.join("install.ps1");
    if !script.is_file() {
        return Err("安装载荷里缺少 install.ps1".to_owned());
    }
    let base = base_url.trim_end_matches('/');

    let mut command = Command::new("powershell");
    command
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-CloudUrl")
        .arg(base)
        // 网页来源白名单：云端网页和接口是同一个基地址，所以直接用它。
        // 不带这一项的话，装完网页端拿不到本机令牌，「数据采集」按钮点不动。
        .arg("-AllowOrigin")
        .arg(base);

    let output = command
        .output()
        .map_err(|error| format!("无法运行安装脚本：{error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "安装脚本失败：{}",
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        ));
    }
    Ok(stdout.trim().to_owned())
}

/// 打开客户端。
///
/// 两种情况必须分开处理：
/// - **已经有一个在跑** → 直接再 spawn 一个会被它的单实例检测顶掉（新进程自查到
///   已有实例就立刻退出），用户看到的现象是"点了没反应"。所以先找窗口叫到前台。
/// - **没在跑** → 启动它，并把失败原因返回出去而不是吞掉。
fn open_console() -> Result<String, String> {
    const TITLE: &str = "AICRM";
    let wide: Vec<u16> = TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 传入的是以 NUL 结尾的宽字符串。窗口类传空表示只按标题查找。
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
        return Ok("客户端已在运行，已切到前台".to_owned());
    }

    let exe = shared::paths::install_dir().join("aicrm-console.exe");
    if !exe.is_file() {
        return Err(format!("找不到客户端程序：{}", exe.display()));
    }
    Command::new(&exe)
        .spawn()
        .map_err(|error| format!("启动客户端失败：{error}"))?;
    Ok("已启动客户端".to_owned())
}

enum State {
    Idle,
    Working(String),
    Done(String),
    Failed(String),
}

struct SetupApp {
    /// 打包时预填的云端地址。不再由用户输入 —— 客户装个软件不该被问这个。
    endpoint: String,
    state: State,
    pending: Option<Receiver<Result<String, String>>>,
    /// 「打开客户端」的结果，失败时直接说清楚原因，别让用户猜。
    hint: Option<String>,
}

impl SetupApp {
    fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            state: State::Idle,
            pending: None,
            hint: None,
        }
    }

    /// 安装过程中要锁住输入，避免用户改参数再点一次。
    fn busy(&self) -> bool {
        matches!(self.state, State::Working(_))
    }

    /// 登录地址与 API 基地址的关系：用户填的是登录接口，其余接口都在同一个源下。
    fn base_url(&self) -> String {
        let trimmed = self.endpoint.trim().trim_end_matches('/');
        trimmed
            .strip_suffix("/api/auth/login")
            .unwrap_or(trimmed)
            .trim_end_matches('/')
            .to_owned()
    }

    /// 静默安装：不问账号密码。
    ///
    /// 装的是什么（放文件 + 注册自启）和"我是谁"（设备配对）是两件事。以前揉在
    /// 一个表单里，客户装个软件还得先要账号密码 —— 现在只做前者，配对交给客户端
    /// 首次打开时的「连接云端」引导页。
    fn start(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let base = self.base_url();
        if base.is_empty() {
            self.state = State::Failed("打包时未填入云端地址".to_owned());
            return;
        }

        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let outcome = (|| -> Result<String, String> {
                // 取出随包的程序与脚本：优先用内嵌载荷（单文件安装程序），
                // 没有载荷就退回"和本 exe 同目录"的旧布局（开发机手工分发用）。
                let work = match unpack_payload() {
                    Ok(dir) => dir,
                    Err(_) => std::env::current_exe()
                        .map_err(|error| format!("取不到自身路径：{error}"))?
                        .parent()
                        .map(Path::to_path_buf)
                        .ok_or_else(|| "安装包路径异常".to_owned())?,
                };
                let log = run_installer(&base, &work)?;
                let _ = std::fs::remove_dir_all(&work);
                Ok(format!(
                    "{log}\n\n下一步：客户端打开后会请你连一次云端，\
                     连上就完成配对，以后不用再填。"
                ))
            })();
            let _ = sender.send(outcome);
            ctx.request_repaint();
        });

        self.pending = Some(receiver);
        self.state = State::Working("正在安装…".to_owned());
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(message)) => {
                self.pending = None;
                self.state = State::Done(message);
                // 装完直接把客户端拉起来：用户点完「安装」最期待看到的就是它，
                // 停在一个"安装成功"的静止页面上反而让人以为没生效。
                // 先把自己的单实例名额放掉 —— 不然客户端一启动就被这把锁挡回去。
                single::release();
                // 装完把安装包本身删掉 —— 客户桌面上只该有一个客户端图标，不该堆着
                // 安装程序，更不该出现"点开说找不到文件"的残留（微信也是这样）。
                remove_self_after_exit();
                match open_console() {
                    // 客户端已经出面了，安装窗口就没必要继续占着任务栏 ——
                    // 两个窗口并排摆着，用户会以为装了两遍。
                    Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(error) => self.hint = Some(error),
                }
            }
            Ok(Err(message)) => {
                self.pending = None;
                self.state = State::Failed(message);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                self.state = State::Failed("安装线程异常退出".to_owned());
            }
        }
    }

    fn status_ui(&self, ui: &mut egui::Ui) {
        match &self.state {
            State::Idle => {
                ui.label(theme::faint("填写完成后点「开始安装」"));
            }
            State::Working(step) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new(step).size(13.0));
                });
            }
            State::Done(message) => {
                theme::pill(ui, theme::OK, "安装成功");
                ui.add_space(10.0);
                ui.label(theme::muted(
                    "客户端已装好并注册了开机自启。点上面的「打开客户端」就能用。",
                ));
                ui.add_space(10.0);
                egui::ScrollArea::vertical()
                    .max_height(130.0)
                    .show(ui, |ui| {
                        ui.label(theme::faint(message));
                    });
            }
            State::Failed(message) => {
                theme::pill(ui, theme::BAD, "安装失败");
                ui.add_space(10.0);
                egui::ScrollArea::vertical()
                    .max_height(150.0)
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(message).size(12.0).color(theme::BAD));
                    });
            }
        }
    }
}

impl eframe::App for SetupApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 不问任何东西，打开就开始装。安装器已经没有需要用户填的内容了。
        if matches!(self.state, State::Idle) {
            self.start(ctx);
        }
        let before = std::mem::discriminant(&self.state);
        self.poll(ctx);
        // 与登录窗口同理：状态变了必须再要一帧，否则界面不刷新
        if matches!(self.state, State::Working(_)) || std::mem::discriminant(&self.state) != before
        {
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {

            ui.add_space(4.0);
            ui.label(egui::RichText::new("AICRM 安装").size(17.0).strong());
            ui.add_space(4.0);
            ui.label(theme::muted(
                "正在安装，完成后会自动打开客户端。不需要填任何东西。",
            ));
            ui.add_space(24.0);

            self.status_ui(ui);

            if let Some(hint) = &self.hint {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(hint).size(12.0).color(theme::BAD));
            }

            // 只有失败才给按钮。成功路径上不该出现需要用户点的东西 ——
            // 装完会自动拉起客户端并关掉自己。
            if matches!(self.state, State::Failed(_)) {
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    if theme::primary_button(ui, "重试").clicked() {
                        let ctx = ui.ctx().clone();
                        self.state = State::Idle;
                        self.start(&ctx);
                    }
                    if theme::ghost_button(ui, "关闭").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            }

            if let Some(warning) = cloud::transport_warning(&self.endpoint) {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(warning).size(12.0).color(theme::WARN));
            }
        });
    }
}
