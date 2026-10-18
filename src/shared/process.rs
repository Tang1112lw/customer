//! 客户端进程的终止与探测：**杀进程名单与实现的唯一定义**。
//!
//! 以前"该杀哪些进程"散在 5 个地方（主控台两处、更新器两处、install.ps1 一处），
//! 托盘并进主控台之后还有 4 处名单带着已删除的 `aicrm-tray.exe` —— 改一处漏一处。
//! Rust 侧现在统一从这里取；`install.ps1` 的 `$stopNames` 无法引入本模块，
//! 改动时需人工同步。

use std::process::Command;

/// 已下线但可能还留在安装目录的旧程序与启动器：登录窗（已并入主控台）、
/// 托盘（更早已并入）、登录窗时代生成的「登录.cmd」（它指向的 exe 已不存在，
/// 留着就是个坏快捷方式）。更新器与安装脚本在收尾时按此清单清理。
pub const OBSOLETE_PROGRAMS: [&str; 3] = ["aicrm-login.exe", "aicrm-tray.exe", "登录.cmd"];

/// 客户端全部进程。**顺序敏感**：看守者在前，被看守的服务在最后 ——
/// 先杀服务的话，主控台会立刻把它原地拉起来，文件覆盖就撞上"正在使用"。
///
/// `aicrm-login` 已不再分发，留在名单里是为了覆盖从旧版本升级的机器
/// （更新那一刻旧登录窗可能还开着）。
pub const CLIENT_PROGRAMS: [&str; 3] = [
    "aicrm-console.exe",
    "aicrm-login.exe",
    "aicrm_windows_api.exe",
];

/// 后台服务的镜像名。
pub const SERVICE_PROGRAM: &str = "aicrm_windows_api.exe";

/// 给命令加上「不留控制台窗口」（与 `install.ps1` 的 `-WindowStyle Hidden` 等价）。
///
/// 为什么收敛到这一处：`CREATE_NO_WINDOW = 0x0800_0000` 这对知识曾经散在 6 处
/// （setup 三处、credentials、service/mod、agent_service），每处都自带一段注释
/// 解释「为什么不能闪黑窗」，每次有人新起进程都要重新抄一遍，漏抄就是客户桌面
/// 上多一个黑窗。从此一处定义、各处调用：
///
/// ```ignore
/// let mut command = Command::new("taskkill");
/// command.args(["/F", "/IM", name]);
/// hidden(&mut command).output()
/// ```
#[cfg(windows)]
pub fn hidden(command: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    /// 不留控制台窗口地执行。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW)
}

/// 非 Windows 上没有这个标志，原样返回。
#[cfg(not(windows))]
pub fn hidden(command: &mut Command) -> &mut Command {
    command
}

/// 结束一个进程（按镜像名）。忽略失败 —— 调用方按需确认结果。
#[cfg(windows)]
pub fn terminate(name: &str) {
    let mut command = Command::new("taskkill");
    command.args(["/F", "/IM", name]);
    let _ = hidden(&mut command).output();
}

/// 非 Windows 上不存在 taskkill；保留空实现让 `cargo check` 能过。
#[cfg(not(windows))]
pub fn terminate(_name: &str) {}

/// 进程是否还在跑（按镜像名）。
///
/// `None` = 探测工具本身失败了 —— 与"不在跑"（`Some(false)`）必须区分：
/// 更新器不能因为 tasklist 跑不起来就把一次正常的更新判成失败。
pub fn running(name: &str) -> Option<bool> {
    let needle = name.to_ascii_lowercase();
    let mut command = Command::new("tasklist");
    command.args(["/FI", &format!("IMAGENAME eq {name}"), "/NH"]);
    hidden(&mut command)
        .output()
        .ok()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .to_ascii_lowercase()
                .contains(&needle)
        })
}

/// 停掉后台服务。
pub fn stop_service() {
    terminate(SERVICE_PROGRAM);
}

/// 结束除 `except` 以外的全部客户端进程。
///
/// 托盘「退出」要的是"干净地关掉"：本地服务是独立进程，不收掉就会留在后台
/// 占着 8765 —— 用户以为退出了，其实还在跑。调用者自己不在名单里，由调用处退出。
pub fn stop_all_except(except: &str) {
    for name in CLIENT_PROGRAMS.iter().filter(|name| **name != except) {
        terminate(name);
    }
}
