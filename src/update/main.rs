//! AICRM 客户端自更新的「外部执行者」。
//!
//! 为什么单独编一个 exe，而不是用 PowerShell 脚本：
//!   ① 客户机上 PowerShell 可能被执行策略或安全软件拦掉 —— 开发机就复现过：
//!      客户端 spawn 脚本这一步直接挂住，更新命令既没成功也没失败；
//!   ② 走 `-ExecutionPolicy Bypass` 在受管机器上本身就是可疑行为；
//!   ③ 校验、备份、替换、重启放在自己的 exe 里，行为完全可控、可测。
//!
//! 流程：等宽限期（让主进程把命令结果回传云端）→ 复算摘要 → 备份旧版 →
//! 停客户端进程 → 覆盖 → 按主进程记下的启动命令重启 → 失败回滚。
//!
//! 成败最终以「重启后心跳带上来的版本号」为准（云端判断），这里的
//! update-result.json 只给人排查用。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[path = "../shared/mod.rs"]
mod shared;

#[path = "../shared/sha256.rs"]
mod sha256;

#[path = "shortcut.rs"]
mod shortcut;

/// 允许被替换的文件白名单。收到名单外的文件名一律报错 ——
/// 宁可这次更新失败，也不能把文件悄悄写到别的位置（实测踩过：把发布产物名
/// 当成目标名，结果安装目录多出一个 agent-0.1.4.exe，主程序根本没换）。
const ALLOWED_TARGETS: [&str; 3] = [
    "aicrm_windows_api.exe",
    "aicrm-console.exe",
    "aicrm-update.exe",
];

fn main() {
    let options = Options::parse();
    // 安装时的入口创建走同一条实现：客户机上 COM/PowerShell 可能被安全软件
    // 拦掉，自己写 .lnk 不依赖它们。
    if options.ensure_shortcut {
        match shortcut::refresh(&options.install_dir, &options.log_file()) {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("建快捷方式失败：{error}");
                std::process::exit(1);
            }
        }
    }
    if let Err(error) = apply(&options) {
        log(&options.log_file(), &format!("更新失败：{error}"));
        write_result(&options, "failed", &error);
        // 出错时也要保证客户端是活的：更新器已经把它停掉了，自己再躺下的话
        // 这台机器就彻底哑了。已经活着就什么都不做（多数情况是并发实例已重启过）。
        if !service_alive() {
            let _ = start_clients(&options);
        }
        std::process::exit(1);
    }
}

struct Options {
    install_dir: PathBuf,
    grace: Duration,
    /// 安装脚本用它来建入口 —— 同一个实现，不重复造一份。
    ensure_shortcut: bool,
}

impl Options {
    fn parse() -> Self {
        let mut install_dir = shared::paths::install_dir();
        let mut grace = 8u64;
        let mut ensure_shortcut = false;
        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--install-dir" => {
                    if let Some(value) = arguments.next() {
                        install_dir = PathBuf::from(value);
                    }
                }
                "--ensure-shortcut" => ensure_shortcut = true,
                "--grace-seconds" => {
                    if let Some(value) = arguments.next() {
                        grace = value.parse().unwrap_or(8);
                    }
                }
                _ => {}
            }
        }
        Self {
            install_dir,
            // 上限兜一下：宽限期只是等主进程回传结果，不该无限期拖着。
            grace: Duration::from_secs(grace.min(120)),
            ensure_shortcut,
        }
    }

    fn updates_dir(&self) -> PathBuf {
        self.install_dir.join("updates")
    }

    fn log_file(&self) -> PathBuf {
        self.updates_dir().join("apply-update.log")
    }
}

fn apply(options: &Options) -> Result<(), String> {
    let updates = options.updates_dir();
    let pending_file = updates.join("pending.json");

    std::thread::sleep(options.grace);

    let raw = std::fs::read_to_string(&pending_file)
        .map_err(|error| format!("读不到 pending.json：{error}"))?;
    let pending: serde_json::Value =
        serde_json::from_str(&raw).map_err(|error| format!("pending.json 不合法：{error}"))?;
    let version = pending
        .get("version")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_owned();
    let items = pending
        .get("files")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        return Err("pending.json 里没有文件".to_owned());
    }
    // 读完立刻消费掉这个标记。更新器重启客户端之后，新起的服务会再检查一次
    // pending —— 若不消费，它会拉起**第二个更新器**，那个实例读不到标记就报失败，
    // 顺手把刚拉起的进程杀掉，这台机器就变成采不到数据的哑机（实测踩过）。
    let _ = std::fs::remove_file(&pending_file);
    log(&options.log_file(), &format!("开始应用更新 {version}"));

    // ① 复算摘要。下载与校验在两个进程里做过，这里是最后一道闸。
    let stage = updates.join(&version);
    let mut planned: Vec<(String, PathBuf)> = Vec::new();
    for item in &items {
        let name = item
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_owned();
        let expected = item
            .get("sha256")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_lowercase();
        if name.is_empty() || Path::new(&name).file_name().and_then(|v| v.to_str()) != Some(&name) {
            return Err(format!("文件名不安全：{name}"));
        }
        if !ALLOWED_TARGETS.contains(&name.as_str()) {
            return Err(format!("不允许替换的文件：{name}"));
        }
        let source = stage.join(&name);
        let bytes =
            std::fs::read(&source).map_err(|error| format!("读不到暂存文件 {name}：{error}"))?;
        let digest = sha256::hex_digest(&bytes);
        if !expected.is_empty() && digest != expected {
            return Err(format!("校验不通过：{name}"));
        }
        planned.push((name, source));
    }
    log(
        &options.log_file(),
        &format!("校验通过，共 {} 个文件", planned.len()),
    );

    // ② 停进程 → 备份 → 覆盖
    stop_clients();
    let backup = updates.join(format!("backup-{}", epoch_seconds()));
    std::fs::create_dir_all(&backup).map_err(|error| format!("建备份目录失败：{error}"))?;

    let mut replaced: Vec<String> = Vec::new();
    for (name, source) in &planned {
        let target = options.install_dir.join(name);
        if target.is_file() {
            std::fs::copy(&target, backup.join(name))
                .map_err(|error| format!("备份 {name} 失败：{error}"))?;
        }
        if let Err(error) = copy_with_retry(source, &target, name) {
            // 换到一半失败：先把已替换的还原，别留个半新半旧的安装目录
            let _ = rollback(&options.install_dir, &backup, &replaced);
            let _ = start_clients(options);
            return Err(error);
        }
        replaced.push(name.clone());
    }

    // ③ 重启 → 确认活下来 → 清理旧程序 → 刷新快捷方式 → 清标记
    log(&options.log_file(), "替换完成，重启客户端");
    start_clients(options)?;
    confirm_restart(options, &replaced, &backup)?;
    // 已下线的旧程序顺手清掉（名单见 shared/process.rs 的 OBSOLETE_PROGRAMS）。
    // 失败只记日志 —— 文件占用之类不该让整次更新作废，下次更新还会再试。
    for name in shared::process::OBSOLETE_PROGRAMS {
        let path = options.install_dir.join(name);
        if path.is_file() {
            match std::fs::remove_file(&path) {
                Ok(()) => log(&options.log_file(), &format!("已清理旧程序 {name}")),
                Err(error) => log(&options.log_file(), &format!("清理 {name} 失败：{error}")),
            }
        }
    }
    // 快捷方式与过期安装包的收尾：失败不该让整次更新作废（exe 已经换好了），
    // 只记一行日志，下次启动还会再补一次。
    if let Err(error) = shortcut::refresh(&options.install_dir, &options.log_file()) {
        log(&options.log_file(), &format!("快捷方式收尾失败：{error}"));
    }
    let _ = std::fs::remove_file(&pending_file);
    let message = format!("已更新到 {version}；旧版本备份在 {}", backup.display());
    write_result(options, "applied", &message);
    log(&options.log_file(), &format!("更新成功 {version}"));
    Ok(())
}

/// 停掉正在运行的客户端，并**确认它们真的退出了**才返回。
///
/// 顺序的坑（先杀服务会被主控台原地拉起来）见 `shared/process.rs` 的名单注释。
/// 这里补第二个坑：只杀一次、只睡 1 秒不够 —— 进程退出与文件句柄释放之间有延迟
/// （杀软扫描、系统索引都会拖住）。所以循环"杀→查→再杀"，直到确认干净（最多 15 秒）。
fn stop_clients() {
    for _ in 0..15 {
        let mut alive = false;
        for name in shared::process::CLIENT_PROGRAMS {
            shared::process::terminate(name);
        }
        for name in shared::process::CLIENT_PROGRAMS {
            if shared::process::running(name).unwrap_or(false) {
                alive = true;
            }
        }
        if !alive {
            // 句柄释放还要一点点时间，给系统一个缓冲
            std::thread::sleep(Duration::from_millis(500));
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// 覆盖文件，遇到"文件正在使用"重试几次（每次都重新停一遍客户端）。
///
/// 客户机上文件句柄的释放不由我们控制：杀软扫描、Explorer 预览、系统索引都
/// 可能多按住几百毫秒。重试比"一次失败就整次更新作废"划算得多 ——
/// 那会让客户停在一个半新半旧的版本上，还要等下一轮更新。
fn copy_with_retry(source: &Path, target: &Path, name: &str) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 0..12 {
        match std::fs::copy(source, target) {
            Ok(_) => return Ok(()),
            Err(error) => {
                last = error.to_string();
                if attempt < 11 {
                    stop_clients();
                    std::thread::sleep(Duration::from_millis(800));
                }
            }
        }
    }
    Err(format!("替换 {name} 失败：{last}"))
}

/// 按主进程启动时记下的命令行原样重启服务。
///
/// 服务参数（密钥目录、数据目录、云端地址、设备令牌…）由安装脚本拼过一次，
/// 在这里重拼一遍只会引入"拼错就采不到数据"的风险，所以直接复用。
fn start_clients(options: &Options) -> Result<(), String> {
    let recorded = options
        .install_dir
        .join("data")
        .join("state")
        .join("startup.json");
    let mut started = false;
    if let Ok(raw) = std::fs::read_to_string(&recorded) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(exe) = value.get("exe").and_then(|item| item.as_str()) {
                if Path::new(exe).is_file() {
                    let arguments: Vec<String> = value
                        .get("args")
                        .and_then(|item| item.as_array())
                        .map(|list| {
                            list.iter()
                                .filter_map(|item| item.as_str().map(|text| text.to_owned()))
                                .collect()
                        })
                        .unwrap_or_default();
                    match spawn_hidden(exe, &arguments) {
                        Ok(pid) => {
                            started = true;
                            log(
                                &options.log_file(),
                                &format!("已按记录的启动命令重启服务，PID={pid}"),
                            );
                        }
                        Err(error) => log(
                            &options.log_file(),
                            &format!("重启服务失败：{error}"),
                        ),
                    }
                }
            }
        }
    }

    // 界面与托盘单独拉起（--hidden：静默更新不该突然弹个窗口）。
    let console = options.install_dir.join("aicrm-console.exe");
    if console.is_file() {
        match spawn_hidden(&console.to_string_lossy(), &["--hidden".to_owned()]) {
            Ok(pid) => log(&options.log_file(), &format!("已启动客户端界面，PID={pid}")),
            Err(error) => log(&options.log_file(), &format!("启动客户端界面失败：{error}")),
        }
    }

    if !started && !console.is_file() {
        return Err("没有可启动的客户端程序".to_owned());
    }
    Ok(())
}

/// 启动一个不弹窗的进程，返回 PID。
///
/// 先试着带上 CREATE_BREAKAWAY_FROM_JOB：EDR 或宿主编排环境常把整棵进程树塞进
/// Job 对象，那样父进程一退出，新拉起的子进程会被一起收掉 —— 本机实测就出现过
/// 「更新成功但进程没活下来」。不在 Job 里时该标志会被忽略，失败就退回普通启动。
#[cfg(windows)]
fn spawn_hidden(exe: &str, arguments: &[String]) -> Result<u32, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    let launch = |flags: u32| {
        let mut command = Command::new(exe);
        command.args(arguments).creation_flags(flags);
        // stdio 必须断开：子进程若继承更新器的管道，更新器一退出管道就关了，
        // 服务下一次写 stdout 就会失败，而 Rust 的 println! 写失败会直接 panic
        // 终止进程 —— 表现就是「更新成功、进程却没了」。实测踩过。
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command.spawn()
    };
    match launch(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB) {
        Ok(child) => Ok(child.id()),
        Err(_) => launch(CREATE_NO_WINDOW)
            .map(|child| child.id())
            .map_err(|error| error.to_string()),
    }
}

#[cfg(not(windows))]
fn spawn_hidden(exe: &str, arguments: &[String]) -> Result<u32, String> {
    let mut command = Command::new(exe);
    command.args(arguments);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|child| child.id())
        .map_err(|error| error.to_string())
}

/// 重启后确认服务真的活着。
///
/// 为什么必须查：服务进程若因为安全软件、Job 对象或依赖缺失起不来，替换后的安装
/// 目录就是一个"采集不到数据"的哑机，而更新结果却写着成功 —— 云端只会看到设备
/// 版本号一直不变，排查时毫无线索。宁可这次更新失败并回滚。
fn confirm_restart(options: &Options, replaced: &[String], backup: &Path) -> Result<(), String> {
    for _ in 0..10 {
        std::thread::sleep(Duration::from_secs(1));
        if service_alive() {
            log(&options.log_file(), "重启确认通过：服务在运行");
            return Ok(());
        }
    }
    log(&options.log_file(), "重启后服务未存活，回滚到上一版本");
    rollback(&options.install_dir, backup, replaced);
    let _ = start_clients(options);
    Err("重启后服务未存活（可能被安全软件拦下），已回滚到上一版本".to_owned())
}

/// 用 tasklist 探活。查不到进程信息时返回 true —— 不能因为探活工具本身失败
/// 就把一次正常的更新判成失败。
fn service_alive() -> bool {
    shared::process::running(shared::process::SERVICE_PROGRAM).unwrap_or(true)
}

fn rollback(install_dir: &Path, backup: &Path, replaced: &[String]) {
    for name in replaced {
        let saved = backup.join(name);
        if saved.is_file() {
            let _ = std::fs::copy(&saved, install_dir.join(name));
        }
    }
}

fn log(path: &Path, message: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{}] {}", epoch_seconds(), message);
    }
}

fn write_result(options: &Options, status: &str, message: &str) {
    let payload = serde_json::json!({
        "status": status,
        "message": message,
        "at": epoch_seconds(),
    });
    let _ = std::fs::write(
        options.updates_dir().join("update-result.json"),
        serde_json::to_vec_pretty(&payload).unwrap_or_default(),
    );
}

fn epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}
