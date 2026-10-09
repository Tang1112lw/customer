//! 云端登录协议（客户端侧实现）。
//!
//! # 契约
//!
//! 对应 TangHang 后端的 `POST /api/auth/login`
//! （`backend/internal/identity/login/handlers.go`）。
//!
//! ## 请求
//!
//! ```http
//! POST <endpoint>
//! Content-Type: application/json
//!
//! {"username":"admin","password":"……","device_id":"3f2a…","client_version":"0.1.0"}
//! ```
//!
//! 云端目前只读 `username` / `password`；其余字段被忽略（`platformweb.DecodeJSON` 解到
//! map，不拒绝未知字段），`device_id` 是为后续设备绑定预留的。
//!
//! **不带 `Authorization` 头。** 这个端点就是"用凭据换令牌"的入口，它自己不可能要求令牌
//! （鸡生蛋）。代价是它天然是公开端点，只能靠 **HTTPS + 云端失败次数限制** 防滥用。
//!
//! ## 成功响应 `200 OK`
//!
//! ```json
//! {"code":0,"message":"success","data":{
//!   "accessToken":"eyJ…","refreshToken":"eyJ…",
//!   "user":{"user":{"name":"系统管理员","username":"admin"},"roles":[…]}}}
//! ```
//!
//! ## 失败响应
//!
//! ```json
//! {"code":401,"message":"账号或密码错误","data":null}
//! ```

use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize)]
struct LoginRequest<'a> {
    username: &'a str,
    password: &'a str,
    device_id: &'a str,
    client_version: &'a str,
}

/// 云端统一的 `{code, message, data}` 信封。
#[derive(Deserialize)]
struct Envelope<T> {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
    // 不加 `#[serde(default)]`：那会让 serde 给 T 加上 Default 约束。
    // `Option<T>` 字段缺失时 serde 本来就解成 None。
    data: Option<T>,
}

#[derive(Deserialize)]
struct LoginData {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken", default)]
    refresh_token: Option<String>,
    #[serde(default)]
    user: Option<LoginUser>,
}

#[derive(Deserialize)]
struct LoginUser {
    #[serde(default)]
    user: Option<UserProfile>,
}

#[derive(Deserialize)]
struct UserProfile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

/// 云端签发的会话。`access_token` 会被写进本地令牌文件供后台服务鉴权。
pub struct Session {
    pub access_token: String,
    #[allow(dead_code)] // 供后续 accessToken 过期后的自动续期使用
    pub refresh_token: Option<String>,
    pub display_name: Option<String>,
}

/// 手写 `Debug` 并把令牌打码 —— 免得哪天有人 `{:?}` 一下就把它写进日志。
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("access_token", &"<已隐藏>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<已隐藏>"),
            )
            .field("display_name", &self.display_name)
            .finish()
    }
}

#[derive(Debug)]
pub enum LoginError {
    /// 凭据被拒（4xx）。改密码重试才有意义。
    Rejected(String),
    /// 连不上云端，或云端 5xx / 超时。重试可能有用。
    Unreachable(String),
    /// 云端返回了不符合契约的内容 —— 大概率是前后端版本不匹配。
    Protocol(String),
}

impl LoginError {
    pub fn message(&self) -> &str {
        match self {
            Self::Rejected(message) | Self::Unreachable(message) | Self::Protocol(message) => {
                message
            }
        }
    }
}

/// 用账号密码向云端换取会话令牌。
///
/// 阻塞调用，必须放在后台线程里跑（GUI 线程不能卡）。
pub fn login(
    endpoint: &str,
    username: &str,
    password: &str,
    device_id: &str,
) -> Result<Session, LoginError> {
    let body = serde_json::to_string(&LoginRequest {
        username,
        password,
        device_id,
        client_version: env!("CARGO_PKG_VERSION"),
    })
    .map_err(|error| LoginError::Protocol(format!("请求序列化失败：{error}")))?;

    let agent: ureq::Agent = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(20)))
        // 自己判状态码，才能把云端的 message 读出来给用户看
        .http_status_as_error(false)
        .build()
        .into();

    let mut response = agent
        .post(endpoint)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send(body.as_str())
        .map_err(|error| LoginError::Unreachable(format!("无法连接云端：{error}")))?;

    let status = response.status();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| LoginError::Unreachable(format!("读取云端响应失败：{error}")))?;

    let envelope: Envelope<LoginData> = serde_json::from_str(&text)
        .map_err(|_| LoginError::Protocol("云端返回的不是 {code,message,data} 信封".to_owned()))?;

    if status.is_success() && envelope.code == 0 {
        let data = envelope
            .data
            .ok_or_else(|| LoginError::Protocol("云端返回成功但缺少 data".to_owned()))?;
        let display_name = data
            .user
            .and_then(|wrapper| wrapper.user)
            .and_then(|profile| profile.name.or(profile.username));
        return Ok(Session {
            access_token: data.access_token,
            refresh_token: data.refresh_token,
            display_name,
        });
    }

    let message = if envelope.message.is_empty() {
        format!("云端返回 HTTP {status}")
    } else {
        envelope.message
    };
    // 4xx 是凭据问题，重试无用；5xx 是云端故障，值得重试。
    if status.is_client_error() {
        Err(LoginError::Rejected(message))
    } else {
        Err(LoginError::Unreachable(message))
    }
}

/// 非 HTTPS 且非本机地址时给出警告 —— 明文传密码是这套方案最危险的失误。
pub fn transport_warning(endpoint: &str) -> Option<String> {
    let lower = endpoint.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return None;
    }
    let is_local = lower.starts_with("http://127.0.0.1")
        || lower.starts_with("http://localhost")
        || lower.starts_with("http://[::1]");
    if is_local {
        return None;
    }
    Some("警告：该地址不是 HTTPS，密码会以明文经过网络".to_owned())
}

/// 云端签发的设备预置授权：设备 ID 与配对码成对出现。
#[derive(Deserialize)]
pub struct Provision {
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "provisionCode")]
    pub provision_code: String,
}

/// 用已登录的令牌为本机申领一个配对码。
///
/// 该接口要求管理员权限 —— 这正是过去"装个客户端还得先要账号密码"的根因。
/// 现在配对挪到客户端首次打开时的连接引导页，由使用者填一次，装的时候一个字都不问。
pub fn create_provision(base_url: &str, access_token: &str) -> Result<Provision, String> {
    let url = format!("{}/api/ui/v1/provisions", base_url.trim_end_matches('/'));
    let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".to_owned());
    let body = serde_json::json!({ "name": name }).to_string();

    let agent: ureq::Agent = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .post(&url)
        .header("Authorization", &format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .send(body.as_str())
        .map_err(|error| format!("无法连接云端：{error}"))?;

    let status = response.status();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("读取响应失败：{error}"))?;
    if !status.is_success() {
        return Err(format!(
            "云端拒绝签发配对码（HTTP {status}）。这个账号需要有管理员权限：{text}"
        ));
    }
    let envelope: Envelope<Provision> =
        serde_json::from_str(&text).map_err(|_| "云端返回格式不符合预期".to_owned())?;
    envelope
        .data
        .ok_or_else(|| "云端没有返回配对信息".to_owned())
}

/// 用配对码换设备身份。
///
/// 令牌在**本机**生成，云端只保存它的 SHA-256 —— 所以明文从不经过网络，
/// 装完之后也只在本地盘上有一份（权限已收紧）。
pub fn pair_device(
    base_url: &str,
    provision: &Provision,
    device_token: &str,
) -> Result<(), String> {
    let url = format!("{}/api/agent/v1/provision", base_url.trim_end_matches('/'));
    let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".to_owned());
    let body = serde_json::json!({
        "deviceId": provision.device_id,
        "code": provision.provision_code,
        "deviceToken": device_token,
        "name": name,
    })
    .to_string();

    let agent: ureq::Agent = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .post(&url)
        .header("Content-Type", "application/json")
        .send(body.as_str())
        .map_err(|error| format!("无法连接云端：{error}"))?;

    let status = response.status();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("读取响应失败：{error}"))?;
    if !status.is_success() {
        return Err(format!("配对失败（HTTP {status}）：{text}"));
    }
    let decoded: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| "云端返回格式不符合预期".to_owned())?;
    let done = decoded
        .pointer("/data/provisioned")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if done {
        Ok(())
    } else {
        Err("云端未确认配对".to_owned())
    }
}
