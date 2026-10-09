//! 本地采集状态：水位线、上传记录、机主身份。
//!
//! # 为什么需要这一层
//!
//! 客户端原本是"纯执行器"：云端说采哪天就采哪天，传完即结束，什么都不记。
//! 于是有三件事谁都不知道：
//!
//! - 上次采到哪天 —— 云端不下发日期就只能全量重采；
//! - 上次传成功没有 —— 上传失败后归档包被下一次任务直接删掉，数据永久丢失；
//! - 这台机器的微信号叫什么 —— 只能每次心跳翻 20 份导出的 JSON 去猜。
//!
//! 这一层把三件事落到 `<data>/state/` 下的 JSON 文件里，客户端因此有了记忆：
//!
//! - `watermarks.json` —— 每个 wxid 上次成功采集到哪天。云端不下发日期时，
//!   客户端自己从断点接着采，这才是真正的增量采集。
//! - `uploads.json` —— 每个任务的上传结果。成功的任务不再重复上传；
//!   失败的任务保留归档包，可以重传。
//! - `identity.json` —— 机主微信昵称缓存。缓存之后心跳不再扫盘，
//!   导出目录也可以安全清理。
//!
//! # 为什么是 JSON 而不是 SQLite
//!
//! 水位线几十条、上传记录几百条，规模完全够用；项目刻意保持轻依赖
//! （`ureq` 而不是 `reqwest` 就是出于同样考虑），为了这点数据引入 rusqlite
//! 不划算。真到索引撑不住那天再换。
//!
//! 写入一律走「先写 `.tmp` 再 rename」：客户端常驻后台，写到一半断电或
//! 被杀不能留下半个 JSON。

use std::{
    collections::HashMap,
    fs,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::now_rfc3339;

/// 采集水位线文件名。
const WATERMARKS: &str = "watermarks.json";
/// 上传记录文件名。
const UPLOADS: &str = "uploads.json";
/// 机主身份文件名。
const IDENTITY: &str = "identity.json";

/// 上传成功后，导出产物保留天数。留几天是为了让人能翻 Markdown 自查。
const KEEP_SUCCEEDED_DAYS: u64 = 7;
/// 上传失败的任务保留天数。失败的包要留着重传，删早了数据就没了。
const KEEP_FAILED_DAYS: u64 = 30;

/// 上传状态：已成功。
pub const UPLOADED: &str = "uploaded";
/// 上传状态：失败，等待重传。
pub const FAILED: &str = "failed";

/// 一次成功采集留下的水印。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Watermark {
    /// 上次成功采集到的日期，`YYYY-MM-DD`。
    pub date: String,
    /// 采集完成时刻，RFC3339。
    pub collected_at: String,
}

/// 一个采集任务的上传结果。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Upload {
    /// [`UPLOADED`] 或 [`FAILED`]。
    pub state: String,
    /// 尝试次数。第一次就是 1。
    #[serde(default)]
    pub attempts: u32,
    /// 失败原因。成功时为空串。
    #[serde(default)]
    pub error: String,
    /// 采集账号的 wxid。成功后留着，任务被重复下发时能直接复用结果，
    /// 不必再跑一次动辄一小时的导出。
    #[serde(default)]
    pub wxid: String,
    /// 上面的微信昵称。
    #[serde(default)]
    pub name: String,
    /// 最后一次更新时刻，RFC3339。
    pub updated_at: String,
}

/// 采集状态存储。
pub struct State {
    dir: PathBuf,
}

impl State {
    /// 打开（并在需要时创建）状态目录。
    pub fn new(dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// 读一个状态文件。文件不存在、内容坏了都回落到默认值 ——
    /// 状态丢了最多重采一次，不能让后台服务起不来。
    fn load<T: Default + DeserializeOwned>(&self, file: &str) -> T {
        let path = self.dir.join(file);
        let Ok(text) = fs::read_to_string(path) else {
            return T::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// 原子写：先落临时文件再改名。
    ///
    /// 失败只在 stderr 打一行：状态写不进去值得让人在日志里看见，
    /// 但不该让一次本来成功的采集变成失败。
    fn save<T: Serialize>(&self, file: &str, value: &T) -> Result<(), String> {
        let path = self.dir.join(file);
        let temp = self.dir.join(format!("{file}.tmp"));
        let text =
            serde_json::to_string_pretty(value).map_err(|error| format!("序列化 {file} 失败：{error}"))?;
        fs::write(&temp, text).map_err(|error| format!("写入 {file} 失败：{error}"))?;
        fs::rename(&temp, &path).map_err(|error| format!("替换 {file} 失败：{error}"))
    }

    /// 取某个微信账号上次成功采集到的水位线。
    pub fn watermark(&self, wxid: &str) -> Option<Watermark> {
        if wxid.is_empty() {
            return None;
        }
        self.load::<HashMap<String, Watermark>>(WATERMARKS)
            .get(wxid)
            .cloned()
    }

    /// 记录一次成功采集。
    ///
    /// **只有成功才记**：失败了也推进水位线，中间那几天就永远采不到了。
    pub fn mark_collected(&self, wxid: &str, date: &str) {
        if wxid.is_empty() || date.is_empty() {
            return;
        }
        let mut all = self.load::<HashMap<String, Watermark>>(WATERMARKS);
        all.insert(
            wxid.to_owned(),
            Watermark {
                date: date.to_owned(),
                collected_at: now_rfc3339(),
            },
        );
        if let Err(error) = self.save(WATERMARKS, &all) {
            eprintln!("写水位线失败：{error}");
        }
    }

    /// 查一个任务的上传结果。已成功的任务应当直接跳过，不重复上传。
    pub fn upload(&self, task_id: &str) -> Option<Upload> {
        if task_id.is_empty() {
            return None;
        }
        self.load::<HashMap<String, Upload>>(UPLOADS)
            .get(task_id)
            .cloned()
    }

    /// 记录一次上传结果。`attempts` 自增，失败原因一并留下方便排查。
    ///
    /// 成功时把 `wxid` / `name` 留下：同一个任务被重复下发时可以直接复用，
    /// 不必再跑一次导出（大聊天库动辄一小时）。
    pub fn mark_upload(&self, task_id: &str, ok: bool, error: &str, wxid: &str, name: &str) {
        if task_id.is_empty() {
            return;
        }
        let mut all = self.load::<HashMap<String, Upload>>(UPLOADS);
        let previous = all.get(task_id);
        let attempts = previous.map(|record| record.attempts).unwrap_or(0) + 1;
        all.insert(
            task_id.to_owned(),
            Upload {
                state: if ok { UPLOADED.to_owned() } else { FAILED.to_owned() },
                attempts,
                error: if ok { String::new() } else { error.to_owned() },
                wxid: if ok { wxid.to_owned() } else { String::new() },
                name: if ok { name.to_owned() } else { String::new() },
                updated_at: now_rfc3339(),
            },
        );
        if let Err(error) = self.save(UPLOADS, &all) {
            eprintln!("写上传记录失败：{error}");
        }
    }

    /// 还没传成功的任务数。给心跳上报用，网页端能显示"有 N 个任务待重传"。
    pub fn pending_uploads(&self) -> usize {
        self.load::<HashMap<String, Upload>>(UPLOADS)
            .values()
            .filter(|record| record.state != UPLOADED)
            .count()
    }

    /// 最近一次成功采集的时刻。给心跳上报用。
    pub fn last_collected_at(&self) -> Option<String> {
        self.load::<HashMap<String, Watermark>>(WATERMARKS)
            .values()
            .map(|mark| mark.collected_at.clone())
            .max()
            .filter(|at| !at.is_empty())
    }

    /// 记住机主的微信昵称。
    ///
    /// 昵称只能从导出的 `messages.json` 里翻（`sender.is_self == true` 的那条
    /// `display_name`）。缓存之后心跳不再扫盘，导出目录也能放心清理。
    pub fn remember_name(&self, wxid: &str, name: &str) {
        if wxid.is_empty() || name.is_empty() {
            return;
        }
        let mut all = self.load::<HashMap<String, String>>(IDENTITY);
        if all.get(wxid).is_some_and(|known| known == name) {
            return;
        }
        all.insert(wxid.to_owned(), name.to_owned());
        if let Err(error) = self.save(IDENTITY, &all) {
            eprintln!("写机主昵称失败：{error}");
        }
    }

    /// 取已记住的机主昵称。没记住返回 `None`，调用方回落到 wxid。
    pub fn known_name(&self, wxid: &str) -> Option<String> {
        if wxid.is_empty() {
            return None;
        }
        self.load::<HashMap<String, String>>(IDENTITY)
            .get(wxid)
            .cloned()
            .filter(|name| !name.is_empty())
    }

    /// 清理过期的导出产物。
    ///
    /// 以前这个目录只增不减，客户用几个月磁盘就被吃光。现在成功的留 7 天，
    /// 失败的留 30 天（要留着重传）。目录名形如 `archive-<task_id>`，
    /// 认不出来的一律按最保守的 30 天处理。
    pub fn cleanup_jobs(&self, jobs: &Path) {
        let uploads = self.load::<HashMap<String, Upload>>(UPLOADS);
        let Ok(entries) = fs::read_dir(jobs) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let task_id = name.strip_prefix("archive-").unwrap_or(name.as_str());
            let keep = match uploads.get(task_id) {
                Some(record) if record.state == UPLOADED => KEEP_SUCCEEDED_DAYS,
                _ => KEEP_FAILED_DAYS,
            };
            if older_than(&path, keep) {
                eprintln!("清理过期导出产物：{}", path.display());
                let _ = fs::remove_dir_all(&path);
            }
        }
    }
}

/// 目录的最后修改时间是否早于 `days` 天前。
///
/// 用文件系统自己的 mtime，不去解析 JSON 里的时间字符串 —— 少一处时区与格式出错的可能。
fn older_than(path: &Path, days: u64) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|meta| meta.modified()) else {
        return false;
    };
    modified
        .elapsed()
        .map(|age| age > Duration::from_secs(days * 86_400))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (tempfile::TempDir, State) {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path().join("state")).unwrap();
        (dir, state)
    }

    /// 把目录的 mtime 推回 `days` 天前。
    ///
    /// Windows 上打开目录句柄必须带 `FILE_FLAG_BACKUP_SEMANTICS`，否则
    /// `CreateFile` 直接拒绝访问（错误码 5）；POSIX 侧照常打开即可。
    fn age_back(path: &Path, days: u64) {
        let past = std::time::SystemTime::now() - Duration::from_secs(days * 86_400);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                // FILE_WRITE_ATTRIBUTES：只改时间戳，不申请写内容
                .access_mode(0x0100)
                // FILE_FLAG_BACKUP_SEMANTICS：允许打开目录句柄
                .custom_flags(0x0200_0000)
                .open(path)
                .expect("打开目录句柄失败")
                .set_modified(past)
                .expect("回拨 mtime 失败");
        }
        #[cfg(not(windows))]
        {
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .expect("打开目录句柄失败")
                .set_modified(past)
                .expect("回拨 mtime 失败");
        }
    }

    #[test]
    fn missing_files_fall_back_to_defaults() {
        let (_guard, state) = state();
        assert!(state.watermark("wxid_x").is_none());
        assert!(state.upload("t-1").is_none());
        assert!(state.known_name("wxid_x").is_none());
        assert_eq!(state.pending_uploads(), 0);
        assert!(state.last_collected_at().is_none());
    }

    #[test]
    fn watermark_round_trips() {
        let (_guard, state) = state();
        state.mark_collected("wxid_x", "2026-10-08");
        let mark = state.watermark("wxid_x").expect("水位线应当写回");
        assert_eq!(mark.date, "2026-10-08");
        assert!(mark.collected_at.ends_with('Z'));
    }

    #[test]
    fn empty_wxid_is_never_recorded() {
        let (_guard, state) = state();
        state.mark_collected("", "2026-10-08");
        assert!(state.watermark("").is_none());
    }

    #[test]
    fn successful_upload_is_idempotent_and_failure_is_pending() {
        let (_guard, state) = state();
        state.mark_upload("t-1", true, "", "wxid_x", "张三");
        assert_eq!(state.upload("t-1").unwrap().state, UPLOADED);
        assert_eq!(state.upload("t-1").unwrap().wxid, "wxid_x");
        assert_eq!(state.pending_uploads(), 0);

        state.mark_upload("t-2", false, "网络中断", "", "");
        assert_eq!(state.upload("t-2").unwrap().state, FAILED);
        assert_eq!(state.pending_uploads(), 1);
        // 重传会累加尝试次数，方便判断"卡了多久"
        state.mark_upload("t-2", false, "网络中断", "", "");
        assert_eq!(state.upload("t-2").unwrap().attempts, 2);
    }

    #[test]
    fn failed_upload_does_not_keep_identity() {
        // 失败记录里的 wxid 是空的：复用结果只能用成功那次的
        let (_guard, state) = state();
        state.mark_upload("t-1", false, "超时", "wxid_x", "张三");
        assert_eq!(state.upload("t-1").unwrap().wxid, "");
    }

    #[test]
    fn corrupted_file_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path().join("state")).unwrap();
        fs::write(state.dir.join(WATERMARKS), "{ not json").unwrap();
        assert!(state.watermark("wxid_x").is_none());
    }

    #[test]
    fn identity_cache_round_trips() {
        let (_guard, state) = state();
        state.remember_name("wxid_x", "张三");
        assert_eq!(state.known_name("wxid_x").as_deref(), Some("张三"));
        // 空昵称不该覆盖已有值
        state.remember_name("wxid_x", "");
        assert_eq!(state.known_name("wxid_x").as_deref(), Some("张三"));
    }

    #[test]
    fn cleanup_keeps_fresh_and_drops_stale() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path().join("state")).unwrap();
        let jobs = dir.path().join("jobs");
        let fresh = jobs.join("archive-fresh");
        let stale = jobs.join("archive-stale");
        fs::create_dir_all(&fresh).unwrap();
        fs::create_dir_all(&stale).unwrap();

        state.mark_upload("stale", true, "", "wxid_x", "");
        state.mark_upload("fresh", true, "", "wxid_x", "");
        // 把 stale 的 mtime 推回 10 天前，超过成功任务的 7 天保留期
        age_back(&stale, 10);

        state.cleanup_jobs(&jobs);
        assert!(!stale.exists(), "过期的应当被清掉");
        assert!(fresh.exists(), "没过期的必须留着");
    }

    #[test]
    fn cleanup_keeps_failed_jobs_longer() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path().join("state")).unwrap();
        let jobs = dir.path().join("jobs");
        let failed = jobs.join("archive-broken");
        fs::create_dir_all(&failed).unwrap();

        state.mark_upload("broken", false, "超时", "", "");
        age_back(&failed, 10);

        state.cleanup_jobs(&jobs);
        assert!(failed.exists(), "失败的任务要留着重传，10 天不能删");
    }
}
