//! AICRM 一键安装（图形界面）。
//!
//! 为什么要做成图形安装程序：装客户端要在客户机上放文件、注册开机自启、写云端地址，
//! 还要跑 PowerShell、拼一串参数 —— 那不是安装体验。这里把它收敛成一个窗口：
//!
//! ```text
//! 点「一键安装」
//!   → 调用 install.ps1 装盘、注册自启、写入云端地址
//! ```
//!
//! 账号与密码一概不问：登录由客户端首次打开时的「连接云端」页完成（2026-10-10 起
//! 没有设备配对，登录即完成接入）。
//!
//! 用法：
//! ```text
//! aicrm-setup.exe [--endpoint https://云端地址] [--silent] [--check]
//! ```
//!
//! `--silent` 是不创建窗口的安装路径：显卡只给 OpenGL 1.1 的机器（驱动没装全，
//! 或通过「远程桌面」操作）窗口创建会失败，这时安装器**自动降级**到这条路上；
//! 实施人员也可以显式指定。见 [`install_without_window`]。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// 共享模块里存在本窗口不引用的项是正常的：登录那条路只在主控台里用，
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

fn main() {
    let log = setup_error_log();
    // 安装器是 GUI：出错时没有控制台可看，静默退出就是"双击没反应"。
    // 先装兜底（panic 也算），再把启动失败报出来。
    shared::dialog::install_panic_reporter(log.clone(), "AICRM 安装程序");
    if let Err(error) = run() {
        shared::dialog::report_failure(
            &log,
            "AICRM 安装程序启动失败",
            &format!(
                "{error}\n\n程序没能创建安装窗口。请把这条信息连同日志一起发给技术支持。"
            ),
        );
    }
}

/// 安装器自己的错误日志（与客户端同一个目录，客户发日志时只需一个位置）。
fn setup_error_log() -> PathBuf {
    shared::paths::install_dir().join("install.err.log")
}

fn run() -> eframe::Result {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    // 打包时预填的云端地址；`--endpoint` 可覆盖（联调用）。
    // 先算出来：下面几条无界面路径都要用它。
    let endpoint = arguments
        .windows(2)
        .find(|pair| pair[0] == "--endpoint")
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());

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

    // 无界面环境检查：界面本身可能就是坏掉的那一个（这是实测最可能的"双击没反应"
    // 原因），所以「环境准备」必须有一条不依赖 GUI 的路径 —— 结论写日志 + 弹原生提示。
    if arguments.iter().any(|item| item == "--check") {
        run_check_only(&endpoint);
    }

    // 无界面安装：不创建窗口，直接解载荷 → 跑 install.ps1。
    // 给两类场景用：① 实施人员明知这台机器显卡不行（远程 / 无人值守）；
    // ② 窗口创建失败时的兜底（见下面 run_native 的分支）。
    let silent_requested = arguments.iter().any(|item| item == "--silent")
        || arguments.iter().any(|item| item == "--no-gui");
    if silent_requested {
        install_without_window(&endpoint, "命令行指定无界面安装");
    }

    // 与主界面共用一把锁：安装器和客户端不许并排出现。已经有一个界面在跑就
    // 把它叫到前台，自己退出。
    if !single::acquire() {
        single::activate_existing();
        // 必须**明说**，不能静默退出：装了客户端并让它常驻的机器上，双击安装程序
        // 走的就是这条路，以前看起来就是"双击没反应"（实测被客户投诉过）。
        shared::dialog::report_failure(
            &setup_error_log(),
            "AICRM 安装程序没有启动",
            "检测到本机已有 AICRM 正在运行，已为你把它切换到前台。\n\n\
             要安装或重装：请先从托盘菜单「退出」AICRM，再运行本安装程序。",
        );
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // 重做后表单放在卡片里、状态区也加宽了，420×300 会把内容挤出去
            .with_inner_size([520.0, 546.0])
            .with_resizable(false)
            .with_title("AICRM 安装"),
        ..Default::default()
    };
    // 降级路径还要用地址，而 endpoint 会被 move 进下面的闭包。
    let fallback_endpoint = endpoint.clone();
    match eframe::run_native(
        "AICRM 安装",
        options,
        Box::new(move |cc| {
            font::install_cjk_font(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(SetupApp::new(endpoint)))
        }),
    ) {
        Ok(()) => Ok(()),
        // 窗口创建失败不再直接放弃。客户机显卡只给 OpenGL 1.1 时，eframe 在这里报
        // "egui_glow requires opengl 2.0+"；可是**安装本身不需要窗口** ——
        // 解载荷、跑 install.ps1 就够了。让"能不能画窗口"决定"能不能安装"，
        // 结果是本来能装的机器被一句显卡提示挡在门外（实测：客户的 Win10 就是这样）。
        Err(error) => install_without_window(&fallback_endpoint, &error.to_string()),
    }
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
    remove_stale_payload_dirs(&dir);
    unpack_payload_to(&dir)?;
    Ok(dir)
}

/// 清掉以前残留的载荷目录。
///
/// 安装器中途被杀（或客户直接关机）会留下近百兆的临时文件 —— 客户机的临时目录
/// 不该替我们攒垃圾。只动自己命名的那一类目录，别人的东西一概不碰。
fn remove_stale_payload_dirs(current: &Path) {
    let Some(parent) = current.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("aicrm-install-") && entry.path() != current {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
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
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // 参数必须走 raw_arg，不能走 args：args 会给含空格的参数自动加外层引号、并把
    // 内部的 `"` 转义成 `\"`，而 cmd.exe 不认这种转义 —— del 拿到的是坏路径，
    // 删除静默失败。这段「装完把自己删掉」因此从来没生效过（实测：安装器跑完，
    // 安装包原封不动地还在原地）。raw_arg 原样传递，cmd 收到的正是上面那条 script。
    // 同一个 Command 上 raw_arg 与 args 不能混用（会 panic），所以分成两个互斥分支。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command
            .raw_arg(format!("/c {script}"))
            .creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    command.args(["/c", &script]);
    let _ = command.spawn();
}

/// 调用载荷里的 install.ps1 —— 装盘、注册自启、启动服务；`-CheckOnly` 时只做检查。
///
/// 复用安装脚本而不是在 Rust 里重写一遍：拷贝文件、icacls 收紧权限、写 HKCU Run、
/// 环境检查，这些逻辑已经在那边验证过了，重复实现只会多一处会走样的地方。
/// 返回 (输出, 是否成功)：检查模式退出码 1 表示"有必须项没过"，那是结论不是故障，
/// 所以由调用方决定怎么解读。
fn run_script(script: &Path, base_url: &str, check_only: bool) -> Result<(String, bool), String> {
    let base = base_url.trim_end_matches('/');
    let mut command = Command::new("powershell");
    command
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .arg("-CloudUrl")
        .arg(base)
        // 网页来源白名单：云端网页和接口是同一个基地址，所以直接用它。
        // 不带这一项的话，装完网页端拿不到本机令牌，「数据采集」按钮点不动。
        .arg("-AllowOrigin")
        .arg(base);
    if check_only {
        command.arg("-CheckOnly");
    }

    let output = command
        .output()
        .map_err(|error| format!("无法运行安装脚本：{error}"))?;

    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        text.push('\n');
        text.push_str(stderr.trim());
    }
    Ok((text, output.status.success()))
}

/// 环境准备：解出载荷 → 跑 `install.ps1 -CheckOnly`。返回（载荷目录，原始输出）。
///
/// 载荷目录要留着 —— 检查通过后紧接着就用它安装，没必要解第二遍。
fn check_environment(endpoint: &str) -> Result<(PathBuf, String), String> {
    let work = match unpack_payload() {
        Ok(dir) => dir,
        Err(_) => std::env::current_exe()
            .map_err(|error| format!("取不到自身路径：{error}"))?
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "安装包路径异常".to_owned())?,
    };
    let script = work.join("install.ps1");
    if !script.is_file() {
        return Err(
            "安装包里缺少 install.ps1 —— 请使用打包后的单文件安装程序（aicrm-pack 产物）"
                .to_owned(),
        );
    }
    let (output, _) = run_script(&script, endpoint, true)?;
    Ok((work, output))
}

/// `--check`：无界面环境检查。结论写日志 + 弹原生提示，退出码 0 通过 / 1 未通过 / 2 出错。
fn run_check_only(endpoint: &str) -> ! {
    let log = setup_error_log();
    let (work, output) = match check_environment(endpoint) {
        Ok(value) => value,
        Err(message) => {
            shared::dialog::report_failure(&log, "AICRM 环境检查没能完成", &message);
            std::process::exit(2);
        }
    };
    // 只做检查，解出来的载荷用完就删（不占客户机的临时目录）。
    let _ = std::fs::remove_dir_all(&work);
    let items = shared::preflight::parse(&output);
    if items.is_empty() {
        shared::dialog::report_failure(
            &log,
            "AICRM 环境检查没有返回结果",
            &format!("请把下面的内容发给技术支持：\n\n{}", output.trim()),
        );
        std::process::exit(2);
    }
    // 明细进日志，弹窗只放结论 —— 弹窗太长反而没人看。
    shared::dialog::write_failure(&log, "AICRM 环境检查明细", &shared::preflight::diagnostic(&items));
    let blocking = shared::preflight::blockers(&items);
    let summary = if blocking.is_empty() {
        shared::preflight::headline(&items)
    } else {
        let lines: Vec<String> = blocking
            .iter()
            .map(|item| format!("· {}：{}", item.title, item.detail))
            .collect();
        format!(
            "{}\n\n{}\n\n详细结果：{}",
            shared::preflight::headline(&items),
            lines.join("\n"),
            log.display()
        )
    };
    shared::dialog::report_failure(&log, "AICRM 环境检查", &summary);
    std::process::exit(if blocking.is_empty() { 0 } else { 1 });
}

/// 解出载荷 → 跑 `install.ps1`（真正安装，不带 `-CheckOnly`）。
///
/// 窗口路径与无界面路径共用这一份：解包、脚本定位、失败判定只有一处实现，
/// 不会出现"改了 GUI 那条、忘了无界面那条"。
///
/// `prepared` 是环境准备阶段已经解好的载荷目录（窗口路径复用它，少解一遍）；
/// 无界面路径没有这一步，传 None，现场解。
fn install_payload(base: &str, prepared: Option<PathBuf>) -> Result<String, String> {
    let work = match prepared {
        Some(dir) => dir,
        // 取出随包的程序与脚本：优先用内嵌载荷（单文件安装程序），
        // 没有载荷就退回"和本 exe 同目录"的旧布局（开发机手工分发用）。
        None => unpack_payload().or_else(|_| {
            std::env::current_exe()
                .map_err(|error| format!("取不到自身路径：{error}"))?
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| "安装包路径异常".to_owned())
        })?,
    };
    let script = work.join("install.ps1");
    if !script.is_file() {
        return Err(
            "安装包里缺少 install.ps1 —— 请使用打包后的单文件安装程序（aicrm-pack 产物）"
                .to_owned(),
        );
    }
    let (log, ok) = run_script(&script, base, false)?;
    if !ok {
        return Err(format!("安装脚本失败：\n{}", log.trim()));
    }
    let _ = std::fs::remove_dir_all(&work);
    Ok(log)
}

/// 无界面安装：不创建窗口，解出载荷 → 跑 `install.ps1` → 拉起客户端。
///
/// 为什么必须有这条路：安装器是 GUI，要求显卡提供 OpenGL 2.0+。客户机若只有
/// Windows 自带的 OpenGL 1.1（显卡驱动没装全，或通过「远程桌面」连接操作），
/// 窗口在**创建阶段**就失败 —— 那时 `install.ps1` 还没机会跑，客户看到的只有
/// "安装程序启动失败"。可这台机器本来是能装的：安装动作只是解载荷 + 跑脚本，
/// 一个窗口都不需要。
///
/// 反馈只用 Win32 原生提示框（`MessageBoxW`，不依赖 OpenGL）—— 这类机器上，
/// 它是唯一还弹得出来的东西。
///
/// 不返回：装完直接拉起客户端并退出进程（收尾与窗口路径一致）。
fn install_without_window(endpoint: &str, reason: &str) -> ! {
    let log = setup_error_log();
    let base = endpoint.trim().trim_end_matches('/').to_owned();

    // 走这条路不是"故障"，是这台机器的显卡能力 —— 记一条日志便于事后分辨
    // （客户把日志发回来时能看出这次是怎么装上的），但不弹错误框吓人。
    shared::dialog::write_failure(&log, "未创建安装窗口，改用无界面安装", reason);

    // `install.ps1` 结尾会把主控台拉起来。先把单实例名额放掉 —— 还握着这把锁的话，
    // 主控台一启动就被自己挡回去（表现就是"装完了但客户端弹不出来"）。
    // 命令行 `--silent` 进来时本进程没抢过锁，这里是一次空操作。
    single::release();

    let outcome = if base.is_empty() {
        Err("打包时未填入云端地址".to_owned())
    } else {
        install_payload(&base, None)
    };

    match outcome {
        Ok(_) => {
            // 与窗口路径同一套收尾：删掉安装包自己（客户桌面上只该有一个客户端图标），
            // 再把客户端叫出来。
            remove_self_after_exit();
            let note = open_console()
                .unwrap_or_else(|error| format!("\n客户端没能自动打开，请从桌面图标启动：{error}"));
            // "为什么界面起不来"必须归因准确：远程桌面和显卡驱动这两种情况，
            // 用户该做的事完全不同 —— 说错了等于把他往反方向指。
            let cause = if std::env::var("SESSIONNAME")
                .map(|name| name.to_ascii_uppercase().starts_with("RDP-"))
                .unwrap_or(false)
            {
                "\n本次是在「远程桌面」会话里运行的。远程桌面会把 OpenGL 强制降到 1.1，\
                 即使显卡驱动完好，界面也起不来。让使用者在「这台电脑上」双击运行一次\
                 安装程序，界面就能正常显示。"
            } else {
                "\n通常是因为显卡驱动没装好、或版本过旧。补装一遍显卡驱动（或让 Windows \
                 Update 打上），客户端界面就能正常打开。"
            };
            shared::dialog::report_success(
                "AICRM 安装完成",
                &format!(
                    "客户端已装好并注册了开机自启。\n\n\
                     这次是无界面安装：安装程序的界面没能显示出来（它需要 OpenGL 2.0+），\
                     但安装结果与正常安装完全一致。{cause}\n\n\
                     界面打不开不影响使用：采集与上传由后台服务承担，网页端照常操作。{note}"
                ),
            );
            std::process::exit(0);
        }
        Err(message) => {
            shared::dialog::report_failure(
                &log,
                "AICRM 安装失败",
                &format!("{message}\n\n请把这条信息连同日志一起发给技术支持。"),
            );
            std::process::exit(1);
        }
    }
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
    /// 环境准备结果。None = 还没检查过；有结果才允许点「开始安装」。
    checks: Option<Vec<shared::preflight::CheckItem>>,
    /// 环境准备进行中（解载荷 + 跑检查，几秒到十几秒）。
    checking: bool,
    check_pending: Option<Receiver<Result<(PathBuf, Vec<shared::preflight::CheckItem>), String>>>,
    /// 已经解出来的载荷目录：检查通过后直接用这一份安装，不解第二遍。
    work: Option<PathBuf>,
    pending: Option<Receiver<Result<String, String>>>,
    /// 「打开客户端」的结果，失败时直接说清楚原因，别让用户猜。
    hint: Option<String>,
    /// 「复制诊断」的反馈。
    copied: bool,
}

impl SetupApp {
    fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            state: State::Idle,
            checks: None,
            checking: false,
            check_pending: None,
            work: None,
            pending: None,
            hint: None,
            copied: false,
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
    /// 装的是什么（放文件 + 注册自启）和"我是谁"（登录哪个账号）是两件事。以前揉在
    /// 一个表单里，客户装个软件还得先要账号密码 —— 现在只做前者，登录交给客户端
    /// 首次打开时的「连接云端」引导页。
    /// 环境准备：解载荷 + 跑检查。**先确认这台机器跑得起来，再让用户点安装** ——
    /// 装完才发现缺运行库 / 空间不够，比当场说清楚难查得多。
    fn begin_check(&mut self, ctx: &egui::Context) {
        if self.checking {
            return;
        }
        let endpoint = self.endpoint.clone();
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let outcome = check_environment(&endpoint).and_then(|(work, output)| {
                let items = shared::preflight::parse(&output);
                if items.is_empty() {
                    return Err(format!(
                        "环境检查没有返回结果，请把下面的内容发给技术支持：\n\n{}",
                        output.trim()
                    ));
                }
                Ok((work, items))
            });
            let _ = sender.send(outcome);
            ctx.request_repaint();
        });
        self.check_pending = Some(receiver);
        self.checking = true;
        self.copied = false;
    }

    fn poll_check(&mut self, ctx: &egui::Context) {
        let Some(receiver) = &self.check_pending else {
            return;
        };
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("环境检查线程异常退出".to_owned()),
        };
        self.check_pending = None;
        self.checking = false;
        match outcome {
            Ok((work, items)) => {
                // 明细一并落日志：客户机上的问题，事后只看这一份文件就能定位。
                shared::dialog::write_failure(
                    &setup_error_log(),
                    "AICRM 环境准备",
                    &shared::preflight::diagnostic(&items),
                );
                self.work = Some(work);
                self.checks = Some(items);
            }
            Err(message) => {
                self.state = State::Failed(message);
                ctx.request_repaint();
            }
        }
    }

    /// 必须项里没通过的 —— 非空就不放行安装。
    fn blocked(&self) -> Vec<shared::preflight::CheckItem> {
        self.checks
            .as_ref()
            .map(|items| {
                shared::preflight::blockers(items)
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 环境准备清单。放不下就滚动 —— 宁可滚动，也不要把中文字挤成竖排。
    fn environment_ui(&mut self, ui: &mut egui::Ui) {
        let Some(items) = self.checks.clone() else {
            return;
        };
        let blocking = self.blocked();

        ui.horizontal(|ui| {
            if blocking.is_empty() {
                theme::pill(ui, theme::OK, "检查通过");
            } else {
                theme::pill(ui, theme::BAD, "需要处理");
            }
            ui.label(theme::muted(&shared::preflight::headline(&items)));
        });
        ui.add_space(10.0);

        egui::ScrollArea::vertical()
            .max_height(226.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for item in &items {
                    let (color, label) = if item.passed {
                        (theme::OK, "通过")
                    } else if item.required {
                        (theme::BAD, "未通过")
                    } else {
                        (theme::WARN, "提醒")
                    };
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("●").size(11.0).color(color));
                        ui.label(egui::RichText::new(&item.title).size(13.0).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(label).size(12.0).color(color));
                        });
                    });
                    ui.add(egui::Label::new(theme::faint(&item.detail)).wrap());
                    ui.add_space(7.0);
                }
            });
        ui.add_space(12.0);

        if !blocking.is_empty() {
            ui.label(theme::muted(
                "先解决上面标「未通过」的必须项，再点「重新检测」。",
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if theme::primary_button(ui, "重新检测").clicked() {
                    let ctx = ui.ctx().clone();
                    self.checks = None;
                    self.begin_check(&ctx);
                }
                if theme::ghost_button(ui, "复制诊断").clicked() {
                    ui.ctx().copy_text(shared::preflight::diagnostic(&items));
                    self.copied = true;
                }
                if theme::ghost_button(ui, "关闭").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            if self.copied {
                ui.add_space(6.0);
                ui.label(theme::faint("诊断信息已复制，可以粘贴给技术支持。"));
            }
            if let Some(first) = blocking.first() {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!("{}：{}", first.title, first.detail))
                        .size(12.0)
                        .color(theme::BAD),
                );
            }
            return;
        }

        ui.horizontal(|ui| {
            if theme::primary_button(ui, "开始安装").clicked() {
                let ctx = ui.ctx().clone();
                self.start_install(&ctx);
            }
            if theme::ghost_button(ui, "重新检测").clicked() {
                let ctx = ui.ctx().clone();
                self.checks = None;
                self.begin_check(&ctx);
            }
        });
    }

    fn start_install(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let base = self.base_url();
        if base.is_empty() {
            self.state = State::Failed("打包时未填入云端地址".to_owned());
            return;
        }

        // 环境准备已经解过载荷，直接用那一份（没有就现解）。
        let prepared = self.work.clone();
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let outcome = (|| -> Result<String, String> {
                let log = install_payload(&base, prepared)?;
                Ok(format!(
                    "{log}\n\n下一步：客户端打开后会用云端账号登录一次，\
                     以后打开就直接进主界面。"
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
        // 打开先做环境准备：检查通过之前，安装按钮是禁着的。
        // 不问任何东西 —— 安装器已经没有需要用户填的内容了。
        if matches!(self.state, State::Idle) && self.checks.is_none() {
            self.begin_check(ctx);
        }
        self.poll_check(ctx);
        let before = std::mem::discriminant(&self.state);
        self.poll(ctx);
        // 与主控台同理：状态变了必须再要一帧，否则界面不刷新
        if self.checking
            || matches!(self.state, State::Working(_))
            || std::mem::discriminant(&self.state) != before
        {
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {

            ui.add_space(4.0);
            ui.label(egui::RichText::new("AICRM 安装").size(17.0).strong());
            ui.add_space(4.0);
            ui.label(theme::muted(match &self.state {
                State::Idle if self.checking => "正在准备运行环境…",
                State::Idle => "先确认运行环境没问题，再开始安装。",
                State::Working(_) => "正在安装，完成后会自动打开客户端。",
                State::Done(_) => "安装完成。",
                State::Failed(_) => "没能完成，原因写在下面。",
            }));
            ui.add_space(18.0);

            if self.checking && self.checks.is_none() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(
                            "正在检查：系统版本 / 磁盘空间 / 目录权限 / 运行库 / 云端连通性…",
                        )
                        .size(13.0),
                    );
                });
            } else if matches!(self.state, State::Idle) {
                self.environment_ui(ui);
            } else {
                self.status_ui(ui);
            }

            if let Some(hint) = &self.hint {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(hint).size(12.0).color(theme::BAD));
            }

            // 只有失败才给按钮。成功路径上不该出现需要用户点的东西 ——
            // 装完会自动拉起客户端并关掉自己。
            if matches!(self.state, State::Failed(_)) {
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if theme::primary_button(ui, "重新检测").clicked() {
                        let ctx = ui.ctx().clone();
                        self.state = State::Idle;
                        self.checks = None;
                        self.begin_check(&ctx);
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
