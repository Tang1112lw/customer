//! 登录凭据（会话令牌、刷新令牌、账号名）的本地持久化。
//!
//! 这里是**账号**的凭据，不是设备的：2026-10-10 取消设备绑定后，客户端不再有
//! 设备令牌，也没有配对流程 —— 云端只认「这个账号有没有客户端登录着」。
//! 客户端实例标识（`client.id`）在 `main.rs` 里生成，只用于区分同一账号的多台电脑，
//! 不是凭据，也不在这里管。
//!
//! 由原 `forckend/store.rs` 并入：登录窗合并进主控台后，凭据的读写不再属于
//! 某一个窗口，而是"谁登录谁写"的公共动作。

use std::{fs, io, path::Path, process::Command};

/// 写入会话令牌，并尽力把文件权限收紧到"仅当前用户 + SYSTEM + 管理员"。
///
/// 权限收紧是 best-effort：失败只返回警告，不阻断登录 —— 但调用方应当把警告显示出来，
/// 因为令牌文件可被本机其他账户读取时，任何人拿到它就能调走全部聊天记录。
pub fn save_token(path: &Path, token: &str) -> io::Result<Option<String>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, token)?;
    Ok(harden(path))
}

/// 用 icacls 断掉继承并只授权给当前用户 / SYSTEM / Administrators。
/// 用 SID 而不是账户名，避免中文系统或重命名导致授权错位。
fn harden(path: &Path) -> Option<String> {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|name| !name.is_empty())?;
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    let identity = if domain.is_empty() {
        user
    } else {
        format!("{domain}\\{user}")
    };

    let mut command = Command::new("icacls");
    command
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{identity}:(F)"))
        .arg("*S-1-5-18:(F)")
        .arg("*S-1-5-32-544:(F)");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // 不留控制台窗口地执行：登录写令牌时会调到这里，黑窗在客户桌面上
        // 一闪而过就是"这软件在偷偷跑命令"（实测被这么问过）。
        // 这里刻意不调 shared::process::hidden：共享文件之间零交叉依赖
        // （见 shared/mod.rs 约定），credentials 可能被单独按需引入。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let status = command
        .output();

    match status {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(format!(
            "令牌已保存，但收紧文件权限失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(error) => Some(format!("令牌已保存，但无法执行 icacls：{error}")),
    }
}

/// 写入账号名。登录成功拿到 `display_name` 后调用。
///
/// 只写不读 —— 读的那一头在 console.rs 的快照里，它直接读这个文件。
pub fn save_account(path: &Path, name: &str) -> io::Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_name_is_trimmed_and_empty_is_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("account.name");
        save_account(&path, "  张三  ").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "张三");
        // 空名字不该把已有文件清掉（"未登录"的判定依赖它）
        save_account(&path, "   ").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "张三");
    }

    #[test]
    fn token_is_written_verbatim() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("api.token");
        // 权限收紧是 best-effort（返回警告不报错），这里只断言内容落盘
        let _ = save_token(&path, "tok-123").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "tok-123");
    }
}
