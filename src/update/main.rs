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

#[path = "../backend/sha256.rs"]
mod sha256;

/// 替换前要停掉的客户端程序。按镜像名杀 —— 客户机上只有安装目录一份，
/// 比按路径过滤简单，也避免依赖 CIM/WMI（部分机器上被安全软件禁用）。
const CLIENT_PROGRAMS: [&str; 4] = [
    "aicrm_windows_api.exe",
    "aicrm-console.exe",
    "aicrm-login.exe",
    "aicrm-tray.exe",
];

/// 允许被替换的文件白名单。收到名单外的文件名一律报错 ——
/// 宁可这次更新失败，也不能把文件悄悄写到别的位置（实测踩过：把发布产物名
/// 当成目标名，结果安装目录多出一个 agent-0.1.4.exe，主程序根本没换）。
const ALLOWED_TARGETS: [&str; 4] = [
    "aicrm_windows_api.exe",
    "aicrm-console.exe",
    "aicrm-login.exe",
    "aicrm-update.exe",
];

fn main() {
    let options = Options::parse();
    if let Err(error) = apply(&options) {
        log(&options.log_file(), &format!("更新失败：{error}"));
        write_result(&options, "failed", &error);
        std::process::exit(1);
    }
}

struct Options {
    install_dir: PathBuf,
    grace: Duration,
}

impl Options {
    fn parse() -> Self {
        let mut install_dir = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("AICRM-Windows-API");
        let mut grace = 8u64;
        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--install-dir" => {
                    if let Some(value) = arguments.next() {
                        install_dir = PathBuf::from(value);
                    }
                }
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
        if let Err(error) = std::fs::copy(source, &target) {
            // 换到一半失败：先把已替换的还原，别留个半新半旧的安装目录
            let _ = rollback(&options.install_dir, &backup, &replaced);
            let _ = start_clients(options);
            return Err(format!("替换 {name} 失败：{error}"));
        }
        replaced.push(name.clone());
    }

    // ③ 重启 → 确认活下来 → 刷新快捷方式 → 清标记
    log(&options.log_file(), "替换完成，重启客户端");
    start_clients(options)?;
    refresh_shortcuts(options);
    // 过期安装包也一并收走：客户桌面上不该堆着点不开的旧版本。
    archive_stale_packages(options);
    confirm_restart(options, &replaced, &backup)?;
    let _ = std::fs::remove_file(&pending_file);
    let message = format!("已更新到 {version}；旧版本备份在 {}", backup.display());
    write_result(options, "applied", &message);
    log(&options.log_file(), &format!("更新成功 {version}"));
    Ok(())
}

/// 停掉正在运行的客户端。正在运行的 exe 无法被覆盖，这一步不能省。
fn stop_clients() {
    for name in CLIENT_PROGRAMS {
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", name])
            .output();
    }
    std::thread::sleep(Duration::from_secs(1));
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
    match Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq aicrm_windows_api.exe", "/NH"])
        .output()
    {
        Ok(result) => String::from_utf8_lossy(&result.stdout)
            .to_ascii_lowercase()
            .contains("aicrm_windows_api.exe"),
        Err(_) => true,
    }
}

/// 规范快捷方式的文件名（桌面与开始菜单共用）。
const SHORTCUT_NAME: &str = "AICRM 客户端.lnk";
/// 安装时存档的快捷方式模板：更新器用它重建，避免在 Rust 里重写一遍 .lnk 的二进制格式。
const SHORTCUT_TEMPLATE: &str = "shortcut-AICRM客户端.lnk";

/// 刷新快捷方式：**先清掉旧的和不规范的，再按存档模板重建**。
///
/// 为什么不能只刷新时间戳：客户桌面上可能有上一版安装包留下的、指向旧路径或旧
/// 安装包的快捷方式。只刷新自己的那一个，旧的会一直留着，客户点了打不开东西。
fn refresh_shortcuts(options: &Options) {
    let template = options
        .install_dir
        .join("data")
        .join("state")
        .join(SHORTCUT_TEMPLATE);
    let template_ready = template.is_file();

    let mut removed = 0usize;
    for root in shortcut_roots() {
        removed += cleanup_shortcuts(&root, options.install_dir.as_path(), template_ready, 0);
    }

    // 桌面 + 用户开始菜单 + 公共开始菜单，各放一份
    let mut rebuilt = 0usize;
    if template_ready {
        for target in shortcut_locations() {
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::copy(&template, &target).is_ok() {
                rebuilt += 1;
            }
        }
    }

    // 无条件刷新：桌面上可能有被删/被挪走的文件留下的残留图标，客户不会去按 F5。
    let _ = Command::new("ie4uinit.exe").arg("-show").output();
    log(
        &options.log_file(),
        &format!("快捷方式：清理 {removed} 个、重建 {rebuilt} 个"),
    );
}

fn shortcut_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let profile = PathBuf::from(profile);
        roots.push(profile.join("Desktop"));
        roots.push(profile.join("AppData/Roaming/Microsoft/Windows/Start Menu/Programs"));
    }
    roots.push(PathBuf::from("C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs"));
    roots
}

fn shortcut_locations() -> Vec<PathBuf> {
    shortcut_roots()
        .into_iter()
        .map(|root| root.join(SHORTCUT_NAME))
        .collect()
}

/// 归档过期安装包：把桌面/下载里版本与当前不一致的 `AICRM-Setup-*.exe`
/// 挪进安装目录的 `updates/archive/`。
///
/// 每次发布都会在客户桌面留一个新的安装包，旧版本越堆越多，而且一旦被挪走或清掉，
/// 桌面上会留下点开就报「找不到文件」的残留图标。客户不该管这些 —— 更新时顺手收走。
fn archive_stale_packages(options: &Options) {
    let current = env!("CARGO_PKG_VERSION");
    let archive = options.install_dir.join("updates").join("archive");

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let profile = PathBuf::from(profile);
        roots.push(profile.join("Desktop"));
        roots.push(profile.join("Downloads"));
    }

    let mut moved = 0usize;
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let lowered = name.to_lowercase();
            if !lowered.starts_with("aicrm-setup-") || !lowered.ends_with(".exe") {
                continue;
            }
            // 与当前版本一致的那份留着（客户可能还要拿它装别的机器）
            let version = lowered
                .trim_start_matches("aicrm-setup-")
                .trim_end_matches(".exe");
            if version == current {
                continue;
            }
            if std::fs::create_dir_all(&archive).is_err() {
                continue;
            }
            if std::fs::rename(&path, archive.join(name)).is_ok() {
                moved += 1;
            }
        }
    }
    if moved > 0 {
        log(
            &options.log_file(),
            &format!("已归档 {moved} 个过期安装包到 {}", archive.display()),
        );
    }
}

/// 删掉陈旧的 AICRM 快捷方式。
///
/// 判定「陈旧」的两条依据：文件名里带 aicrm（旧版本可能用别的名字），
/// 或 .lnk 内容里出现 AICRM 的安装目录 / 安装包名。规范名称的那个也一起删 ——
/// 反正随后会用模板重建，删掉重建比原地改更干净。
fn cleanup_shortcuts(directory: &Path, install_dir: &Path, template_ready: bool, depth: usize) -> usize {
    if depth > 3 {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            removed += cleanup_shortcuts(&path, install_dir, template_ready, depth + 1);
            continue;
        }
        let is_shortcut = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.eq_ignore_ascii_case("lnk"))
            .unwrap_or(false);
        if !is_shortcut {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_lowercase();
        let canonical = name == SHORTCUT_NAME.to_lowercase();
        let related = name.contains("aicrm")
            || matches_shortcut_content(&path, install_dir)
            || content_contains(&path, "aicrm-setup");
        if !related {
            continue;
        }
        // 没有模板时不能删规范快捷方式 —— 删了就再没人重建，客户桌面上会少一个入口。
        if canonical && !template_ready {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn matches_shortcut_content(path: &Path, install_dir: &Path) -> bool {
    content_contains(path, &install_dir.to_string_lossy().to_lowercase())
}

/// .lnk 是二进制格式，但目标路径以明文（UTF-16LE 或 ANSI）存在里面，
/// 按内容判断比解析格式或引入 COM 依赖简单可靠。
fn content_contains(path: &Path, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let utf16: Vec<u8> = needle
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    if !utf16.is_empty() && bytes.windows(utf16.len()).any(|window| window == utf16.as_slice()) {
        return true;
    }
    String::from_utf8_lossy(&bytes).to_lowercase().contains(needle)
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
