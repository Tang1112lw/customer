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
//! {"username":"admin","password":"……","client_id":"3f2a…","client_version":"0.1.0"}
//! ```
//!
//! 云端目前只读 `username` / `password`；其余字段被忽略（`platformweb.DecodeJSON` 解到
//! map，不拒绝未知字段）。`client_id` 是本机客户端实例标识 —— 云端不认设备，
//! 它只用来落实「一个账号只能登录一个客户端」。
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
    client_id: &'a str,
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
    client_id: &str,
) -> Result<Session, LoginError> {
    let body = serde_json::to_string(&LoginRequest {
        username,
        password,
        client_id,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc::{Receiver, channel},
    };

    /// 起一个只服务一次请求的假云端，返回它的 URL 和"收到的原始请求"的接收端。
    fn mock_cloud(status_line: &'static str, body: &'static str) -> (String, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = channel();

        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();

            // 必须按 Content-Length 读满，单次 read 可能只拿到请求头。
            let mut raw = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&raw);
                if let Some(head) = text.find("\r\n\r\n") {
                    // 头部名大小写不敏感：ureq 发的是小写 content-length。
                    let mut length = 0usize;
                    for line in text[..head].lines() {
                        if let Some((name, value)) = line.split_once(':')
                            && name.eq_ignore_ascii_case("content-length")
                        {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                    if raw.len() >= head + 4 + length {
                        break;
                    }
                }
            }
            let _ = sender.send(String::from_utf8_lossy(&raw).into_owned());

            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        });

        (format!("http://{address}/api/auth/login"), receiver)
    }

    #[test]
    fn sends_credentials_as_json_and_reads_the_session() {
        // 真实云端（TangHang POST /api/auth/login）的响应形状
        let (endpoint, received) = mock_cloud(
            "200 OK",
            r#"{"code":0,"message":"success","data":{
                "accessToken":"eyJhbGciOi.access","refreshToken":"eyJhbGciOi.refresh",
                "user":{"user":{"name":"系统管理员","username":"admin"},"roles":[
                    {"name":"admin","display_name":"管理员"}]}}}"#,
        );

        let session = login(&endpoint, "zhangsan", "hunter2", "client-1").unwrap();

        assert_eq!(session.access_token, "eyJhbGciOi.access");
        assert_eq!(session.refresh_token.as_deref(), Some("eyJhbGciOi.refresh"));
        assert_eq!(session.display_name.as_deref(), Some("系统管理员"));

        let request = received.recv().unwrap();
        assert!(
            request.starts_with("POST /api/auth/login HTTP/1.1"),
            "{request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: application/json"),
            "{request}"
        );
        // 关键：这个请求绝不能带 Authorization —— 它就是用来换取令牌的入口。
        assert!(
            !request.to_ascii_lowercase().contains("authorization:"),
            "登录请求不应携带 Authorization 头：{request}"
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let payload: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(payload["username"], "zhangsan");
        assert_eq!(payload["password"], "hunter2");
        assert_eq!(payload["client_id"], "client-1");
        assert!(payload["client_version"].is_string());
    }

    #[test]
    fn falls_back_to_username_when_display_name_is_absent() {
        let (endpoint, _) = mock_cloud(
            "200 OK",
            r#"{"code":0,"message":"success","data":{"accessToken":"tok","user":{"user":{"username":"admin"}}}}"#,
        );
        let session = login(&endpoint, "admin", "x", "client-1").unwrap();
        assert_eq!(session.display_name.as_deref(), Some("admin"));
        assert!(session.refresh_token.is_none());
    }

    #[test]
    fn rejected_credentials_surface_the_cloud_message() {
        // 真实云端的错误信封：{code, message, data:null}
        let (endpoint, _) = mock_cloud(
            "401 Unauthorized",
            r#"{"code":401,"message":"账号或密码错误","data":null}"#,
        );

        let error = login(&endpoint, "zhangsan", "wrong", "client-1").unwrap_err();
        assert!(matches!(error, LoginError::Rejected(_)), "{error:?}");
        assert_eq!(error.message(), "账号或密码错误");
    }

    #[test]
    fn success_envelope_without_data_is_a_protocol_error() {
        let (endpoint, _) = mock_cloud("200 OK", r#"{"code":0,"message":"success","data":null}"#);
        let error = login(&endpoint, "admin", "x", "client-1").unwrap_err();
        assert!(matches!(error, LoginError::Protocol(_)), "{error:?}");
    }

    #[test]
    fn server_fault_is_retryable_not_a_credential_problem() {
        let (endpoint, _) = mock_cloud("500 Internal Server Error", r#"{"message":"boom"}"#);

        let error = login(&endpoint, "zhangsan", "hunter2", "client-1").unwrap_err();
        assert!(matches!(error, LoginError::Unreachable(_)), "{error:?}");
    }

    #[test]
    fn non_contract_response_is_reported_as_protocol_error() {
        let (endpoint, _) = mock_cloud("200 OK", r#"{"unexpected":true}"#);

        let error = login(&endpoint, "zhangsan", "hunter2", "client-1").unwrap_err();
        assert!(matches!(error, LoginError::Protocol(_)), "{error:?}");
    }
}
