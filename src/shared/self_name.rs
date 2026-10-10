//! 从本地导出里找机主自己的微信号（昵称）。
//!
//! 导出目录形状是 `<root>/<wxid>/<会话>/<日期>/messages.json`，每条消息的
//! `sender.is_self == true` 表示是机主发的，其 `sender.display_name` 就是昵称。
//! 深度优先翻到第一份能给出昵称的导出为止；**只翻 20 份文件** —— 心跳每 20 秒
//! 跑一次，这个开销必须兜得住。找不到返回 `None`，界面回落显示 wxid。
//!
//! 服务端心跳与主控台界面共用这一份：以前两边各写一份，注释里还写着
//! "改的时候两边都要改" —— 现在不用了。

use std::path::Path;

/// 翻导出目录找机主昵称；找不到返回 `None`（不是空串）。
pub fn self_name_from_exports(root: &Path) -> Option<String> {
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
    let raw = std::fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
    for message in value.get("messages")?.as_array()? {
        let sender = message.get("sender")?;
        if sender.get("is_self") == Some(&serde_json::Value::Bool(true)) {
            let name = sender.get("display_name").and_then(|value| value.as_str());
            if let Some(name) = name.filter(|name| !name.is_empty()) {
                return Some(name.to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_name_comes_from_own_messages() {
        let dir = tempfile::tempdir().unwrap();
        let day = dir.path().join("wxid_test").join("张三").join("2026-10-05");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            day.join("messages.json"),
            r#"{"messages":[
                {"sender":{"is_self":false,"display_name":"张三"}},
                {"sender":{"is_self":true,"display_name":"flow state"}}]}"#,
        )
        .unwrap();
        assert_eq!(
            self_name_from_exports(dir.path()).as_deref(),
            Some("flow state")
        );
    }

    #[test]
    fn missing_exports_yield_no_name() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(self_name_from_exports(dir.path()), None);
    }
}
