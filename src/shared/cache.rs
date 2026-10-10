//! 解密缓存探测：一个微信号的密钥"真的能用"与否。
//!
//! 缓存的真实位置是 `<key-dir>/cache/*.db`（见账号目录 config.json 的
//! `"decrypted_dir": "cache"`，相对密钥目录解析），老布局在
//! `<key-dir>/<wxid>/cache/` 下，一并认。
//!
//! 为什么它是"就绪"的必要条件：只有密钥、没有缓存时，导出程序读不到聊天库
//! 会一直空等（实测卡到云端 15 分钟超时），界面却写着「密钥就绪」。
//! 服务端心跳与主控台界面共用这一份判定。

use std::path::Path;

/// 解密缓存（聊天数据库）是否已经生成。
pub fn decrypted_cache_ready(accounts: &Path, wxid: &str) -> bool {
    [accounts.join("cache"), accounts.join(wxid).join("cache")]
        .iter()
        .any(|dir| has_db_file(dir))
}

fn has_db_file(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("db"))
    })
}
