//! 聊天记录归档与上传。
//!
//! 「数据采集」闭环的上半段：客户端把聊天记录导成 Markdown，打包上传给云端。
//!
//! 只跑一个导出程序 `aicrm-export-new.exe`，它产出
//! `<wxid>/<联系人>/<日期>/messages.md`，同一天的语音则落在 `<日期>/audio/*.silk`，
//! Markdown 里的 `[语音](audio/xxx.silk)` 就指向这些文件。云端把这份 Markdown
//! 原样存盘、原样交给 Hermes 智能体分析（AI 输入格式 `aicrm-analysis-input/v3`），
//! 全链路一种格式：人工阅读用的和 AI 分析用的是同一份数据，不再有 JSON 中转。
//!
//! 归档包格式（云端 `archive.go` 的 `markdownBundle`）：
//!
//! ```json
//! {
//!   "schema_version": "aicrm-chat-archive/v3",
//!   "task_id": "<uuid>",
//!   "bootstrap": false,
//!   "exports": [{
//!     "chat": {"id": "…", "display_name": "…", "type": "contact"},
//!     "date_range": {"start": "2026-10-07", "end": "2026-10-09"},
//!     "days": [{
//!       "date": "2026-10-09",
//!       "message_count": 12,
//!       "markdown": "# 2026-10-09\n\n- **我**：…",
//!       "media": [{
//!         "path": "audio/1_voice_1.silk",
//!         "size": 12345,
//!         "sha256": "…",
//!         "content_base64": "…"
//!       }]
//!     }]
//!   }]
//! }
//! ```
//!
//! `media[].path` 相对当天的日期目录，与 Markdown 里的 `](audio/…)` 链接同源：
//! 云端按 `<会话>/<日期>/<path>` 原样落盘，链接不用改写就仍然指得到文件。
//!
//! 会话 ID 取导出目录里的会话文件夹名：群聊是 `xxx@chatroom`，单聊是联系人名。
//! Markdown 里没有消息 ID，云端的分析去重因此按天（`day:YYYY-MM-DD`）记账。

use super::{ApiError, ApiResult, Service, option, state_service::UPLOADED, today};
use crate::backend::service::run_blocking;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// 导出程序：产 `messages.md`，云端归档与 AI 分析都用它。
pub(crate) const ARCHIVE_PROGRAM: &str = super::export_service::EXPORT_PROGRAM;

/// 归档包 schema 版本，必须与云端 `archive.go` 的 `archiveSchemaVersion` 一致。
///
/// v2 → v3：`days[]` 多带一个 `media`，把导出程序产在日期目录里的语音文件一起上传。
/// **两端必须同时升**：云端只认写死的那个版本号，客户端单方面升上去只会得到
/// 「schema_version 必须是 aicrm-chat-archive/v3」的 400，一次采集都发不出去。
const ARCHIVE_SCHEMA: &str = "aicrm-chat-archive/v3";

/// 单个媒体文件的大小上限（16MB）。
///
/// 微信语音实测 1KB–62KB，这是 250 倍以上的余量。超限的文件跳过并记日志，不让它
/// 进包：一个异常大的文件足以让 `serde_json::to_vec` 把内存吃光、或顶穿云端的
/// 请求体上限（`MAX_UPLOAD_BYTES` 默认 128MB，Base64 还要再涨三分之一），
/// 那会让**整包**归档失败；少一个文件只是那一条语音变成死链，代价小一个数量级。
const MAX_MEDIA_FILE_BYTES: u64 = 16 << 20;

/// **一天**的媒体总量上限（48MB）。
///
/// 单文件上限挡不住"一天攒很多"：导出程序除了语音还会往日期目录里放图片视频
/// （`images/*.dat`、`videos/*.mp4`、`files/`），一个带视频的日子 Base64 后就能
/// 顶穿云端请求体上限（`MAX_UPLOAD_BYTES` 默认 128MB，Base64 再涨三分之一），
/// 那会让**整包**归档失败 —— 一天的文字全部发不出去。超限后当天剩余媒体跳过
/// （文字照发，语音死链比整包丢失轻），并记日志说清跳过原因。
/// 实测全量微信语音约 20MB/账号，这个预算远在正常用量之上。
const MAX_DAY_MEDIA_BYTES: u64 = 48 << 20;

/// **整包**的媒体总量上限（64MB）。
///
/// 单文件与单天的闸都管不住"很多天"：一次采集动辄覆盖十来天，每天 48MB 就是
/// 近五百兆，Base64 后远超云端请求体上限（`MAX_UPLOAD_BYTES` 默认 128MB）——
/// 结果是整包上传失败，十多天的文字一起发不出去。所以第三道闸按包算：
/// 到顶后剩余媒体跳过（文字照发），并记日志说清跳过原因。
/// 64MB Base64 后约 85MB，加上 Markdown 与 JSON 结构仍在 128MB 之内有余量。
const MAX_BUNDLE_MEDIA_BYTES: u64 = 64 << 20;

/// 一天的媒体文件在归档包里的样子。
///
/// `path` 相对当天的日期目录（如 `audio/1_voice_1.silk`），且**一律用 `/` 分隔**：
/// Windows 上 `Path` 的显示是 `\`，而 Markdown 链接里写的是 `/`，两边必须一致，
/// 云端才能按 `<会话>/<日期>/<path>` 落盘并让链接继续有效。
/// `sha256` 让云端校验完整性，也便于同一天重复上传时判断内容有没有变。
struct MediaEntry {
    path: String,
    size: u64,
    sha256: String,
    content_base64: String,
}

/// 一天的归档内容：Markdown 正文加这一天的媒体文件。
struct DayPayload {
    markdown: String,
    media: Vec<MediaEntry>,
}

/// 导出超时。聊天库大时这个程序会跑很久。
// 必须小于云端僵尸任务阈值（backend/internal/aicrm/cleanup: StaleTaskTimeout=15 分钟），
// 否则导出超过 15 分钟时云端会先判死，客户端还在傻等，最后给出误导性原因。
// 留 ~3 分钟余量给上传与状态回报。实测踩过 30 分钟这种不自洽的配置。
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(720);

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

        // 微信没就绪就别开工：导出程序会一直等数据库，最后拖到云端 15 分钟超时，
        // 用户只看到「采集卡住」而拿不到原因（实测：有凭据但没有解密缓存，
        // 说明密钥没抓全 —— 微信进程在跑、却读不到库）。
        let probe = crate::backend::service::agent_service::WeChatProbe::detect(self);
        if !probe.online() {
            // 前置检查失败**也要回报云端**：否则任务会僵在「进行中」，
            // 同一账号 15 分钟内再也发不出新采集（唯一 running 约束）——
            // 实测踩过：微信没开时点采集，秒回失败但任务一直挂着。
            let message = format!(
                "本机微信未就绪：{}。请先打开并登录微信，再重新绑定以获取密钥。",
                probe.reason()
            );
            report_task_failure(&request, &message);
            return Err(ApiError::bad(message));
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
                // 水位线只给了起点，终点必须是今天：导出程序要求 --start / --end 成对出现，
                // 只给 start 会被它判成参数错误（实测「--start 与 --end 必须同时指定」）。
                request.start = Some(mark.date);
                request.end = Some(today());
            }
        }

        let work = self
            .data
            .join("jobs")
            .join(format!("archive-{}", request.task_id));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).map_err(|_| ApiError::io())?;

        // 先报一声「活儿收到了」。这一步之后如果再无音信，云端就能断定
        // 卡在导出上，而不是浏览器压根没调到本机。
        report_task_progress(
            &request,
            "exporting",
            10,
            &match (&request.start, &request.end) {
                (Some(start), Some(end)) => format!("已收到采集请求，开始导出 {start} 至 {end}"),
                _ => "已收到采集请求，开始导出全部历史".to_owned(),
            },
        );

        // 导出 Markdown：JSON 给流水线、Markdown 给人看的老分工已经取消，
        // 现在一份 Markdown 同时满足人工查阅与 AI 分析。
        // 导出失败同样要回报云端（理由见上面的前置检查）。
        if let Err(error) = self.run_archive_program(ARCHIVE_PROGRAM, &work, &request).await {
            report_task_failure(&request, error.message());
            return Err(error);
        }
        let _ = work; // 导出程序把结果写进 work，云端按会话/日期拆开

        // ② 收集所有 messages.md 与同目录的媒体文件，合并成一个归档包
        // 归档打包失败同样回报（理由见上面的前置检查）。
        let bundle = tokio::task::spawn_blocking({
            let root = work.clone();
            let task_id = request.task_id.clone();
            move || build_archive(&root, &task_id)
        })
        .await
        .map_err(|_| {
            report_task_failure(&request, "归档打包线程异常结束");
            ApiError::io()
        })?
        .map_err(|message| {
            report_task_failure(&request, &message);
            ApiError::helper(message)
        })?;

        let conversation_count = bundle
            .get("exports")
            .and_then(|value| value.as_array())
            .map(|items| items.len())
            .unwrap_or(0);
        let media_count = archive_media_count(&bundle);
        report_task_progress(
            &request,
            "exporting",
            35,
            // 语音数单独报：v3 起它会实打实占上传体积，看不到这个数，
            // 上传变慢时只能猜是不是网断了。
            &if media_count > 0 {
                format!(
                    "导出完成，已打包 {conversation_count} 个会话、{media_count} 个语音文件，准备上传"
                )
            } else {
                format!("导出完成，已打包 {conversation_count} 个会话，准备上传")
            },
        );

        // ③ 上传
        let payload =
            serde_json::to_vec(&bundle).map_err(|_| ApiError::helper("归档序列化失败"))?;
        let upload_url = request.upload_url.clone();
        let token = request.execution_token.clone();
        let user_token = request.user_token.clone();
        let task_id = request.task_id.clone();
        report_task_progress(
            &request,
            "uploading",
            60,
            &format!("正在上传归档包（{} 字节）", payload.len()),
        );

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
                    .mark_upload(&request.task_id, false, error.message(), "", "");
                // 失败了必须告诉云端。采集是在后台任务里跑的，浏览器早就不再等那个
                // 请求 —— 不上报，任务就一直停在 created，网页永远显示"采集中"，
                // 只能干等僵尸任务清理（15 分钟）把它放开。
                report_task_failure(&request, error.message());
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

/// 把导出目录里的 `messages.md` 与同目录的媒体文件合并成一个
/// `aicrm-chat-archive/v3` 包。
///
/// 目录结构由导出程序决定：`<导出目录>/<wxid>/<会话>/<YYYY-MM-DD>/messages.md`，
/// 语音在同级的 `audio/` 下。会话文件夹名就是会话标识（群聊为 `xxx@chatroom`，
/// 单聊为联系人名），云端按它分会话。
fn build_archive(root: &Path, task_id: &str) -> Result<Value, String> {
    let mut conversations: BTreeMap<String, BTreeMap<String, DayPayload>> = BTreeMap::new();
    let mut bundle_bytes: u64 = 0;
    collect_days(root, &mut conversations, &mut bundle_bytes)?;
    if conversations.is_empty() {
        return Err(format!(
            "导出目录里没有找到任何 messages.md：{}（导出程序可能没产出数据）",
            root.display()
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
                .map(|(date, payload)| {
                    let DayPayload { markdown, media } = payload;
                    let message_count = count_markdown_messages(&markdown);
                    let media_values: Vec<Value> = media
                        .iter()
                        .map(|entry| {
                            json!({
                                "path": entry.path,
                                "size": entry.size,
                                "sha256": entry.sha256,
                                "content_base64": entry.content_base64,
                            })
                        })
                        .collect();
                    json!({
                        "date": date,
                        "message_count": message_count,
                        "markdown": markdown,
                        "media": media_values,
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

/// 递归收集目录树里的 `<会话>/<日期>/messages.md`，按会话分组放回 `out`；
/// 同一天的媒体文件一并收进当天的 [`DayPayload`]。
///
/// 不能假设"导出目录下第一个子目录就是账号层"：导出程序会在同一层并排放
/// `.locks` 这类内部目录，靠名字猜或靠遍历顺序取第一个，命中与否全看运气。
/// 实测就因为先枚举到 `.locks`，一个会话都没找到 —— 导出的 Markdown 明明躺在
/// 旁边的 `<wxid>/<会话>/<日期>/messages.md` 里。所以改成按**结构特征**识别：
/// 目录名是日期且里面有 `messages.md`，就是一个有效日期；其余向下继续钻。
/// 以点开头的目录一律跳过（它们是程序的内部产物，不是聊天数据）。
///
/// `bundle_bytes` 是整包媒体预算的累计值（见 [`MAX_BUNDLE_MEDIA_BYTES`]），
/// 由调用方持有、在这里跨天累加：它决定了后面的日期还能不能收媒体。
fn collect_days(
    dir: &Path,
    out: &mut BTreeMap<String, BTreeMap<String, DayPayload>>,
    bundle_bytes: &mut u64,
) -> Result<(), String> {
    let entries =
        fs::read_dir(dir).map_err(|error| format!("读取 {} 失败：{error}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if is_iso_date(name) {
            let markdown_path = path.join("messages.md");
            if !markdown_path.is_file() {
                continue;
            }
            let markdown = fs::read_to_string(&markdown_path)
                .map_err(|error| format!("读取 {} 失败：{error}", markdown_path.display()))?;
            if markdown.trim().is_empty() {
                continue;
            }
            // 会话标识取日期目录的上一层；群聊能据 @chatroom 判定类型。
            let conversation = dir
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("未知会话")
                .to_string();
            let media = collect_media(&path, bundle_bytes)?;
            out.entry(conversation)
                .or_default()
                .insert(name.to_string(), DayPayload { markdown, media });
            continue;
        }
        collect_days(&path, out, bundle_bytes)?;
    }
    Ok(())
}

/// 收集一个日期目录里的媒体文件：除 `messages.md` 与 `messages.json` 之外的全部文件。
///
/// 递归而不是只认 `audio/`：导出程序今天把语音放在 `audio/`，以后加图片、视频也会是
/// 新目录；按「除正文外都要」来收，就不用每加一种媒体改一次这里。反过来，漏收的
/// 代价是 Markdown 里的链接变成死链，云端拿不到文件、语音转写无从谈起。
///
/// 三个上限各自管一件事：[`MAX_MEDIA_FILE_BYTES`] 挡单个异常大的文件，
/// [`MAX_DAY_MEDIA_BYTES`] 挡「一天攒太多」，[`MAX_BUNDLE_MEDIA_BYTES`]
/// （经 `bundle_bytes` 累计）挡「很多天攒更多」—— 后两者到顶就当天/整包收手，
/// 文字照发。顺序敏感：先量后读，读到内存里再判断就已经晚了。
/// 返回的条目按路径排序，让同一批文件每次打包出来的包字节一致，便于比对与复现。
fn collect_media(day_dir: &Path, bundle_bytes: &mut u64) -> Result<Vec<MediaEntry>, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    walk_media_files(day_dir, &mut files)?;
    files.sort();

    let mut entries: Vec<MediaEntry> = Vec::with_capacity(files.len());
    let mut day_bytes: u64 = 0;
    for file in &files {
        let relative = file
            .strip_prefix(day_dir)
            .map_err(|_| format!("媒体文件不在日期目录内：{}", file.display()))?;
        // 相对路径一律用 `/`：Windows 上是 `\`，而 Markdown 链接里写的是 `/`。
        let path = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        // 先看大小再读内容：否则一个异常大的文件会先被整个读进内存，跳过就失去意义了。
        let size = fs::metadata(file)
            .map_err(|error| format!("读取 {} 失败：{error}", file.display()))?
            .len();
        if size > MAX_MEDIA_FILE_BYTES {
            eprintln!(
                "归档跳过超大媒体文件 {}（{} 字节，上限 {} 字节）",
                file.display(),
                size,
                MAX_MEDIA_FILE_BYTES
            );
            continue;
        }
        if day_bytes + size > MAX_DAY_MEDIA_BYTES {
            eprintln!(
                "归档跳过媒体文件 {}：当天媒体总量已达 {} 字节（上限 {} 字节），当天文字仍会照常上传",
                file.display(),
                day_bytes,
                MAX_DAY_MEDIA_BYTES
            );
            continue;
        }
        if *bundle_bytes + size > MAX_BUNDLE_MEDIA_BYTES {
            eprintln!(
                "归档跳过媒体文件 {}：整包媒体总量已达 {} 字节（上限 {} 字节），后续日期的文字仍会照常上传",
                file.display(),
                *bundle_bytes,
                MAX_BUNDLE_MEDIA_BYTES
            );
            continue;
        }
        let bytes =
            fs::read(file).map_err(|error| format!("读取 {} 失败：{error}", file.display()))?;
        // 读完之后再核一次：导出目录是我们自己刚产出的，但"元数据说小、内容变大"
        // 这种竞态不该让整包上传失败。
        if bytes.len() as u64 > MAX_MEDIA_FILE_BYTES {
            eprintln!(
                "归档跳过超大媒体文件 {}（{} 字节，上限 {} 字节）",
                file.display(),
                bytes.len(),
                MAX_MEDIA_FILE_BYTES
            );
            continue;
        }
        day_bytes += bytes.len() as u64;
        *bundle_bytes += bytes.len() as u64;
        entries.push(MediaEntry {
            path,
            size: bytes.len() as u64,
            sha256: crate::backend::sha256::hex_digest(&bytes),
            content_base64: crate::backend::base64::encode(&bytes),
        });
    }
    Ok(entries)
}

/// 递归枚举 `dir` 下的文件，跳过目录、隐藏文件和两种已知的正文/中间产物。
fn walk_media_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        fs::read_dir(dir).map_err(|error| format!("读取 {} 失败：{error}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_media_files(&path, out)?;
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        // messages.md 已经在 markdown 字段里；messages.json 是导出程序的中间格式，
        // 云端自 v2 起就不收 JSON 了，带上去只是白占带宽。
        // 以点开头的是内部产物，与 collect_days 的判断保持一致。
        if name.starts_with('.') || name == "messages.md" || name == "messages.json" {
            continue;
        }
        out.push(path);
    }
    Ok(())
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

/// 数归档包里的媒体文件总数，只用于往进度消息里报一个数。
///
/// 结构不对时返回 0 而不是报错：这只是给人看的诊断信息，包是 `build_archive` 刚产出来的，
/// 形状由那个函数自己保证，出错也只可能是编程错误，不该在汇报时再把采集打断。
fn archive_media_count(bundle: &Value) -> usize {
    bundle
        .get("exports")
        .and_then(Value::as_array)
        .map(|exports| {
            exports
                .iter()
                .filter_map(|export| export.get("days").and_then(Value::as_array))
                .flatten()
                .filter_map(|day| day.get("media").and_then(Value::as_array))
                .map(|media| media.len())
                .sum()
        })
        .unwrap_or(0)
}

/// 上报一次采集进度。
///
/// 存在的理由：以前只有失败才联系云端，客户端一旦卡在导出里（导出程序 hang、
/// 微信没运行、数据目录读不到），云端看到的永远是「任务已创建」—— 分不清到底是
/// 浏览器没调到本机、还是导出卡死了，只能靠人到机器上翻目录。
/// 阶段与进度走云端已有白名单（checking_module / downloading / exporting / uploading，0-75）。
///
/// 上报失败只打日志：进度是给人看的诊断信息，不能因为它把真正的采集打断。
fn report_task_progress(request: &ArchiveRequest, stage: &str, progress: i32, message: &str) {
    let Some(base) = request.upload_url.strip_suffix("/upload") else {
        return;
    };
    let status_url = format!("{base}/status");
    let payload = json!({
        "status": "running",
        "stage": stage,
        "progress": progress,
        "message": message,
    })
    .to_string();
    let sent = ureq::post(&status_url)
        .header("X-Execution-Token", &request.execution_token)
        .header("Authorization", &format!("Bearer {}", request.user_token))
        .header("Content-Type", "application/json")
        .send(payload.as_bytes());
    if let Err(error) = sent {
        eprintln!("采集进度上报失败（{error}）：{message}");
    }
}

/// 通知云端「这次采集失败了」。
///
/// 状态接口与上传接口同源：`.../api/tasks/<id>/upload` → `.../api/tasks/<id>/status`。
/// 上报失败只打日志 —— 此时已经无可挽回，不能让它掩盖真正的采集失败原因。
fn report_task_failure(request: &ArchiveRequest, message: &str) {
    let Some(status_url) = request.upload_url.strip_suffix("/upload") else {
        return;
    };
    let status_url = format!("{status_url}/status");
    let message = if message.is_empty() {
        "客户端采集失败（原因未知）".to_string()
    } else {
        message.chars().take(1000).collect()
    };
    let report = || -> Result<(), String> {
        let payload = json!({ "status": "failed", "message": message }).to_string();
        let response = ureq::post(&status_url)
            .header("X-Execution-Token", &request.execution_token)
            .header("Authorization", &format!("Bearer {}", request.user_token))
            .header("Content-Type", "application/json")
            .send(payload.as_bytes())
            .map_err(|error| format!("上报失败：{error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP {status}"));
        }
        Ok(())
    };
    match report() {
        Ok(()) => eprintln!("采集失败已上报云端：{message}"),
        Err(error) => eprintln!("云端未能记下这次失败（{error}），原始原因：{message}"),
    }
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
        fs::create_dir_all(chat.join("audio")).unwrap();
        fs::write(
            chat.join("messages.md"),
            "- **张三** 10:01:00：你好\n- **张三** 10:02:00：[语音](audio/1_voice_1.silk)\n",
        )
        .unwrap();
        // 导出程序的中间产物与语音并排放：只有后者该进归档
        fs::write(chat.join("messages.json"), r#"{"messages":[]}"#).unwrap();
        let voice = b"\x02#SILK\x03voice-bytes";
        fs::write(chat.join("audio").join("1_voice_1.silk"), voice).unwrap();

        let bundle = build_archive(dir.path(), "task-1").unwrap();
        assert_eq!(bundle["schema_version"], ARCHIVE_SCHEMA);
        assert_eq!(bundle["task_id"], "task-1");
        assert_eq!(bundle["bootstrap"], false);
        let exports = bundle["exports"].as_array().unwrap();
        assert_eq!(exports.len(), 1, "只应收集 messages.md");
        // Markdown 里没有会话 ID，用文件夹名当标识；@chatroom 判群聊。
        assert_eq!(exports[0]["chat"]["id"], "张三");
        assert_eq!(exports[0]["chat"]["type"], "contact");
        assert_eq!(exports[0]["days"][0]["date"], "2026-10-05");
        assert_eq!(exports[0]["days"][0]["message_count"], 2);
        // v3：语音跟着当天一起走；messages.json 仍被排除在外
        let media = exports[0]["days"][0]["media"].as_array().unwrap();
        assert_eq!(media.len(), 1, "语音该进归档，messages.json 不该");
        assert_eq!(media[0]["path"], "audio/1_voice_1.silk");
        assert_eq!(media[0]["size"], voice.len() as u64);
        assert_eq!(
            media[0]["sha256"],
            crate::backend::sha256::hex_digest(voice)
        );
        assert_eq!(
            media[0]["content_base64"],
            crate::backend::base64::encode(voice)
        );
    }

    /// 回归：没有语音的那天不能报错，`media` 给空数组即可。
    ///
    /// 大量会话整天只有文字，若把"没有媒体"当成异常，第一次全量采集就会失败。
    #[test]
    fn day_without_media_carries_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("wxid_demo").join("王五").join("2026-10-05");
        fs::create_dir_all(&chat).unwrap();
        fs::write(chat.join("messages.md"), "- **王五** 10:01:00：纯文字\n").unwrap();

        let bundle = build_archive(dir.path(), "task-1").unwrap();
        let media = bundle["exports"][0]["days"][0]["media"].as_array().unwrap();
        assert!(media.is_empty(), "没有语音的天应该是空数组");
        assert_eq!(archive_media_count(&bundle), 0);
    }

    /// 媒体路径必须用 `/` 分隔，且能钻到子目录。
    ///
    /// Windows 上 `Path` 的显示是 `\`，而 Markdown 链接里写的是 `/`；两边不一致，
    /// 云端按 `](audio/…)` 找文件就会全部落空。递归则保证导出程序以后加
    /// `image/`、`video/` 时不用改这里。
    #[test]
    fn media_paths_use_forward_slashes_and_recurse() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("wxid_demo").join("赵六").join("2026-10-05");
        fs::create_dir_all(chat.join("audio").join("nested")).unwrap();
        fs::write(chat.join("messages.md"), "- **赵六** 10:01:00：在\n").unwrap();
        fs::write(chat.join("audio").join("1_voice_1.silk"), b"silk-a").unwrap();
        fs::write(
            chat.join("audio").join("nested").join("2_voice_2.silk"),
            b"silk-b",
        )
        .unwrap();

        let bundle = build_archive(dir.path(), "task-1").unwrap();
        let media = bundle["exports"][0]["days"][0]["media"].as_array().unwrap();
        let paths: Vec<&str> = media
            .iter()
            .map(|item| item["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            vec!["audio/1_voice_1.silk", "audio/nested/2_voice_2.silk"]
        );
        assert_eq!(archive_media_count(&bundle), 2);
    }

    /// 超大媒体文件要跳过，但不能让整包失败。
    ///
    /// 真实语音只有几十 KB；这里踩的是上限本身有没有生效 —— 少了这道闸，
    /// 一个异常文件足以顶穿云端的请求体上限，把整天的文字一起带走。
    #[test]
    fn oversized_media_is_skipped_without_failing_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("wxid_demo").join("孙七").join("2026-10-05");
        fs::create_dir_all(chat.join("audio")).unwrap();
        fs::write(chat.join("messages.md"), "- **孙七** 10:01:00：嗨\n").unwrap();
        fs::write(chat.join("audio").join("small.silk"), b"tiny").unwrap();
        fs::write(
            chat.join("audio").join("huge.silk"),
            vec![b'x'; MAX_MEDIA_FILE_BYTES as usize + 1],
        )
        .unwrap();

        let bundle = build_archive(dir.path(), "task-1").unwrap();
        let media = bundle["exports"][0]["days"][0]["media"].as_array().unwrap();
        assert_eq!(media.len(), 1, "超限的那个必须被跳过");
        assert_eq!(media[0]["path"], "audio/small.silk");
    }

    /// 当天总量超限时，**排在前面的**照收、后面的跳过，整包仍然成功。
    ///
    /// 单文件上限挡不住"一天攒很多"：带图片视频的日子 Base64 后足以顶穿云端
    /// 请求体上限，把整天的文字一起带走。这里验证的是：到顶就收手当天剩余
    /// 媒体，而不是整包失败。
    #[test]
    fn daily_media_budget_stops_at_the_cap_without_failing_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("wxid_demo").join("周八").join("2026-10-05");
        fs::create_dir_all(chat.join("audio")).unwrap();
        fs::write(chat.join("messages.md"), "- **周八** 10:01:00：在
").unwrap();
        // 四个各 16MB 的文件：前三格合计正好 48MB 到顶，第四个超限
        // （48 + 16 = 64 > 48）。每个都压在单文件上限内，撞的正是当天总量闸。
        for name in ["a.silk", "b.silk", "c.silk", "d.silk"] {
            fs::write(chat.join("audio").join(name), vec![b'x'; MAX_MEDIA_FILE_BYTES as usize]).unwrap();
        }

        let bundle = build_archive(dir.path(), "task-1").unwrap();
        let media = bundle["exports"][0]["days"][0]["media"].as_array().unwrap();
        let paths: Vec<&str> = media.iter().map(|item| item["path"].as_str().unwrap()).collect();
        assert_eq!(
            paths,
            vec!["audio/a.silk", "audio/b.silk", "audio/c.silk"],
            "到顶后当天剩余媒体必须跳过"
        );
        assert_eq!(archive_media_count(&bundle), 3);
    }

    /// 整包媒体预算：跨天累计，到顶就收手，整包仍然成功。
    ///
    /// 单天的闸管不住"很多天"：这次采集覆盖十来天、每天 48MB 就是近五百兆，
    /// Base64 后远超云端 128MB 请求体上限，整包上传失败、十多天文字一起丢。
    /// 这里验证的是第三道闸（整包 64MB）生效，且到顶后**文字照发**。
    ///
    /// 只断言总量不断言按天分布：`collect_days` 按 `read_dir` 顺序扫日期，
    /// 哪天先被扫到看文件系统心情；但无论哪天先扫，整包都只收到 64MB ——
    /// 先扫的那天吃满当天/整包预算，后扫的那天只剩一个文件的余量。
    #[test]
    fn bundle_media_budget_stops_at_the_cap_without_failing_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("wxid_demo").join("吴九").join("2026-10-01");
        let second = dir.path().join("wxid_demo").join("吴九").join("2026-10-02");
        fs::create_dir_all(first.join("audio")).unwrap();
        fs::create_dir_all(second.join("audio")).unwrap();
        fs::write(first.join("messages.md"), "- **吴九** 10:01:00：早\n").unwrap();
        fs::write(second.join("messages.md"), "- **吴九** 10:01:00：早\n").unwrap();
        // 两天共 6 个 16MB 文件。先扫的一天收满 64MB 里的绝大部分，后扫的一天
        // 只剩一个文件的余量，其余跳过 —— 合起来恰好 4 个文件、64MB。
        for day in [&first, &second] {
            for name in ["a.silk", "b.silk", "c.silk", "d.silk"] {
                fs::write(
                    day.join("audio").join(name),
                    vec![b'x'; MAX_MEDIA_FILE_BYTES as usize],
                )
                .unwrap();
            }
        }

        let bundle = build_archive(dir.path(), "task-1").unwrap();
        assert_eq!(archive_media_count(&bundle), 4, "整包到顶后剩余媒体必须跳过");
        let collected: u64 = bundle["exports"][0]["days"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|day| day["media"].as_array().unwrap())
            .map(|entry| entry["size"].as_u64().unwrap())
            .sum();
        assert_eq!(collected, MAX_BUNDLE_MEDIA_BYTES, "整包媒体总量必须停在预算线");
        // 文字不受媒体预算影响：两天都还在
        assert_eq!(bundle["exports"][0]["days"].as_array().unwrap().len(), 2);
        for day in bundle["exports"][0]["days"].as_array().unwrap() {
            assert!(day["markdown"].as_str().unwrap().contains("早"));
        }
    }

    /// 回归：导出程序会在导出根下并排放 `.locks` 这类内部目录。
    ///
    /// 以前的实现取"第一个子目录"当账号层 —— 命中 `.locks` 就会一个会话都找不到，
    /// 表现是客户端报「导出目录里没有找到任何 messages.md」，而数据其实好端端躺在旁边。
    #[test]
    fn hidden_lock_directory_does_not_hide_conversations() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".locks")).unwrap();
        let chat = dir.path().join("wxid_demo").join("李四").join("2026-10-06");
        fs::create_dir_all(&chat).unwrap();
        fs::write(chat.join("messages.md"), "- **李四** 10:01:00：嗨\n").unwrap();
        // 日期目录里也可能藏点文件，同样不该被当成会话层或媒体
        fs::write(chat.join(".partial"), b"half-written").unwrap();

        let bundle = build_archive(dir.path(), "task-2").unwrap();
        let exports = bundle["exports"].as_array().unwrap();
        assert_eq!(exports.len(), 1, "内部目录不能被当成会话层");
        assert_eq!(exports[0]["chat"]["id"], "李四");
        assert!(
            exports[0]["days"][0]["media"]
                .as_array()
                .unwrap()
                .is_empty(),
            "日期目录里的隐藏文件不是媒体"
        );
    }

    #[test]
    fn empty_directory_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(build_archive(dir.path(), "task-1").is_err());
    }
}
