//! 云端命令通道（客户端侧）。
//!
//! # 为什么是"客户端轮询"而不是"云端直连"
//!
//! 客户端装在客户的 Windows 电脑上，位于 NAT 之后、没有公网 IP，云端无法主动连进来。
//! 所以方向反过来：**客户端定时向云端取命令**，执行完把结果回传。这样：
//!
//! - 不需要任何入站端口，不需要端口转发或隧道
//! - 云端地址与凭据是出站配置，客户网络环境怎么变都不影响
//! - 沿用 TangHang `windows/control-plane` 已验证的形态
//!
//! # 契约
//!
//! 心跳 `PUT <cloud>/api/agent/v1/status`（每 20 秒一次，见 [`HEARTBEAT_SECONDS`]）：
//!
//! ```json
//! {"agentVersion":"0.1.0","state":"idle","progress":0,
//!  "wechat":{"wxid":"wxid_x","name":"张三","online":true,"state":"ready","processRunning":true,
//!            "processName":"WeChat.exe","accountPresent":true,"databasePresent":true,
//!            "credentialReady":true,"observedAt":"…","reason":""},
//!  "at":"…"}
//! ```
//!
//! 取命令 `POST <cloud>/api/agent/v1/commands/poll`：
//!
//! ```http
//! Authorization: Device <deviceToken>
//! X-AICRM-Device-ID: <deviceId>
//! ```
//! ```json
//! {"code":0,"message":"success","data":{"command":null}}
//! {"code":0,"message":"success","data":{"command":{
//!    "id":"uuid","type":"wechat.export","payload":{"day":2,"export_dir":"D:\\导出"},
//!    "leaseToken":"…"}}}
//! ```
//!
//! 回传 `PUT <cloud>/api/agent/v1/commands/<id>/result`（形状见 [`cloud_result`]）：
//!
//! ```json
//! {"leaseToken":"…","finishedAt":"2026-10-06T10:49:01Z","status":"succeeded",
//!  "result":{"wxid":"wxid_x","name":"张三","platform":"windows",
//!            "keyReady":true,"databaseReady":true}}
//! {"leaseToken":"…","finishedAt":"…","status":"failed",
//!  "error":{"code":"ACCOUNT_NOT_LOGGED_IN","message":"微信当前未运行或未登录"}}
//! ```
//!
//! 支持三种命令类型，见 [`execute`]：
//!
//! - `wechat.bind` → `AICRM-Key.exe prepare`（等价于 `/api/aicrm/prepare`）。密钥缺失时
//!   辅助程序**可能重启微信**，云端只应在确认客户在场时下发。
//! - `wechat.export` → 导出，结果留在本机（等价于 `/api/aicrm/export`）。
//! - `wechat.collect` → 导出 + 归档上传，把结果交回云端（等价于 `/api/aicrm/collect`）。
//!
//! 三种也都保留着同名的本地 HTTP 接口，两条路径复用同一批 `Service` 方法。

use std::{path::Path, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ApiError, ApiResult, ArchiveRequest, ExportRequest, PrepareRequest, Service, now_rfc3339};

/// 长轮询的挂起秒数。云端允许 5..=60；取中间值，兼顾实时性与连接数。
const POLL_WAIT_SECONDS: u64 = 25;

/// 心跳间隔。云端 `OFFLINE_AFTER` 默认 45 秒，超时即判设备离线、网页端不允许下发任务。
/// 取 20 秒留出抖动余量。
const HEARTBEAT_SECONDS: u64 = 20;

/// 云端基地址与设备凭据。
#[derive(Clone, Debug)]
pub struct CloudConfig {
    /// 固定基地址。`base_url_file` 存在时以文件内容为准。
    pub base_url: String,
    /// 从文件读取云端的基地址。隧道（cloudflared / serveo 等）每次重启地址都会变，
    /// 把它放进文件后，地址变了只要改文件 —— 不用重装客户端。
    pub base_url_file: Option<std::path::PathBuf>,
    /// 本机登录账号的云端令牌文件（api.token）。
    ///
    /// 设备本身只是机器身份，不归账号所有；「谁在用这台设备」由云端会话表达。
    /// 客户端每次心跳都现读这个文件并把令牌带给云端，网页端据此列出属于当前
    /// 登录账号的设备 —— 换账号登录（文件被登录程序重写）下一次心跳就生效，
    /// 不需要重新配对、不需要重启、也不会挪动任何按账号隔离的数据。
    pub user_token_file: Option<std::path::PathBuf>,
    pub device_token: String,
    pub device_id: String,
    /// 轮询间隔。太密会被云端限流，太疏用户点了要等。
    pub interval: Duration,
}

impl CloudConfig {
    /// 每次请求都重新解析：文件里改了地址，下一轮轮询就会用新的。
    fn base_url(&self) -> String {
        if let Some(path) = &self.base_url_file
            && let Ok(text) = std::fs::read_to_string(path)
        {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.trim_end_matches('/').to_owned();
            }
        }
        self.base_url.trim_end_matches('/').to_owned()
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    // 不加 `#[serde(default)]`：那会给 T 加上 Default 约束。
    // `Option<T>` 字段缺失时 serde 本来就解成 None。
    data: Option<T>,
}

#[derive(Deserialize)]
struct PollData {
    #[serde(default)]
    command: Option<Command>,
}

/// 云端下发的命令。
#[derive(Debug, Deserialize, PartialEq)]
pub struct Command {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    /// 回传结果时必须原样带回的租约令牌（云端校验 32..=2048 长度）。
    #[serde(rename = "leaseToken", default)]
    pub lease_token: Option<String>,
}

/// 命令执行结果，回传给云端。拥有自己的字符串，避免为构造错误信息而泄漏内存。
#[derive(Serialize, Debug)]
struct Report {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl Report {
    fn ok(data: Value) -> Self {
        Self {
            success: true,
            error_code: None,
            message: None,
            data: Some(data),
        }
    }

    fn failed(code: &str, message: impl Into<String>) -> Self {
        Self {
            success: false,
            error_code: Some(code.to_owned()),
            message: Some(message.into()),
            data: None,
        }
    }
}

/// 启动后台任务：心跳上报 + 命令轮询。任务随进程存活，不返回句柄。
pub fn spawn(service: std::sync::Arc<Service>, config: CloudConfig) {
    // 心跳。云端靠它判断设备在线 —— 没有心跳，网页端会显示"未连接"，
    // 并且不允许对这台设备下发采集任务。
    // 开机先清一次过期的导出产物。放在启动而不是采集之后，是为了让服务一
    // 起来就把磁盘腾出来 —— 老机器上的 jobs 目录可能已经攒了好几个月。
    let startup = service.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        startup.state.cleanup_jobs(&startup.data.join("jobs"));
    });

    let heartbeat_service = service.clone();
    let heartbeat_config = config.clone();
    tokio::spawn(async move {
        // 先立刻报一次，别让用户在网页端等一个心跳周期
        tokio::time::sleep(Duration::from_secs(2)).await;
        loop {
            if let Err(error) = send_heartbeat(&heartbeat_service, &heartbeat_config).await {
                eprintln!("心跳上报失败：{}", error.message());
            }
            // 顺手把「当前账号在本机绑定的微信号」拉到本地缓存，供界面展示。
            if let Err(error) = refresh_bound_wechat(&heartbeat_service, &heartbeat_config).await {
                eprintln!("读取绑定微信号失败：{}", error.message());
            }
            tokio::time::sleep(Duration::from_secs(HEARTBEAT_SECONDS)).await;
        }
    });

    tokio::spawn(async move {
        // 首次稍等，避免服务刚起来就和云端抢资源
        tokio::time::sleep(Duration::from_secs(3)).await;
        loop {
            // 网络抖动是常态：一次失败不能让轮询停下来
            if let Err(error) = tick(&service, &config).await {
                eprintln!("命令轮询失败：{}", error.message());
            }
            tokio::time::sleep(config.interval).await;
        }
    });
}

/// 本机微信是否在运行。用 tasklist 而不是引第三方库 —— 心跳每 20 秒一次，开销可接受。
fn wechat_process_running() -> bool {
    // 老版是 WeChat.exe，4.x 改成 Weixin.exe，两个都认
    ["WeChat.exe", "Weixin.exe"].iter().any(|name| {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("IMAGENAME eq {name}"), "/NH"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(name))
            .unwrap_or(false)
    })
}

/// 从本地导出的 messages.json 里找机主自己的微信号（昵称）。
///
/// 导出目录形状是 `<root>/<wxid>/<会话>/<日期>/messages.json`，每条消息的
/// `sender.is_self == true` 表示是机主发的，其 `sender.display_name` 就是微信号。
/// 深度优先翻到第一份能给出昵称的导出为止；只翻 20 份文件 —— 心跳每 20 秒跑
/// 一次，这个开销必须兜得住。找不到就返回空串，界面回落显示 wxid。
fn self_name_from_exports(root: &Path) -> String {
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
                    return String::new();
                }
                if let Some(name) = self_name_in_export(&path) {
                    return name;
                }
            }
        }
    }
    String::new()
}

/// 读一份 messages.json，返回机主的 display_name。
fn self_name_in_export(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&raw).ok()?;
    for message in value.get("messages")?.as_array()? {
        let sender = message.get("sender")?;
        if sender.get("is_self") == Some(&Value::Bool(true)) {
            let name = sender.get("display_name").and_then(Value::as_str);
            if let Some(name) = name.filter(|name| !name.is_empty()) {
                return Some(name.to_owned());
            }
        }
    }
    None
}

/// 探测本机微信的可采集状态。
///
/// 云端要求：只有 `online=true` 的设备才允许被下发采集任务，而 `online=true` 必须同时
/// 满足 wxid 非空、state=="ready"、进程在跑、账号在、库在、凭据就绪。任何一项不成立都
/// 如实报 false —— 宁可让用户看到"未就绪"，也不要让云端以为可以采集。
struct WeChatProbe {
    wxid: String,
    /// 微信号（昵称），从本地导出的 messages.json 里找：机主发消息时
    /// sender.is_self == true，那个 sender.display_name 就是微信号。
    /// 找不到就留空，云端和界面回落显示 wxid。
    name: String,
    process_running: bool,
    account_present: bool,
    database_present: bool,
    credential_ready: bool,
}

impl WeChatProbe {
    fn detect(service: &Service) -> Self {
        let mut probe = Self {
            wxid: String::new(),
            name: String::new(),
            process_running: wechat_process_running(),
            account_present: false,
            database_present: false,
            credential_ready: false,
        };
        if let Ok(entries) = std::fs::read_dir(&service.keys) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("wxid_") || !entry.path().is_dir() {
                    continue;
                }
                probe.wxid = name;
                probe.account_present = true;
                probe.credential_ready = entry.path().join("credentials.local.json").is_file();
                // cache 是密钥程序解密出来的数据库缓存，有它说明库也读得到
                probe.database_present = entry.path().join("cache").is_dir();
                break;
            }
        }
        if probe.account_present {
            // 昵称翻导出目录才能拿到，而心跳每 20 秒一次 —— 先查缓存，
            // 缓存没有才翻盘，翻到就记住，之后这台机器再也不用扫目录。
            probe.name = service.state.known_name(&probe.wxid).unwrap_or_else(|| {
                let found = self_name_from_exports(&service.data.join("jobs"));
                if !found.is_empty() {
                    service.state.remember_name(&probe.wxid, &found);
                }
                found
            });
        }
        probe
    }

    fn online(&self) -> bool {
        self.process_running
            && self.account_present
            && self.database_present
            && self.credential_ready
            && !self.wxid.is_empty()
    }

    fn reason(&self) -> String {
        if self.online() {
            return String::new();
        }
        let mut missing = Vec::new();
        if !self.process_running {
            missing.push("微信未运行");
        }
        if !self.account_present {
            missing.push("尚未绑定微信账号");
        }
        if !self.database_present {
            missing.push("未找到本地数据库");
        }
        if !self.credential_ready {
            missing.push("密钥尚未就绪");
        }
        missing.join("；")
    }
}

/// 上报一次心跳。
async fn send_heartbeat(service: &Service, config: &CloudConfig) -> ApiResult<()> {
    let probe = WeChatProbe::detect(service);
    let online = probe.online();
    let observed_at = now_rfc3339();
    let body = json!({
        "agentVersion": env!("CARGO_PKG_VERSION"),
        // 顶层 state 由云端正则校验：^[a-z][a-z0-9_]{0,31}$
        "state": "idle",
        "progress": 0,
        // 采集进度。网页端靠这两个数回答客户最常问的两句：
        // "上次什么时候采的"、"有没有没传上去的"。缺省时空串 / 0，云端不校验。
        "lastCollectAt": service.state.last_collected_at().unwrap_or_default(),
        "pendingUploads": service.state.pending_uploads(),
        "wechat": {
            "wxid": probe.wxid,
            // 微信号（昵称）。云端 WeChatStatus 有对应字段，缺省不影响校验。
            "name": probe.name,
            "online": online,
            // online 为真时云端强制要求 state == "ready"
            "state": if online { "ready" } else { "not_ready" },
            "processRunning": probe.process_running,
            "processName": "WeChat.exe",
            "accountPresent": probe.account_present,
            "databasePresent": probe.database_present,
            "credentialReady": probe.credential_ready,
            "observedAt": observed_at,
            "reason": probe.reason(),
        },
        "at": observed_at,
    })
    .to_string();

    let config = config.clone();
    blocking(move || {
        let endpoint = format!("{}/api/agent/v1/status", config.base_url());
        let request = agent()
            .put(&endpoint)
            .header("Authorization", &format!("Device {}", config.device_token))
            .header("X-AICRM-Device-ID", &config.device_id)
            .header("Content-Type", "application/json");
        // 每次心跳现读登录令牌：告诉云端「这台设备上现在是哪个账号在登录」。
        // 换账号登录会重写这个文件，下一次心跳（20 秒内）云端就认得新账号了。
        let request = match current_user_token(&config) {
            Some(token) => request.header("X-AICRM-User-Token", &token),
            None => request,
        };
        let status = request
            .send(body.as_str())
            .map_err(|error| ApiError::helper(format!("云端不可达：{error}")))?
            .status();
        if !status.is_success() {
            return Err(ApiError::helper(format!("心跳被拒：HTTP {status}")));
        }
        Ok(())
    })
    .await
}

/// 现读本机登录令牌（api.token）。文件缺失或为空表示这台机器还没登录过。
fn current_user_token(config: &CloudConfig) -> Option<String> {
    let path = config.user_token_file.as_ref()?;
    let text = std::fs::read_to_string(path).ok()?;
    let token = text.trim().to_owned();
    (!token.is_empty()).then_some(token)
}

/// 取一次命令并执行。`Ok(false)` 表示本次没有任务。
async fn tick(service: &Service, config: &CloudConfig) -> ApiResult<bool> {
    let Some(command) = poll(config).await? else {
        return Ok(false);
    };
    let outcome = execute(service, config, &command).await;
    report(config, &command, outcome).await?;
    Ok(true)
}

/// 阻塞 HTTP 统一放到 `spawn_blocking`，不占用异步运行时的工作线程。
async fn blocking<T, F>(job: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> ApiResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|_| ApiError::io())?
}

fn agent() -> ureq::Agent {
    ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(POLL_WAIT_SECONDS + 20)))
        // 自己判状态码，才能把云端的错误消息读出来
        .http_status_as_error(false)
        .build()
        .into()
}

async fn poll(config: &CloudConfig) -> ApiResult<Option<Command>> {
    let config = config.clone();
    blocking(move || {
        let endpoint = format!("{}/api/agent/v1/commands/poll", config.base_url());
        // 云端用长轮询：请求体会被挂起，直到有任务或 waitSeconds 到期。
        // 少了这个字段（或值不在 5..=60）云端直接 400。
        let body = format!("{{\"waitSeconds\":{POLL_WAIT_SECONDS}}}");
        let mut response = agent()
            .post(&endpoint)
            .header("Authorization", &format!("Device {}", config.device_token))
            .header("X-AICRM-Device-ID", &config.device_id)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .send(body.as_str())
            .map_err(|error| ApiError::helper(format!("云端不可达：{error}")))?;

        let status = response.status();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|error| ApiError::helper(format!("读取命令失败：{error}")))?;
        if !status.is_success() {
            return Err(ApiError::helper(format!("云端返回 HTTP {status}")));
        }

        let envelope: Envelope<PollData> =
            serde_json::from_str(&text).map_err(|_| ApiError::helper("云端命令响应不符合契约"))?;
        Ok(envelope.data.and_then(|data| data.command))
    })
    .await
}

/// 把命令 payload 解成具体请求类型。云端省略 `payload` 时 serde 给的是 `Value::Null`，
/// 而请求类型需要的是对象 —— 必须先归一化成 `{}`，否则会误报 payload 不合法。
fn decode<T: serde::de::DeserializeOwned>(command: &Command) -> Result<T, Report> {
    let payload = if command.payload.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        command.payload.clone()
    };
    serde_json::from_value(payload).map_err(|error| {
        Report::failed(
            "INVALID_PAYLOAD",
            format!("{} 命令 payload 不合法：{error}", command.kind),
        )
    })
}

/// 执行命令。任何失败都转成回传结果，绝不中断轮询。
async fn execute(service: &Service, config: &CloudConfig, command: &Command) -> Report {
    match command.kind.as_str() {
        // 取密钥。注意：密钥缺失时辅助程序**可能重启微信**，云端只应在确认客户在场时下发。
        "wechat.bind" => match decode::<PrepareRequest>(command) {
            Err(report) => report,
            Ok(request) => {
                // 先报一条，让网页端立刻从「没反应」变成「正在检测本机微信…」。
                report_command_progress(config, command, 3, "已收到绑定请求，正在检测本机微信…").await;
                match service.prepare(request).await {
                    Ok(data) => Report::ok(data),
                    Err(error) => Report::failed(error.code(), error.message()),
                }
            }
        },
        "wechat.export" => match decode::<ExportRequest>(command) {
            Err(report) => report,
            Ok(request) => match service.export_files(request).await {
                Ok(data) => Report::ok(data),
                Err(error) => Report::failed(error.code(), error.message()),
            },
        },
        // 数据采集闭环：导出 → 合并归档包 → 上传云端。
        // 与 wechat.export 的区别是它会把结果交回云端，而不只是落在本机。
        "wechat.collect" => match decode::<ArchiveRequest>(command) {
            Err(report) => report,
            Ok(request) => match service.export_and_upload(request).await {
                Ok(data) => Report::ok(data),
                Err(error) => Report::failed(error.code(), error.message()),
            },
        },
        // 客户端自更新。自己覆盖不了正在运行的自己：这里只负责下载、校验、
        // 落到 updates/<版本>/，真正的替换与重启交给 apply-update.ps1。
        "agent.update" => match decode::<UpdateRequest>(command) {
            Err(report) => report,
            Ok(request) => match service.apply_update(config, request).await {
                Ok(data) => Report::ok(data),
                Err(error) => Report::failed(error.code(), error.message()),
            },
        },
        other => Report::failed(
            "UNSUPPORTED_COMMAND",
            format!("本客户端不支持命令类型：{other}"),
        ),
    }
}

/// 把内部结果翻译成云端要求的 `BindResult` 形状。
///
/// 云端校验很严（`validateBindResult`）：succeeded 时必须带 result 且不能带 error，
/// `wxid` 要合法、`platform` 必须是 windows、`keyReady`/`databaseReady` 都要为 true；
/// failed 时反过来只能带 error。元数据里 `leaseToken` 长度须 ≥32，`finishedAt` 须是
/// RFC3339 且不能超过当前时间 10 分钟。
fn cloud_result(command: &Command, outcome: &Report) -> Value {
    let mut body = serde_json::Map::new();
    body.insert(
        "leaseToken".into(),
        json!(command.lease_token.clone().unwrap_or_default()),
    );
    body.insert("finishedAt".into(), json!(now_rfc3339()));

    if outcome.success {
        let data = outcome.data.clone().unwrap_or(Value::Null);
        // 只有绑定/采集类命令才需要那套「微信账号」形状的结果。
        // 其余命令（例如 agent.update）原样透传自己的结果 —— 否则云端记下的是
        // 一条 wxid 为空的绑定记录，更新出了问题时完全没法查。
        if command.kind != "wechat.bind" && command.kind != "wechat.collect" {
            // 云端对结果体是**严格解码**的：多一个未知字段整条回执就被判 400。
            // 所以只发服务端认识的字段，把版本号与就绪状态填进去。
            body.insert("status".into(), json!("succeeded"));
            body.insert(
                "result".into(),
                json!({
                    "wxid": "",
                    "name": "",
                    "platform": "windows",
                    "keyReady": false,
                    "databaseReady": false,
                    "version": data.get("version").and_then(Value::as_str).unwrap_or_default(),
                    "staged": data.get("staged").and_then(Value::as_bool).unwrap_or(false),
                }),
            );
            return Value::Object(body);
        }
        let flag = |key: &str| data.get(key).and_then(Value::as_bool).unwrap_or(true);
        let wxid = data
            .get("wxid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let name = data
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&wxid)
            .to_owned();
        body.insert("status".into(), json!("succeeded"));
        body.insert(
            "result".into(),
            json!({
                "wxid": wxid,
                "name": name,
                "platform": "windows",
                "keyReady": flag("key_ready"),
                "databaseReady": flag("database_ready"),
            }),
        );
    } else {
        body.insert("status".into(), json!("failed"));
        body.insert(
            "error".into(),
            json!({
                "code": outcome.error_code.clone().unwrap_or_else(|| "UNKNOWN".to_owned()),
                "message": outcome.message.clone().unwrap_or_default(),
            }),
        );
    }
    Value::Object(body)
}

/// 命令执行期间的进度上报。
///
/// 取密钥最长要几分钟，而云端原先只看得到「领走」和「最终结果」两个状态 ——
/// 中间什么都不报，网页端就是「点了没反应」。这里只负责让人看见「在跑」，
/// 上报失败一律忽略：它是可观测性，不是命令本身的一部分。
async fn report_command_progress(config: &CloudConfig, command: &Command, progress: i32, message: &str) {
    let config = config.clone();
    let command_id = command.id.clone();
    let message = message.to_owned();
    let _ = blocking(move || {
        let endpoint = format!(
            "{}/api/agent/v1/commands/{command_id}/progress",
            config.base_url()
        );
        let body = serde_json::json!({ "progress": progress, "message": message }).to_string();
        let _ = agent()
            .put(&endpoint)
            .header("Authorization", &format!("Device {}", config.device_token))
            .header("X-AICRM-Device-ID", &config.device_id)
            .header("Content-Type", "application/json")
            .send(body.as_str());
        Ok::<(), ApiError>(())
    })
    .await;
}

async fn report(config: &CloudConfig, command: &Command, outcome: Report) -> ApiResult<()> {
    let config = config.clone();
    let command_id = command.id.clone();
    let body = serde_json::to_string(&cloud_result(command, &outcome))
        .map_err(|error| ApiError::helper(format!("结果序列化失败：{error}")))?;
    blocking(move || {
        let endpoint = format!(
            "{}/api/agent/v1/commands/{command_id}/result",
            config.base_url()
        );
        let status = agent()
            .put(&endpoint)
            .header("Authorization", &format!("Device {}", config.device_token))
            .header("X-AICRM-Device-ID", &config.device_id)
            .header("Content-Type", "application/json")
            .send(body.as_str())
            .map_err(|error| ApiError::helper(format!("回传结果失败：{error}")))?
            .status();
        if !status.is_success() {
            return Err(ApiError::helper(format!("回传结果被拒：HTTP {status}")));
        }
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    // rfc3339 已经挪到 service/mod.rs（心跳与状态存储都要用），这里补回来
    use crate::backend::service::rfc3339;

    fn parse(raw: &str) -> Option<Command> {
        serde_json::from_str::<Envelope<PollData>>(raw)
            .unwrap()
            .data
            .unwrap()
            .command
    }

    #[test]
    fn parses_an_export_command() {
        let command = parse(
            r#"{"code":0,"message":"success","data":{"command":{
                "id":"c-1","type":"wechat.export","payload":{"day":2,"export_dir":"D:\\导出"}}}}"#,
        )
        .unwrap();
        assert_eq!(command.id, "c-1");
        assert_eq!(command.kind, "wechat.export");

        let request: ExportRequest = serde_json::from_value(command.payload).unwrap();
        assert_eq!(request.day, Some(2));
        assert_eq!(request.export_dir.as_deref(), Some("D:\\导出"));
    }

    #[test]
    fn empty_command_means_nothing_to_do() {
        assert!(parse(r#"{"code":0,"message":"success","data":{"command":null}}"#).is_none());
    }

    #[test]
    fn missing_payload_falls_back_to_export_everything() {
        // 云端只发 {"type":"wechat.export"} 时等价于"导出全部历史"
        let command = parse(
            r#"{"code":0,"message":"success","data":{"command":{"id":"c-2","type":"wechat.export"}}}"#,
        )
        .unwrap();
        // 省略 payload 时 serde 给的是 Null，必须能被归一化成空对象
        let request: ExportRequest = decode(&command).ok().expect("空 payload 应当可解");
        assert!(request.day.is_none() && request.export_dir.is_none());
    }

    #[test]
    fn wechat_bind_command_decodes_into_prepare_request() {
        let command = parse(
            r#"{"code":0,"message":"success","data":{"command":{"id":"c-3","type":"wechat.bind","payload":{"expected_wxid":"wxid_x","no_restart":true}}}}"#,
        )
        .unwrap();
        assert_eq!(command.kind, "wechat.bind");
        let request: PrepareRequest = decode(&command).ok().expect("bind payload 应当可解");
        assert_eq!(request.expected_wxid.as_deref(), Some("wxid_x"));
        assert!(request.no_restart);
    }

    #[test]
    fn unknown_payload_field_is_rejected_cleanly() {
        // ExportRequest 带 deny_unknown_fields：字段写错要立刻暴露，而不是被静默忽略
        let error = serde_json::from_value::<ExportRequest>(json!({"day": 2, "目录": "x"}))
            .err()
            .expect("未知字段应当被拒绝");
        assert!(error.to_string().contains("unknown field") || error.to_string().contains("目录"));
    }

    #[test]
    fn failure_report_serialises_the_contract_fields() {
        let report = Report::failed("ACCOUNT_NOT_LOGGED_IN", "微信当前未运行或未登录");
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["success"], false);
        assert_eq!(value["error_code"], "ACCOUNT_NOT_LOGGED_IN");
        assert_eq!(value["message"], "微信当前未运行或未登录");
        assert!(value.get("data").is_none(), "失败时不应带 data");
    }

    #[test]
    fn success_report_omits_error_fields() {
        let value = serde_json::to_value(Report::ok(json!({"message_count": 129}))).unwrap();
        assert_eq!(value["success"], true);
        assert_eq!(value["data"]["message_count"], 129);
        assert!(value.get("error_code").is_none() && value.get("message").is_none());
    }

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_000_000_000), "2001-09-09T01:46:40Z");
        // 取自真实签发的 JWT iat：2026-10-06 18:49:01 +08:00
        assert_eq!(rfc3339(1_791_283_741), "2026-10-06T10:49:01Z");
    }

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
        assert_eq!(self_name_from_exports(dir.path()), "flow state");
    }

    #[test]
    fn missing_exports_yield_empty_name() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(self_name_from_exports(dir.path()), "");
    }

    fn command_with_lease() -> Command {
        parse(
            r#"{"code":0,"message":"success","data":{"command":{
                "id":"c-9","type":"wechat.bind","leaseToken":"0123456789abcdef0123456789abcdef"}}}"#,
        )
        .unwrap()
    }

    #[test]
    fn failed_result_matches_the_cloud_contract() {
        let value = cloud_result(
            &command_with_lease(),
            &Report::failed("ACCOUNT_NOT_LOGGED_IN", "微信当前未运行或未登录"),
        );
        assert_eq!(value["status"], "failed");
        assert_eq!(value["leaseToken"], "0123456789abcdef0123456789abcdef");
        assert_eq!(value["error"]["code"], "ACCOUNT_NOT_LOGGED_IN");
        assert!(value.get("result").is_none(), "failed 时不能带 result");
        // 云端要求 finishedAt 是 RFC3339，且不能超过当前时间 10 分钟
        let finished = value["finishedAt"].as_str().unwrap();
        assert!(
            finished.ends_with('Z') && finished.len() == 20,
            "{finished}"
        );
    }

    #[test]
    fn succeeded_result_carries_platform_and_ready_flags() {
        let value = cloud_result(
            &command_with_lease(),
            &Report::ok(
                json!({"wxid":"wxid_abc","name":"张三","key_ready":true,"database_ready":true}),
            ),
        );
        assert_eq!(value["status"], "succeeded");
        assert_eq!(value["result"]["wxid"], "wxid_abc");
        assert_eq!(value["result"]["platform"], "windows");
        assert_eq!(value["result"]["keyReady"], true);
        assert_eq!(value["result"]["databaseReady"], true);
        assert!(value.get("error").is_none(), "succeeded 时不能带 error");
    }

    #[test]
    fn missing_ready_flags_default_to_true_on_success() {
        // 辅助程序没回报这两个字段时，绑定成功本身就意味着它们成立
        let value = cloud_result(&command_with_lease(), &Report::ok(json!({"wxid":"wxid_x"})));
        assert_eq!(value["result"]["keyReady"], true);
        assert_eq!(value["result"]["databaseReady"], true);
        // name 缺省时回落成 wxid
        assert_eq!(value["result"]["name"], "wxid_x");
    }
}

/// `agent.update` 命令的载荷：云端发布程序时算好的版本与摘要。
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct UpdateRequest {
    /// 程序类型。云端 programs.json 里只会有 agent / key / export。
    #[serde(default)]
    kind: String,
    version: String,
    /// 发布时的文件名，例如 agent-0.1.2.exe。
    #[serde(default)]
    name: String,
    /// 期望的 SHA-256（小写十六进制）。
    sha256: String,
    /// 字节数，只用于日志与界面提示。
    #[serde(default)]
    size: i64,
}

impl Service {
    /// 客户端自更新：下载 → 校验 → 落盘 → 交接给外部脚本替换重启。
    ///
    /// 本函数返回后主进程仍会正常把命令结果回传云端；真正的替换发生在宽限期之后
    /// （apply-update.ps1 先停服务进程，再覆盖 exe，最后按自启项把客户端拉起来）。
    /// 云端不依赖这次回执判断成败 —— 它看重启后心跳带上来的版本号。
    async fn apply_update(
        &self,
        config: &CloudConfig,
        request: UpdateRequest,
    ) -> ApiResult<Value> {
        let install_dir = self.tools.parent().ok_or_else(ApiError::io)?.to_path_buf();
        let version = request.version.trim().to_owned();
        if version.is_empty() {
            return Err(ApiError::helper("更新版本号为空"));
        }
        let kind = if request.kind.trim().is_empty() {
            "agent".to_owned()
        } else {
            request.kind.trim().to_owned()
        };
        // 只接受 agent：key / export 是辅助程序，替换它们各自还有落地路径要处理。
        if kind != "agent" {
            return Err(ApiError::helper(format!("不支持的更新类型：{kind}")));
        }
        // 目标文件名固定：安装目录里 agent 恒为 aicrm_windows_api.exe。
        // 云端清单里的 name 是**发布产物名**（如 agent-0.1.4.exe），拿它当替换目标
        // 会往安装目录里多写一个文件、而真正要换的程序根本没动 —— 实测踩过这个坑。
        let file_name = "aicrm_windows_api.exe".to_owned();
        let artifact_name = request.name.trim().to_owned();
        let updater = install_dir.join("aicrm-update.exe");
        if !updater.is_file() {
            return Err(ApiError::helper("安装目录缺少 aicrm-update.exe，无法完成自更新"));
        }

        // 幂等：同一个版本已经下载并校验过就直接回「已就绪」，别重复拉几 MB。
        // 云端在冷却后会重复下发，这里挡掉多余流量。
        if let Some(ready) = staged_ready(&install_dir, &version, &file_name) {
            return Ok(ready);
        }

        let url = format!("{}/api/agent/v1/programs/{kind}", config.base_url());
        let expected = request.sha256.trim().to_lowercase();
        let device_token = config.device_token.clone();
        let device_id = config.device_id.clone();
        let stage_dir = install_dir.join("updates").join(&version);
        // 闭包要拿走所有权，下面写 pending.json 还要用同一个名字，先复制一份。
        let staged_name = file_name.clone();

        let (size, digest) = blocking(move || {
            std::fs::create_dir_all(&stage_dir).map_err(|_| ApiError::io())?;
            let response = agent()
                .get(&url)
                .header("Authorization", &format!("Device {device_token}"))
                .header("X-AICRM-Device-ID", &device_id)
                .call()
                .map_err(|error| ApiError::helper(format!("下载更新包失败：{error}")))?;
            let bytes = response
                .into_body()
                .read_to_vec()
                .map_err(|error| ApiError::helper(format!("读取更新包失败：{error}")))?;
            let digest = crate::backend::sha256::hex_digest(&bytes);
            if !expected.is_empty() && digest != expected {
                return Err(ApiError::helper("更新包校验失败，已丢弃"));
            }
            std::fs::write(stage_dir.join(&staged_name), &bytes).map_err(|_| ApiError::io())?;
            Ok((bytes.len(), digest))
        })
        .await?;

        let requested_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or(0);
        let pending = json!({
            "version": version,
            "kind": kind,
            "files": [{ "name": file_name, "sha256": digest, "size": size }],
            "requestedAt": requested_at,
        });
        std::fs::write(
            install_dir.join("updates").join("pending.json"),
            serde_json::to_vec_pretty(&pending).map_err(|_| ApiError::io())?,
        )
        .map_err(|_| ApiError::io())?;

        // 到此为止，**不重启**。微信/QQ 都是「新版本就绪 → 下次启动应用」：
        // 采集可能正在跑，正用着的时候把进程杀掉是产品事故。
        // 真正的替换在下次启动时由 main() 交给 aicrm-update.exe 完成。
        Ok(json!({
            "staged": true,
            "version": version,
            "size": size,
            "artifact": artifact_name,
            "declaredSize": request.size,
            "from": env!("CARGO_PKG_VERSION"),
            "message": "新版本已下载并校验完成，将在下次启动时自动应用",
        }))
    }
}

/// 拉起更新器并立即返回：它必须活过本进程，才能真正覆盖正在运行的 exe。
///
/// 只在「启动时发现有已就绪的新版本」这一条路径上调用 —— 也就是说更新发生在
/// 客户自己下次打开程序的时候，而不是正用着的时候被强行重启（微信就是这个模型）。
///
/// DETACHED_PROCESS + CREATE_NEW_PROCESS_GROUP 让脚本脱离父进程；否则调用方退出时
/// 子进程会被一起收掉，替换永远不会发生。
#[cfg(windows)]
pub(crate) fn launch_updater(updater: &Path, install_dir: &Path) -> ApiResult<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    Command::new(updater)
        .arg("--install-dir")
        .arg(install_dir)
        .arg("--grace-seconds")
        .arg("8")
        .current_dir(install_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map_err(|_| ApiError::helper("无法启动更新器"))?;
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn launch_updater(_updater: &Path, _install_dir: &Path) -> ApiResult<()> {
    Err(ApiError::helper("当前平台不支持自更新"))
}

/// 拉取「当前登录账号在本机绑定的微信号」，缓存到 `data/state/bound-wechat.json`。
///
/// 界面上的「密钥获取」卡片必须按**当前账号**渲染：本机 `accounts` 目录里可能躺着
/// 别的账号采集过的密钥，直接显示第一个会让客户以为当前账号已经绑好了（实测踩过：
/// 登录 liang，卡片却显示 test2 的微信号）。
///
/// 网络请求放在服务里而不是界面里：界面每帧都刷新快照，在 UI 线程等 HTTP 会卡住界面。
async fn refresh_bound_wechat(service: &Service, config: &CloudConfig) -> ApiResult<()> {
    let Some(token) = current_user_token(config) else {
        return Ok(());
    };
    let base = config.base_url().to_owned();
    let device_id = config.device_id.clone();
    let state_dir = service.data.join("state");

    blocking(move || {
        let url = format!("{base}/api/ui/v1/devices");
        let mut response = agent()
            .get(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("Accept", "application/json")
            .call()
            .map_err(|error| ApiError::helper(format!("云端不可达：{error}")))?;
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|_| ApiError::io())?;
        let value: Value =
            serde_json::from_str(&text).map_err(|_| ApiError::helper("设备列表响应不合法"))?;
        let bound = value
            .get("data")
            .and_then(|data| data.get("devices"))
            .and_then(|list| list.as_array())
            .and_then(|list| {
                list.iter().find(|item| {
                    item.get("id").and_then(|value| value.as_str()) == Some(device_id.as_str())
                })
            });
        let record = json!({
            "deviceId": device_id,
            "wxid": bound.and_then(|item| item.get("boundWxid")).and_then(|v| v.as_str()).unwrap_or(""),
            "name": bound.and_then(|item| item.get("boundName")).and_then(|v| v.as_str()).unwrap_or(""),
            "at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_secs())
                .unwrap_or(0),
        });
        std::fs::create_dir_all(&state_dir).map_err(|_| ApiError::io())?;
        std::fs::write(
            state_dir.join("bound-wechat.json"),
            serde_json::to_vec_pretty(&record).unwrap_or_default(),
        )
        .map_err(|_| ApiError::io())?;
        Ok(())
    })
    .await
}

/// 判断某个版本是否已经下载并校验过。已就绪就返回回执，免得重复下载。
fn staged_ready(install_dir: &Path, version: &str, file_name: &str) -> Option<Value> {
    let updates = install_dir.join("updates");
    let pending = updates.join("pending.json");
    let raw = std::fs::read_to_string(&pending).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    if value.get("version").and_then(|item| item.as_str()) != Some(version) {
        return None;
    }
    let staged = updates.join(version).join(file_name);
    if !staged.is_file() {
        return None;
    }
    Some(json!({
        "staged": true,
        "version": version,
        "size": std::fs::metadata(&staged).map(|item| item.len()).unwrap_or(0),
        "from": env!("CARGO_PKG_VERSION"),
        "message": "该版本此前已下载完成，将在下次启动时自动应用",
    }))
}
