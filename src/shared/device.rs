//! 设备标识与会话令牌的本地持久化。
//!
//! 由原 `forckend/store.rs` 并入：登录窗合并进主控台后，设备凭据的读写不再属于
//! 某一个窗口，而是"谁登录谁写"的公共动作。

use std::{fs, io, path::Path, process::Command};

/// 每台客户端唯一的设备标识，持久化后不再变化。
///
/// 云端用它区分"同一账号的不同设备"，从而能够**单独撤销某一台**而不影响其他设备。
/// 文件不存在或为空时生成一个新的 v4 UUID 并落盘。
pub fn device_id(path: &Path) -> String {
    if let Ok(existing) = fs::read_to_string(path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    let generated = uuid::Uuid::new_v4().simple().to_string();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, &generated);
    generated
}

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

    let status = Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{identity}:(F)"))
        .arg("*S-1-5-18:(F)")
        .arg("*S-1-5-32-544:(F)")
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
    fn device_id_is_stable_across_calls() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("device.id");
        let first = device_id(&path);
        assert_eq!(first.len(), 32);
        assert_eq!(first, device_id(&path));
    }
}
