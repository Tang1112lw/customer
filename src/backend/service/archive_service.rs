//! 聊天记录归档与上传。
//!
//! 「数据采集」闭环的上半段：客户端把聊天记录导成 Markdown，打包上传给云端。
//!
//! 只跑一个导出程序 `aicrm-export-new.exe`，它产出
//! `<wxid>/<联系人>/<日期>/messages.md`。云端把这份 Markdown 原样存盘、原样交给
//! Hermes 智能体分析（AI 输入格式 `aicrm-analysis-input/v3`），全链路一种格式：
//! 人工阅读用的和 AI 分析用的是同一份数据，不再有 JSON 中转。
//!
//! 归档包格式（云端 `archive.go` 的 `markdownBundle`）：
//!
//! ```json
//! {
//!   "schema_version": "aicrm-chat-archive/v2",
//!   "task_id": "<uuid>",
//!   "bootstrap": false,
//!   "exports": [{
//!     "chat": {"id": "…", "display_name": "…", "type": "contact"},
//!     "date_range": {"start": "2026-10-07", "end": "2026-10-09"},
//!     "days": [{"date": "2026-10-09", "message_count": 12, "markdown": "# 2026-10-09\n\n- **我**：…"}]
//!   }]
//! }
//! ```
//!
//! 会话 ID 取导出目录里的会话文件夹名：群聊是 `xxx@chatroom`，单聊是联系人名。
//! Markdown 里没有消息 ID，云端的分析去重因此按天（`day:YYYY-MM-DD`）记账。

use super::{ApiError, ApiResult, Service, option, state_service::UPLOADED, today};
use crate::backend::service::run_blocking;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};

/// 导出程序：产 `messages.md`，云端归档与 AI 分析都用它。
pub(crate) const ARCHIVE_PROGRAM: &str = super::export_service::EXPORT_PROGRAM;

/// 归档包 schema 版本，必须与云端 `archive.go` 的 `archiveSchemaVersion` 一致。
const ARCHIVE_SCHEMA: &str = "aicrm-chat-archive/v2";

/// 导出超时。聊天库大时这个程序会跑很久。
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(3600);

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveRequest {
    /// 云端任务 ID。会写进归档包，云端会校验它与 URL 上的一致。
    pub task_id: String,
    /// 云端任务执行令牌。上传接口要求 `X-Execution-Token`。
    pub execution_token: String,
    /// 用户 JWT。上传接口除了执行令牌，还要 `Authorization: Bearer` 才能定位到
    /// 「这个任务属于谁」—— 设备令牌在这条路径上不被接受（实测只带执行令牌会 401）。
    /// 云端下发采集命令时需要现签发一个短时令牌捎过来：`accessToken` 只有 2 小时
    /// 有效期，不能长期存在客户机器上。
    pub user_token: String,
    /// 上传地址，形如 `https://云端/api/tasks/<taskId>/upload`。
    pub upload_url: String,
    pub wxid: Option<String>,
    pub person: Option<String>,
    pub day: Option<u32>,
    pub start: Option<String>,
    pub end: Option<String>,
}

impl Service {
    /// 导出 → 合并归档 → 上传。返回云端响应。
    pub async fn export_and_upload(&self, mut request: ArchiveRequest) -> ApiResult<Value> {
        if request.task_id.trim().is_empty() || request.execution_token.trim().is_empty() {
            return Err(ApiError::bad("task_id 与 execution_token 不能为空"));
        }
        if request.user_token.trim().is_empty() {
            return Err(ApiError::bad(
                "user_token 不能为空：上传接口除执行令牌外还需要用户身份",
            ));
        }
        if !request.upload_url.starts_with("http://") && !request.upload_url.starts_with("https://")
        {
            return Err(ApiError::bad("upload_url 必须是 http(s) 地址"));
        }

        // 幂等：这个任务已经传成功过就直接复用结果。
        // 云端正常不会重发同一个 task_id，但客户端崩溃重启、命令重复投递时会；
        // 大聊天库导出动辄一小时，没必要为同一批消息付第二次。
        if let Some(record) = self.state.upload(&request.task_id) {
            if record.state == UPLOADED {
                return Ok(json!({
                    "skipped": true,
                    "task_id": request.task_id,
                    "wxid": record.wxid,
                    "name": record.name,
                }));
            }
        }

        // 水位线记在机主账号上；归档包里的 chat.id 是会话对方的，不能拿来当自己。
        let owner = request
            .wxid
            .clone()
            .filter(|wxid| !wxid.is_empty())
            .unwrap_or_else(|| self.self_wxid());

        // 云端没指定范围时，从上次成功采集的水位线接着采 —— 这才是增量采集。
        // 否则每次都是全量重采：几万条消息反复上传、反复分析，客户等得没道理。
        // 起点取上次那天而不是次日，多采一天由云端去重兜住，少采一天就是永久缺口。
        if request.start.is_none() && request.end.is_none() && request.day.is_none() {
            if let Some(mark) = self.state.watermark(&owner) {
                request.start = Some(mark.date);
            }
        }

        let work = self
            .data
            .join("jobs")
            .join(format!("archive-{}", request.task_id));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).map_err(|_| ApiError::io())?;

        // 导出 Markdown：JSON 给流水线、Markdown 给人看的老分工已经取消，
        // 现在一份 Markdown 同时满足人工查阅与 AI 分析。
        self.run_archive_program(ARCHIVE_PROGRAM, &work, &request)
            .await?;
        let _ = work; // 导出程序把结果写进 work，云端按会话/日期拆开

        // ② 收集所有 messages.json，合并成一个归档包
        let bundle = tokio::task::spawn_blocking({
            let root = work.clone();
            let task_id = request.task_id.clone();
            move || build_archive(&root, &task_id)
        })
        .await
        .map_err(|_| ApiError::io())?
        .map_err(|message| ApiError::helper(message))?;

        // ③ 上传
        let payload =
            serde_json::to_vec(&bundle).map_err(|_| ApiError::helper("归档序列化失败"))?;
        let upload_url = request.upload_url.clone();
        let token = request.execution_token.clone();
        let user_token = request.user_token.clone();
        let task_id = request.task_id.clone();
        let mut outcome =
            run_blocking(move || upload_archive(&upload_url, &token, &user_token, &task_id, payload))
                .await;

        // ④ 留痕。这一步以前什么都不记：上传失败后目录被下一次任务清掉，
        //    那批数据就永久丢了，而且没人知道丢过。
        let name = self.state.known_name(&owner).unwrap_or_default();
        match &mut outcome {
            Ok(value) => {
                self.state.mark_upload(&request.task_id, true, "", &owner, &name);
                if !owner.is_empty() && !name.is_empty() {
                    self.state.remember_name(&owner, &name);
                }
                // 云端响应未必带回 wxid，但回传结果的校验需要它，缺了补上
                if let Value::Object(map) = value {
                    map.entry("wxid").or_insert_with(|| json!(owner));
                    map.entry("name").or_insert_with(|| json!(name));
                }
                // 水位线只在成功后推进：失败了也推，中间那几天就再也采不到。
                let until = request
                    .end
                    .clone()
                    .filter(|end| !end.is_empty())
                    .unwrap_or_else(today);
                self.state.mark_collected(&owner, &until);
            }
            Err(error) => {
                self.state
                    .mark_upload(&request.task_id, false, &error.message(), "", "");
            }
        }

        // ⑤ 顺手清理过期产物。以前这个目录只增不减，用几个月就能吃光磁盘。
        self.state.cleanup_jobs(&self.data.join("jobs"));

        outcome
    }

    /// 跑导出程序。参数与 `aicrm-export-new.exe` 的命令行一致。
    async fn run_archive_program(
        &self,
        program: &str,
        dir: &Path,
        request: &ArchiveRequest,
    ) -> ApiResult<Value> {
        let _lock = self.gate.read().await;
        let mut args: Vec<String> = vec![
            "--key-dir".into(),
            self.keys.display().to_string(),
            "--export-dir".into(),
            dir.display().to_string(),
            "--task-id".into(),
            request.task_id.clone(),
            "--json".into(),
        ];
        option(&mut args, "--wxid", request.wxid.clone())?;
        option(&mut args, "--person", request.person.clone())?;
        option(&mut args, "--start", request.start.clone())?;
        option(&mut args, "--end", request.end.clone())?;
        if let Some(day) = request.day {
            args.extend(["--day".into(), day.to_string()]);
        }
        self.run(program, args, ARCHIVE_TIMEOUT).await
    }
}

/// 把导出目录里的 `messages.md` 合并成一个 `aicrm-chat-archive/v2` 包。
///
/// 目录结构由导出程序决定：`<导出目录>/<wxid>/<会话>/<YYYY-MM-DD>/messages.md`。
/// 会话文件夹名就是会话标识（群聊为 `xxx@chatroom`，单聊为联系人名），云端按它分会话。
fn build_archive(root: &Path, task_id: &str) -> Result<Value, String> {
    // 先定位 wxid 层：优先用任务里的 wxid，否则取导出目录下唯一的子目录。
    let account_root = locate_account_root(root)?;
    let mut conversations: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for entry in
        fs::read_dir(&account_root).map_err(|error| format!("读取 {} 失败：{error}", account_root.display()))?
    {
        let entry = entry.map_err(|error| format!("读取会话目录失败：{error}"))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(conversation) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        for day_entry in
            fs::read_dir(&path).map_err(|error| format!("读取 {} 失败：{error}", path.display()))?
        {
            let day_entry = day_entry.map_err(|error| format!("读取日期目录失败：{error}"))?;
            let day_path = day_entry.path();
            if !day_path.is_dir() {
                continue;
            }
            let Some(date) = day_path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| is_iso_date(name))
            else {
                continue;
            };
            let markdown_path = day_path.join("messages.md");
            if !markdown_path.is_file() {
                continue;
            }
            let markdown = fs::read_to_string(&markdown_path)
                .map_err(|error| format!("读取 {} 失败：{error}", markdown_path.display()))?;
            if markdown.trim().is_empty() {
                continue;
            }
            conversations
                .entry(conversation.to_string())
                .or_default()
                .insert(date.to_string(), markdown);
        }
    }
    if conversations.is_empty() {
        return Err(format!(
            "导出目录里没有找到任何 messages.md：{}",
            account_root.display()
        ));
    }

    let exports: Vec<Value> = conversations
        .into_iter()
        .map(|(conversation, days)| {
            let dates: Vec<String> = days.keys().cloned().collect();
            let start = dates.first().cloned().unwrap_or_default();
            let end = dates.last().cloned().unwrap_or_default();
            let day_values: Vec<Value> = days
                .into_iter()
                .map(|(date, markdown)| {
                    json!({
                        "date": date,
                        "message_count": count_markdown_messages(&markdown),
                        "markdown": markdown,
                    })
                })
                .collect();
            json!({
                "chat": {
                    // Markdown 里没有会话 ID，用文件夹名当 ID；群聊能据 @chatroom 判定类型。
                    "id": conversation,
                    "display_name": conversation,
                    "type": if conversation.ends_with("@chatroom") { "group" } else { "contact" },
                },
                "date_range": {"start": start, "end": end},
                "days": day_values,
            })
        })
        .collect();

    Ok(json!({
        "schema_version": ARCHIVE_SCHEMA,
        "task_id": task_id,
        // 增量采集：只提交本次导出的范围，不做全量整理
        "bootstrap": false,
        "exports": exports,
    }))
}

/// 定位导出目录下的账号层目录。
fn locate_account_root(root: &Path) -> Result<std::path::PathBuf, String> {
    let direct = root.join("messages.md");
    if !direct.is_file() {
        let entries = fs::read_dir(root)
            .map_err(|error| format!("读取 {} 失败：{error}", root.display()))?;
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                return Ok(entry.path());
            }
        }
    }
    Ok(root.to_path_buf())
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

/// 数 Markdown 里的消息行（`- **发言人**：正文`）。与云端 `CountMarkdownMessages` 一致。
fn count_markdown_messages(markdown: &str) -> usize {
    markdown
        .lines()
        .filter(|line| line.trim_start().starts_with("- "))
        .count()
}

/// 以 multipart/form-data 上传归档包。字段名必须是 `file`。
fn upload_archive(
    url: &str,
    token: &str,
    user_token: &str,
    task_id: &str,
    payload: Vec<u8>,
) -> ApiResult<Value> {
    const BOUNDARY: &str = "----aicrmarchive7f3a9c";
    let filename = format!("{task_id}.json");
    let mut body: Vec<u8> = Vec::with_capacity(payload.len() + 512);
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/json\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&payload);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    let agent: ureq::Agent = ureq::config::Config::builder()
        // 大聊天库上传慢，给足时间
        .timeout_global(Some(Duration::from_secs(600)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .post(url)
        .header("X-Execution-Token", token)
        .header("Authorization", &format!("Bearer {user_token}"))
        .header(
            "Content-Type",
            &format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .send(body.as_slice())
        .map_err(|error| ApiError::helper(format!("上传失败：{error}")))?;
    let status = response.status();
    let text = response
        .body_mut()
        .read_to_string()
        .unwrap_or_else(|_| String::new());
    let parsed: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
    if !status.is_success() {
        let message = parsed
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("云端拒绝上传");
        return Err(ApiError::helper(format!("HTTP {status}：{message}")));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_bundle_matches_cloud_schema() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("wxid_demo").join("张三").join("2026-10-05");
        fs::create_dir_all(&chat).unwrap();
        fs::write(
            chat.join("messages.json"),
            r#"{"schema_version":"aicrm-chat/v1","chat":{"display_name":"张三","id":"wxid_zhangsan","type":"contact"},"date_range":{"start":"2026-10-05","end":"2026-10-05"},"messages":[{"sequence":1}]}"#,
        )
        .unwrap();
        // Markdown 与媒体文件都不该被收进归档
        fs::write(chat.join("messages.md"), "# 2026-10-05\n").unwrap();
        fs::write(chat.join("voice.silk"), b"x").unwrap();

        let bundle = build_archive(dir.path(), "task-1").unwrap();
        assert_eq!(bundle["schema_version"], ARCHIVE_SCHEMA);
        assert_eq!(bundle["task_id"], "task-1");
        assert_eq!(bundle["bootstrap"], false);
        let exports = bundle["exports"].as_array().unwrap();
        assert_eq!(exports.len(), 1, "只应收集 messages.json");
        assert_eq!(exports[0]["chat"]["id"], "wxid_zhangsan");
        assert_eq!(exports[0]["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn missing_chat_field_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("messages.json"), r#"{"messages":[]}"#).unwrap();
        let error = build_archive(dir.path(), "task-1").unwrap_err();
        assert!(error.contains("缺少 chat"), "实际：{error}");
    }

    #[test]
    fn empty_directory_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(build_archive(dir.path(), "task-1").is_err());
    }
}
