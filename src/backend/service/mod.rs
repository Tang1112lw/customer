//! 共享底座：错误类型、服务状态、辅助程序调用与 Bearer 鉴权。
//!
//! 按辅助程序拆分的业务逻辑位于子模块：
//! - [`key_service`]：`AICRM-Key.exe`，密钥准备与本地账号查询
//! - [`export_service`]：`aicrm-export-new.exe`，导出执行（Markdown 输出）
//! - [`archive_service`]：`AICRM-Export.exe` + 归档上传，数据采集闭环的上半段

pub mod agent_service;
pub mod archive_service;
pub mod export_service;
pub mod key_service;
pub mod state_service;

pub use archive_service::ArchiveRequest;
pub use export_service::ExportRequest;
pub use key_service::PrepareRequest;
// 注意：state_service::State 不在这里重导出 —— axum::extract::State 也叫 State，
// 重导出会和它撞名，路由那一层全是 `State<Arc<Service>>`。要用就写全路径。

use axum::{
    Json,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{fs, io, path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{process::Command, sync::RwLock, time::timeout};

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: String,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
        }
    }
    /// 错误码，供命令通道回传给云端。
    pub(crate) fn code(&self) -> &str {
        &self.code
    }

    /// 错误描述，供命令通道回传给云端。
    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "ARGUMENT_ERROR", message)
    }
    pub(crate) fn helper(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "HELPER_ERROR", message)
    }
    pub(crate) fn io() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "IO_ERROR",
            "文件读写失败",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"success":false,"error_code":self.code,"message":self.message})),
        )
            .into_response()
    }
}

pub struct Service {
    tools: PathBuf,
    keys: PathBuf,
    data: PathBuf,
    /// 采集状态（水位线 / 上传记录 / 机主昵称）。见 [`state_service`]。
    state: state_service::State,
    token: String,
    gate: RwLock<()>,
}

impl Service {
    /// 本地接口令牌。只给 `session` 握手端点用 —— 它要在校验来源后把令牌交给本机前端。
    /// 别在别处散播：这个值的唯一用途就是做 Bearer 鉴权。
    pub fn token(&self) -> &str {
        &self.token
    }

    /// 机主微信 wxid：密钥目录下第一个 `wxid_` 开头的子目录名。
    ///
    /// 水位线必须按**机主**账号记。归档包里的 `chat.id` 是会话对方的，
    /// 拿它当自己会让水位线记到别人头上。
    pub(crate) fn self_wxid(&self) -> String {
        let Ok(entries) = fs::read_dir(&self.keys) else {
            return String::new();
        };
        entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .find(|name| name.starts_with("wxid_"))
            .unwrap_or_default()
    }
}

impl Service {
    pub fn new(
        tools: PathBuf,
        keys: PathBuf,
        data: PathBuf,
        token_file: PathBuf,
    ) -> io::Result<Arc<Self>> {
        for name in [key_service::KEY_PROGRAM, export_service::EXPORT_PROGRAM] {
            if !tools.join(name).is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("缺少 {name}"),
                ));
            }
        }
        fs::create_dir_all(&keys)?;
        fs::create_dir_all(data.join("jobs"))?;
        let state = state_service::State::new(data.join("state"))?;
        let token = fs::read_to_string(token_file)?.trim().to_owned();
        if token.len() < 32 || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "API token 至少需要 32 个可见 ASCII 字符",
            ));
        }
        Ok(Arc::new(Self {
            // Go SQLite rejects Windows extended paths (\\?\...) as file URIs.
            tools: normal_absolute(tools)?,
            keys: normal_absolute(keys)?,
            data: normal_absolute(data)?,
            state,
            token,
            gate: RwLock::new(()),
        }))
    }

    pub(crate) async fn run(
        &self,
        program: &str,
        args: Vec<String>,
        limit: Duration,
    ) -> ApiResult<Value> {
        let mut command = Command::new(self.tools.join(program));
        command
            .args(args)
            .current_dir(&self.tools)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let output = timeout(limit, command.output())
            .await
            .map_err(|_| {
                ApiError::new(
                    StatusCode::GATEWAY_TIMEOUT,
                    "HELPER_TIMEOUT",
                    "辅助程序执行超时",
                )
            })?
            .map_err(|_| ApiError::helper("辅助程序启动失败"))?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|_| ApiError::helper("辅助程序输出不是 UTF-8"))?;
        let result = stdout
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            // 兼容两种结果协议：
            // - 旧程序 AICRM-Export.exe 输出 `{"type":"result","success":…}` 行
            // - 新程序 aicrm-export-new.exe 直接输出单个结果对象（无 type 字段）
            .filter(|v| {
                v.get("type").and_then(Value::as_str) == Some("result")
                    || v.get("success").is_some()
            })
            .last()
            .ok_or_else(|| ApiError::helper("辅助程序没有返回最终结果"))?;
        if !output.status.success() || result["success"] != true {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                result["error_code"].as_str().unwrap_or("HELPER_ERROR"),
                result["message"].as_str().unwrap_or("辅助程序执行失败"),
            ));
        }
        Ok(result)
    }

    pub(crate) async fn version(&self, program: &str) -> ApiResult<Value> {
        let output = Command::new(self.tools.join(program))
            .args(["version", "--json"])
            .output()
            .await
            .map_err(|_| ApiError::helper("版本查询失败"))?;
        if !output.status.success() {
            return Err(ApiError::helper("版本查询失败"));
        }
        serde_json::from_slice(&output.stdout).map_err(|_| ApiError::helper("版本输出无效"))
    }

    pub async fn help(&self, program: &str) -> ApiResult<String> {
        let output = Command::new(self.tools.join(program))
            .arg("help")
            .output()
            .await
            .map_err(|_| ApiError::helper("帮助查询失败"))?;
        if !output.status.success() {
            return Err(ApiError::helper("帮助查询失败"));
        }
        String::from_utf8(output.stdout).map_err(|_| ApiError::helper("帮助输出无效"))
    }
}

pub async fn authorize(
    State(service): State<Arc<Service>>,
    request: Request,
    next: Next,
) -> Response {
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if supplied != Some(format!("Bearer {}", service.token).as_str()) {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "缺少或错误的 Bearer token",
        )
        .into_response();
    }
    next.run(request).await
}

/// 把阻塞的 IO/网络调用挪到阻塞线程池，别占住 tokio 的工作线程。
/// 归档遍历与 multipart 上传都是同步实现，走这里。
pub(crate) async fn run_blocking<T, F>(job: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> ApiResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|_| ApiError::io())?
}

pub(crate) fn option(args: &mut Vec<String>, flag: &str, value: Option<String>) -> ApiResult<()> {
    if let Some(value) = value {
        if value.len() > 4096 || value.contains('\0') {
            return Err(ApiError::bad("参数过长或包含无效字符"));
        }
        if !value.is_empty() {
            args.extend([flag.into(), value]);
        }
    }
    Ok(())
}

fn normal_absolute(path: PathBuf) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// 把 Unix 秒格式化成 RFC3339（UTC）。
///
/// 云端的时间字段是 Go 的 `time.Time`，只认 RFC3339；给 epoch 数字会直接 400。
/// 这里不引日期库，用 Howard Hinnant 的 civil_from_days 换算。
pub(crate) fn rfc3339(epoch_seconds: i64) -> String {
    let days = epoch_seconds.div_euclid(86_400);
    let secs = epoch_seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// 当前时刻的 RFC3339（UTC）字符串。
pub(crate) fn now_rfc3339() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    rfc3339(seconds)
}

/// 今天的日期，`YYYY-MM-DD`。水位线用这个粒度，与导出程序的 `--start` 一致。
pub(crate) fn today() -> String {
    now_rfc3339()[..10].to_owned()
}
