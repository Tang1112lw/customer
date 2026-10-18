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
//! aicrm-setup.exe [--endpoint https://云端地址] [--check]
//! ```
//!
//! 安装器**只走有界面一条路**：先直接开窗口；开不出来（本机只有 OpenGL 1.1：
//! 远程桌面会话强制降到 1.1、或显卡驱动没装全）才把随包的软件 OpenGL
//! （Mesa llvmpipe）和自身副本落到临时目录、从那里重启一次 —— 应用目录里
//! 有 Mesa，loader 才会优先用它而不是系统那份 OpenGL 1.1，于是任何机器都
//! 开得出窗口。窗口创建失败、且 Mesa 重启也没救回来就是明确故障：报错退出
//! 并写日志，不静默降级 —— 客户看不到安装过程比装不上更糟（2026-10-15
//! 用户明确要求：必须使用 OpenGL）。装完删掉的是**客户原本双击的那个安装包**；
//! 临时副本由父进程连 staging 目录一起收走。
//! `--check` 是给支持人员用的无界面环境自检，不安装任何东西。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// 共享模块里存在本窗口不引用的项是正常的：登录那条路只在主控台里用。
// （cloud.rs 曾在这里提示"地址是 http 明文"，2026-10-16 随地址填写一起删除。）
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
    // 先直接开窗口：有硬件 OpenGL 2.0+ 的机器（绝大多数）一步不多走 ——
    // 省掉一次上百兆的临时复制与重启。开不出来才带随包 Mesa 重启一次。
    //
    // run() 会返回 Err 只有一种情况：窗口没创建出来（eframe::run_native 失败）。
    // 安装过程里的失败都在界面内部展示，不会走到这里 —— 所以这里重启绝不会
    // 把「装到一半失败」重新跑一遍。
    if let Err(error) = run() {
        // 只剩 OpenGL 1.1 的机器（远程桌面会话强制降到 1.1、驱动没装全）就靠
        // 这一步救回来。子进程带 SWGL_STAGE：它再失败会直接走到下面的报错，
        // 不会无限重启。
        //
        // 放锁必须在重启**之前**：run() 失败前已经握过单实例锁了（窗口没开出来，
        // 锁是在那之前拿的），而子进程开界面时也要拿同一把锁 —— 不还回去，
        // 子进程会被"已有实例"直接挡掉，那正是以前修过的"装完了客户端弹不出来"。
        // 父进程到此窗口已经没指望了，放锁没有任何损失。
        single::release();
        if let Some(code) = relaunch_with_software_opengl(&log) {
            std::process::exit(code);
        }
        shared::dialog::report_failure(
            &log,
            "AICRM 安装程序启动失败",
            &format!(
                "{error}\n\n安装界面需要 OpenGL 2.0+。正常情况下随包的软件渲染组件\
                 （Mesa）会在首次开窗失败后自动就位并重启，任何机器都开得出窗口；\
                 看到这条消息说明自动就位也没成功（常见原因：临时目录不可写、\
                 磁盘空间不足，详见日志）。请把本内容和日志一起发给技术支持。"
            ),
        );
        std::process::exit(1);
    }
}

/// 安装器自己的错误日志（与客户端同一个目录，客户发日志时只需一个位置）。
fn setup_error_log() -> PathBuf {
    shared::paths::install_dir().join("install.err.log")
}

fn run() -> eframe::Result {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    // 打包时预填的云端地址；`--endpoint` 可覆盖（联调用）。
    // 先算出来：下面几条无界面子命令（--check / --extract-only）都要用它。
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
            // 560×480：与主控台登录窗同尺寸（2026-10-13 按用户要求）——
            // 两个窗口一样大，客户眼里才是"同一套东西"。
            .with_inner_size([560.0, 480.0])
            .with_resizable(false)
            .with_title("AICRM 安装"),
        ..Default::default()
    };
    // 只走有界面一条路：随包的 Mesa 保证任何机器都开得出窗口（见 main 里的
    // 惰性重启），所以创建失败且 Mesa 也没救回来就是明确故障 —— 由 main 报错
    // 退出，不静默降级无界面安装（2026-10-15 用户明确要求：必须使用 OpenGL）。
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

/// 随包的软件 OpenGL 文件（Mesa llvmpipe，x64）。与 `src/pack/main.rs` 的 SWGL
/// 常量各持一份 —— pack 是独立 bin，setup 引用不到它，改一处记得改另一处。
const SWGL_FILES: [&str; 5] = [
    "opengl32.dll",
    "libgallium_wgl.dll",
    "msvcp140.dll",
    "vcruntime140.dll",
    "vcruntime140_1.dll",
];

/// staging 目录路径：父进程启动子进程时把它设进子进程的环境，一身兼两职 ——
/// ① 子进程看到它就不再重启（防循环）；② 子进程清理陈旧临时目录时靠它认出
/// 自己的 staging 目录并避开（正在运行的 exe 和 Mesa DLL 都在里面）。
const SWGL_STAGE: &str = "AICRM_SWGL_STAGE";

/// 原始安装包路径：父进程（客户双击的那个进程）启动子进程时把自己的路径传下去。
/// 装完删安装包删的是这个路径 —— 子进程的 `current_exe()` 是临时目录里的副本，
/// 删它没意义（父进程退出时会连整个 staging 目录收走）。
const SETUP_ORIGIN: &str = "AICRM_SETUP_ORIGIN";

/// 把随包的软件 OpenGL 与自身副本落到临时目录，从那里重启自己。
///
/// **只有第一次开窗失败后才调它**（见 main）：绝大多数机器有硬件 OpenGL 2.0+，
/// 直接开窗就行，不必每人复制上百兆、重启一遍。
///
/// 为什么失败后必须重启：客户机常常只给 OpenGL 1.1（远程桌面会话强制降到 1.1，
/// 与显卡无关；或驱动没装全），而界面框架（eframe/glutin）要求 2.0+，
/// opengl32.dll 是**编译期导入表**依赖 —— 进程一启动就由 Windows loader 按
/// 「应用目录优先」解析，运行期再 SetDllDirectory 已经来不及（import 早就解析完
/// 了）。所以唯一的办法是让「应用目录里就有 Mesa」：把这 5 个 DLL 和本 exe 的
/// 副本放进同一个临时目录，从副本启动 —— loader 就会优先加载 Mesa 的
/// opengl32.dll，渲染走纯软件（GALLIUM_DRIVER=llvmpipe，少了这条 Mesa 会去找
/// 硬件驱动并当场失败，实测进程秒退、无任何日志）。
///
/// 返回 `Some(退出码)` = 已经重启过并等到了结果，调用方原样退出；
/// 返回 `None` = 这一步没做成或没必要做（已带 Mesa 重启过、没有载荷、
/// 铺失败），由调用方决定报错。
fn relaunch_with_software_opengl(log: &Path) -> Option<i32> {
    // 已经是重启后的进程了：staging 目录就在自己身上（见 SWGL_STAGE）。
    if std::env::var_os(SWGL_STAGE).is_some() {
        return None;
    }
    // --check / --extract-only 不建窗口，用不到 Mesa：跳过 staging，
    // 免得支持人员每次自检都先复制上百兆、再重启一遍。
    if std::env::args()
        .skip(1)
        .any(|item| item == "--check" || item == "--extract-only")
    {
        return None;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            shared::dialog::write_failure(
                log,
                "软件 OpenGL 就位失败",
                &format!("取不到自身路径：{error}"),
            );
            return None;
        }
    };
    let stage = std::env::temp_dir().join(format!("aicrm-swgl-{}", std::process::id()));
    let outcome = match relaunch_from_stage(&exe, &stage) {
        // 载荷不在（开发机上直接跑 cargo 出来的 exe）：没什么可铺的，静默继续
        // —— 那不是故障，有硬件 OpenGL 2.0+ 的机器窗口照样开。
        Ok(None) => None,
        Ok(Some(code)) => Some(code),
        // 铺不上不一定装不了（有硬件加速的机器照样走），但必须留痕：1.1 机器上
        // 随后报的 OpenGL 错，要靠这一行区分「就位失败」和别的原因。
        Err(reason) => {
            shared::dialog::write_failure(log, "软件 OpenGL 就位失败", &reason);
            None
        }
    };
    // 临时目录无论成败都清掉：客户机的临时目录不该替我们攒一百多兆垃圾。
    let _ = std::fs::remove_dir_all(&stage);
    outcome
}

/// 真正干活的那一半。`Ok(None)` = 没有载荷可铺；`Err` = 铺失败了（原因可读）。
fn relaunch_from_stage(exe: &Path, stage: &Path) -> Result<Option<i32>, String> {
    let _ = std::fs::create_dir_all(stage);
    let Ok(mut archive) = payload_zip() else {
        return Ok(None);
    };
    for name in SWGL_FILES {
        let mut entry = archive
            .by_name(&format!("tools/swgl/{name}"))
            .map_err(|error| format!("载荷里缺少软件渲染组件 {name}：{error}"))?;
        let target = stage.join(name);
        let mut file = std::fs::File::create(&target)
            .map_err(|error| format!("无法写入 {}：{error}", target.display()))?;
        std::io::copy(&mut entry, &mut file)
            .map_err(|error| format!("无法解出软件渲染组件 {name}：{error}"))?;
    }
    // 复制自身：载荷页脚在副本里同样有效，子进程按同一套读法解包。
    let staged_exe = stage.join(exe.file_name().ok_or("安装程序文件名异常")?);
    std::fs::copy(exe, &staged_exe).map_err(|error| format!("无法复制安装程序副本：{error}"))?;
    let mut command = Command::new(&staged_exe);
    command.args(std::env::args().skip(1));
    command.env(SWGL_STAGE, stage);
    command.env(SETUP_ORIGIN, exe);
    command.env("GALLIUM_DRIVER", "llvmpipe");
    command.env("LIBGL_ALWAYS_SOFTWARE", "1");
    let status = command
        .status()
        .map_err(|error| format!("无法启动安装界面：{error}"))?;
    Ok(Some(status.code().unwrap_or(1)))
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

/// 清掉以前残留的载荷目录与 staging 目录。
///
/// 安装器中途被杀（或客户直接关机）会留下近百兆的临时文件；staging 目录
/// （软件 OpenGL + 安装程序副本）更有一百多兆 —— 客户机的临时目录不该替我们
/// 攒垃圾。只动自己命名的那两类目录，别人的东西一概不碰。
///
/// 自己的 staging 目录必须避开：正在运行的 exe 和 Mesa DLL 都在里面
/// （路径由父进程经 SWGL_STAGE 传下来，拿不到说明本进程没被 staging 重启）。
fn remove_stale_payload_dirs(current: &Path) {
    let Some(parent) = current.parent() else {
        return;
    };
    let own_stage = std::env::var_os(SWGL_STAGE).map(PathBuf::from);
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let stale =
            name.starts_with("aicrm-install-") || name.starts_with("aicrm-swgl-");
        if stale && entry.path() != current && Some(&entry.path()) != own_stage.as_ref() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// 装完把安装包删掉。
///
/// 正在运行的 exe 删不掉自己，交给一个延迟执行的命令：等本进程退出后再删。
/// 用 ping 当延时 —— 不依赖控制台，也不会像 timeout 那样在被重定向时报错。
///
/// 删的是**客户原本双击的那个安装包**：staging 重启后的子进程，
/// `current_exe()` 是临时目录里的副本，删它没意义（父进程退出时会连整个
/// staging 目录收走）。原始路径由父进程经 SETUP_ORIGIN 传下来；拿不到才
/// 退回删自己（开发机上直接跑、没有 staging 的老行为）。
fn remove_self_after_exit() {
    let Some(exe) = self_delete_target() else {
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
        command.raw_arg(format!("/c {script}"));
        shared::process::hidden(&mut command);
    }
    #[cfg(not(windows))]
    command.args(["/c", &script]);
    let _ = command.spawn();
}

/// 装完要删的安装包路径：优先原始安装包（父进程经 SETUP_ORIGIN 透传的），
/// 文件确实不在才退回当前 exe。
fn self_delete_target() -> Option<PathBuf> {
    if let Some(origin) = std::env::var_os(SETUP_ORIGIN) {
        let origin = PathBuf::from(origin);
        if origin.is_file() {
            return Some(origin);
        }
    }
    std::env::current_exe().ok()
}

/// 调用载荷里的 install.ps1 —— 装盘、注册自启、启动服务；`-CheckOnly` 时只做检查。
///
/// 复用安装脚本而不是在 Rust 里重写一遍：拷贝文件、icacls 收紧权限、写 HKCU Run、
/// 环境检查，这些逻辑已经在那边验证过了，重复实现只会多一处会走样的地方。
/// `launch_console` 一路透传给脚本的 `-LaunchConsole`：装完客户端由安装器
/// 统一拉起（open_console），不让脚本重复拉起（详见 install.ps1 该参数说明）。
/// 返回 (输出, 是否成功)：检查模式退出码 1 表示"有必须项没过"，那是结论不是故障，
/// 所以由调用方决定怎么解读。
fn run_script(script: &Path, base_url: &str, check_only: bool, launch_console: bool) -> Result<(String, bool), String> {
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
        .arg(base)
        .arg("-LaunchConsole")
        .arg(if launch_console { "true" } else { "false" });
    if check_only {
        command.arg("-CheckOnly");
    }
    // 安装/检查的全过程都藏窗：powershell 是控制台程序，不藏的话整个
    // install.ps1 运行期间客户桌面上一直挂着一个黑窗（实测就是这样）。
    shared::process::hidden(&mut command);

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
    let (output, _) = run_script(&script, endpoint, true, false)?;
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
/// 只在安装线程里调用（安装器只走有界面一条路，见模块头说明）。
///
/// `prepared` 是环境准备阶段已经解好的载荷目录（复用它，少解一遍）；
/// 传 None 则现场解。
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
    // launch_console=false：客户端由安装器装完后统一拉起（open_console），
    // 脚本里再拉一次会被安装器还握着的单实例锁挡掉，白闪一下。
    let (log, ok) = run_script(&script, base, false, false)?;
    if !ok {
        return Err(format!("安装脚本失败：\n{}", log.trim()));
    }
    let _ = std::fs::remove_dir_all(&work);
    Ok(log)
}

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
    // 拉起来之后必须确认它真的活下来了：安装器放锁与 console 抢锁之间有
    // 竞争窗口，抢输的那个会自查到"已有实例"后直接退出 —— 现象就是
    // "装完了，客户端闪了一下就不见"。等一秒看进程还在不在，不在就再拉一次；
    // 第二次还是秒退就不能再装作成功 —— 那句话会骗过界面上的"安装完成"。
    let spawn = || Command::new(&exe).spawn().map_err(|error| format!("启动客户端失败：{error}"));
    let mut child = spawn()?;
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let mut died = matches!(child.try_wait(), Ok(Some(_)));
    if died {
        child = spawn()?;
        std::thread::sleep(std::time::Duration::from_millis(1500));
        died = matches!(child.try_wait(), Ok(Some(_)));
    }
    if died {
        return Err(
            "客户端启动后立刻退出了（重试过一次）。请手动双击桌面的「AICRM」图标打开；\
             若还是闪退，请把安装日志发给技术支持。"
                .to_owned(),
        );
    }
    let _ = child;
    Ok("已启动客户端".to_owned())
}

/// 环境准备线程交给界面的东西：解出来的载荷目录 + 检查清单。
/// 抽成别名免得字段与 `poll_check` 各写一遍这串嵌套类型。
type CheckOutcome = Result<(PathBuf, Vec<shared::preflight::CheckItem>), String>;

/// 安装线程交给界面的东西：安装日志 + 「客户端拉起来没有」。
/// 抽成别名免得字段与 `poll` 的 match 各写一遍这串嵌套类型。
type InstallOutcome = Result<(String, Result<String, String>), String>;

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
    check_pending: Option<Receiver<CheckOutcome>>,
    /// 已经解出来的载荷目录：检查通过后直接用这一份安装，不解第二遍。
    work: Option<PathBuf>,
    /// 安装线程的回报：安装日志，加上「客户端拉起来没有」—— 后者失败不算
    /// 安装失败，只是要在完成页提示一句（2026-10-15 起在安装线程里跑完）。
    pending: Option<Receiver<InstallOutcome>>,
    /// 「打开客户端」的结果，失败时直接说清楚原因，别让用户猜。
    hint: Option<String>,
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

    /// 环境检测结果（2026-10-13 方案 C，Hero 居中）：
    /// 绿色徽标 + 一句话状态 + 进度条 + 两个**同宽同色**的玻璃按钮。
    ///
    /// 按钮只复用 `theme::glass_button` 一种表单：用户明确要求两个按钮
    /// 颜色一致，主次靠位置表达，不靠颜色。明细不再逐项罗列 —— 过了就是
    /// 满格绿条，没过给出第一条必须项原因，完整明细在错误日志里。
    fn environment_ui(&mut self, ui: &mut egui::Ui) {
        if self.checks.is_none() {
            return;
        }
        let blocking = self.blocked();
        let (color, status) = if blocking.is_empty() {
            (theme::GLASS_OK, "环境检测通过")
        } else {
            (theme::BAD, "环境检测未通过")
        };

        ui.vertical_centered(|ui| {
            theme::check_badge(ui, color, 48.0);
        });
        ui.add_space(32.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new(status).size(20.0).strong());
        });
        ui.add_space(28.0);
        ui.vertical_centered(|ui| {
            theme::glass_progress(ui, 320.0, Some(1.0), color);
        });

        if !blocking.is_empty() {
            if let Some(first) = blocking.first() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{}：{}", first.title, first.detail))
                            .size(12.0)
                            .color(theme::BAD),
                    );
                });
            }
            ui.add_space(44.0);
            ui.vertical_centered(|ui| {
                ui.set_width(240.0);
                if theme::glass_button(ui, 240.0, "重新检测").clicked() {
                    let ctx = ui.ctx().clone();
                    self.checks = None;
                    self.begin_check(&ctx);
                }
                ui.add_space(14.0);
                if theme::glass_button(ui, 240.0, "关闭").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            return;
        }

        ui.add_space(44.0);
        ui.vertical_centered(|ui| {
            ui.set_width(240.0);
            if theme::glass_button(ui, 240.0, "开始安装").clicked() {
                let ctx = ui.ctx().clone();
                self.start_install(&ctx);
            }
            ui.add_space(14.0);
            if theme::glass_button(ui, 240.0, "重新检测").clicked() {
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

        // 安装期间**不放**单实例名额（2026-10-15 修）：此前这里开装即 release，
        // 整个安装期不持锁 —— 重复双击安装程序会真并发跑两个 install.ps1，
        // 并发写同一个安装目录（拷文件撞「正在使用」、抢注册表启动项），安装
        // 就这样莫名其妙失败，「只会 front 到前台」的假设在放锁后已不成立。
        // 锁持到装完为止：install.ps1 末尾自己拉起 console 会被挡一下（它自查
        // 到已有实例就退），但紧接着由安装器统一拉起，结果一样 —— 安装期并发
        // 才是真会装坏的。

        // 环境准备已经解过载荷，直接用那一份（没有就现解）。
        let prepared = self.work.clone();
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            // open_console 里有「等 1.5 秒看客户端活没活」的重试逻辑：整个放
            // 安装线程跑，UI 线程只等一条 channel 消息。放在 poll() 里调会让
            // 安装窗冻结至多 3 秒，观感等同卡死（2026-10-15 修）。
            // 返回 (安装日志, 拉起客户端的结果)：后者失败不该算安装失败，
            // 只是要在界面上提示一句。
            let outcome = (|| -> Result<(String, Result<String, String>), String> {
                let log = install_payload(&base, prepared)?;
                // 装完才放名额，紧接着拉起客户端：放早了第二个安装器会钻进来
                // 并发安装；放晚了客户端被锁挡掉（「装完了客户端闪一下就不见」）。
                single::release();
                let console = open_console();
                Ok((log, console))
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
            Ok(Ok((log, console))) => {
                self.pending = None;
                self.state = State::Done(format!(
                    "{log}\n\n下一步：客户端打开后会用云端账号登录一次，\
                     以后打开就直接进主界面。"
                ));
                match console {
                    // 客户端已经出面了，安装窗口就没必要继续占着任务栏 ——
                    // 两个窗口并排摆着，用户会以为装了两遍。
                    Ok(_) => {
                        // 装完把安装包本身删掉 —— 客户桌面上只该有一个客户端图标，
                        // 不该堆着安装程序，更不该出现"点开说找不到文件"的残留
                        // （微信也是这样）。
                        remove_self_after_exit();
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    // 软件装好了，只是客户端没自动起来：不删安装包，让人能就地重试。
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

    /// 安装中 / 完成 / 失败 三态。都是居中一句话 + 玻璃元素，不堆说明文字。
    fn status_ui(&mut self, ui: &mut egui::Ui) {
        match &self.state {
            State::Idle => {}
            State::Working(step) => {
                ui.add_space(28.0);
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(step).size(13.0));
                });
            }
            State::Done(message) => {
                ui.vertical_centered(|ui| {
                    theme::check_badge(ui, theme::GLASS_OK, 48.0);
                });
                ui.add_space(32.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("安装成功").size(20.0).strong());
                });
                ui.add_space(16.0);
                ui.vertical_centered(|ui| {
                    ui.label(theme::muted("客户端已装好并注册了开机自启，正在为你打开…"));
                });
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.set_width(400.0);
                    egui::ScrollArea::vertical()
                        .max_height(120.0)
                        .show(ui, |ui| {
                            ui.label(theme::faint(message));
                        });
                });
            }
            State::Failed(message) => {
                ui.vertical_centered(|ui| {
                    theme::check_badge(ui, theme::BAD, 48.0);
                });
                ui.add_space(32.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("安装失败").size(20.0).strong());
                });
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.set_width(400.0);
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(message).size(12.0).color(theme::BAD));
                        });
                });
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.set_width(240.0);
                    if theme::glass_button(ui, 240.0, "重新检测").clicked() {
                        let ctx = ui.ctx().clone();
                        self.state = State::Idle;
                        self.checks = None;
                        self.begin_check(&ctx);
                    }
                    ui.add_space(14.0);
                    if theme::glass_button(ui, 240.0, "关闭").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
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
            // 玻璃底：盖住 theme::apply 的 PAGE。egui 没有背景模糊，
            // 玻璃感 = 浅灰绿底 × 半透明白层 × 受光高光边（见 theme.rs）。
            ui.painter()
                .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, theme::GLASS_PAGE);

            // 方案 C（Hero 居中）：页内不再放标题（窗口标题栏已有" AICRM 安装"），
            // 整块内容在窗口里纵向居中，只留徽标 / 状态 / 进度条 / 两个对齐按钮。
            ui.vertical_centered(|ui| {
                ui.set_width(ui.available_width());
                // 整块高约 250，剩下的空间对半分——上下留白相等才像"设计过"。
                ui.add_space(((ui.available_height() - 250.0) / 2.0).max(12.0));

                if self.checking && self.checks.is_none() {
                    // 检测中：没有真实进度可报，用一段来回扫描的亮条表达"在跑"
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new("环境检测中").size(16.0).strong());
                    });
                    ui.add_space(28.0);
                    ui.vertical_centered(|ui| {
                        theme::glass_progress(ui, 320.0, None, theme::GLASS_OK);
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
            });
        });
    }
}
