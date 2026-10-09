//! 共享底座：错误类型、服务状态、辅助程序调用与 Bearer 鉴权。
//!
//! 按辅助程序拆分的业务逻辑位于子模块：
//! - [`key_service`]：`AICRM-Key.exe`，密钥准备与本地账号查询
//! - [`export_service`]：`aicrm-export-new.exe`，导出执行（Markdown 输出）
//! - [`archive_service`]：Markdown 归档打包上传，数据采集闭环的上半段

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
use std::{fs, io, path::Path, path::PathBuf, process::Stdio, sync::Arc, time::Duration};
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

    /// 机主微信 wxid：密钥目录下**最近一次抓到**的那个账号。
    ///
    /// 水位线必须按**机主**账号记。归档包里的 `chat.id` 是会话对方的，
    /// 拿它当自己会让水位线记到别人头上。
    pub(crate) fn self_wxid(&self) -> String {
        newest_account_dir(&self.keys).unwrap_or_default()
    }
}

/// 选「本机当前的微信号目录」：取**最近一次完成抓取**的那一个。
///
/// 一台机器换过微信账号时，密钥目录下会同时留着旧号的目录；按枚举顺序
/// 取第一个会一直拿到旧号 —— 实测踩过：换号并重新绑定成功后，心跳、
/// 控制台、复用判断全都还指着旧号，网页显示「未关联客户端」「尚未绑定微信」。
/// 密钥程序每次抓取都会重写该账号目录里的 config.json，用它的落盘时间排序
/// 就能稳定取到当前登录的号。
pub(crate) fn newest_account_dir(keys: &Path) -> Option<String> {
    let entries = fs::read_dir(keys).ok()?;
    let mut newest: Option<(String, std::time::SystemTime)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if !name.starts_with("wxid_") || !path.is_dir() {
            continue;
        }
        // 抓取时间：优先 config.json 的落盘时间（抓取时重写），退而取目录时间。
        let stamp = fs::metadata(path.join("config.json"))
            .and_then(|meta| meta.modified())
            .or_else(|_| fs::metadata(&path).and_then(|meta| meta.modified()))
            .ok();
        let Some(stamp) = stamp else {
            continue;
        };
        if newest.as_ref().map(|(_, best)| stamp > *best).unwrap_or(true) {
            newest = Some((name, stamp));
        }
    }
    newest.map(|(name, _)| name)
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
        // 记下自己的启动命令：自更新后由 aicrm-update.exe 原样重放。
        // 重拼一遍服务参数只会引入"拼错就采不到数据"的风险。
        record_startup_command(&data);
        // 界面程序靠这个文件显示"服务实际在跑的版本"。
        record_service_version(&data);
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
///
/// 按东八区计算：导出程序把 `--start` / `--end` 理解成本地日期，而 `rfc3339`
/// 输出的是 UTC —— 直接用会在这边早上八点前把「今天」算成昨天，导致当天数据漏采。
pub(crate) fn today() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    rfc3339(seconds + 8 * 3600)[..10].to_owned()
}

/// 解密缓存（聊天数据库）是否已经生成。
///
/// **路径必须是 `<key-dir>/cache/`**。AICRM-Key.exe 抓完密钥后把解密出来的库放在
/// 这里，文件名是库路径的哈希（如 `451f45d20f33427e.db`），账号目录里的
/// `config.json` 写得明明白白：`"decrypted_dir": "cache"`，相对 key-dir 解析。
///
/// 之前只查 `<key-dir>/<wxid>/cache/` —— 那个目录从来不存在，于是**明明已经解密好
/// 的机器被一直判成「未找到本地数据库」**：云端 online 恒为 false、采集任务一条都
/// 下发不出去，界面还显示"密钥不完整"。老布局（wxid 子目录下）也一并认。
pub(crate) fn decrypted_cache_ready(keys: &Path, wxid: &str) -> bool {
    let candidates = [keys.join("cache"), keys.join(wxid).join("cache")];
    candidates.iter().any(|dir| has_db_file(dir))
}

fn has_db_file(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("db"))
    })
}

/// 把本次的启动命令行写进 `<data>/state/startup.json`。
///
/// 自更新时主进程会被杀掉，重新拉起需要与服务完全一致的参数（密钥目录、数据目录、
/// 云端地址、设备令牌…）。这些参数由安装脚本拼过一次，记下来重放比在别处再拼一遍可靠。
/// 写失败不影响启动 —— 只是自更新时会退回"只拉界面"。
pub(crate) fn record_startup_command(data: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let payload = serde_json::json!({
        "exe": exe.to_string_lossy(),
        "args": arguments,
    });
    let directory = data.join("state");
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    let _ = fs::write(
        directory.join("startup.json"),
        serde_json::to_vec_pretty(&payload).unwrap_or_default(),
    );
}

/// 把自己的版本号写进 `<data>/state/version.json`，供界面程序读取。
///
/// 界面（aicrm-console.exe）是**独立程序**，它右下角显示的版本原本取自自己的
/// 编译期常量 —— 而自更新历史上只替换服务端 exe，界面程序根本没被换过，
/// 于是"升完级版本号还是旧的"（实测被当成更新失败）。版本号只有一个真相源：
/// 正在跑的这份服务。界面读这个文件，就不再自己骗自己。
pub(crate) fn record_service_version(data: &Path) {
    let directory = data.join("state");
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    let payload = serde_json::json!({ "version": env!("CARGO_PKG_VERSION") });
    let _ = fs::write(
        directory.join("version.json"),
        serde_json::to_vec_pretty(&payload).unwrap_or_default(),
    );
}

/// 读「已就绪、等待下次启动应用」的版本号。None 表示没有待应用的更新。
///
/// 参数是**安装目录**（exe 所在目录），不是数据目录 —— `updates/` 与 `data/` 是
/// 安装目录下的两个平级目录。这里曾经错传成数据目录，导致启动检查永远读到 None，
/// 「下次启动应用更新」整条路径静默失效（诊断行会打印「待应用版本 None」）。
pub(crate) fn pending_update_version(install_dir: &Path) -> Option<String> {
    let raw = fs::read_to_string(install_dir.join("updates").join("pending.json")).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value
        .get("version")
        .and_then(|item| item.as_str())
        .map(str::to_owned)
        .filter(|item| !item.is_empty())
}

/// pending 是否比 current 更新。
///
/// 不能用 `!=` 判断：服务端若回退发布（或本地残留了一个更旧的标记），`!=` 会把
/// 客户端硬拉回旧版本。点分数字比较，任一侧解析不出来就按"不更新"处理。
pub(crate) fn newer_version(pending: &str, current: &str) -> bool {
    fn parts(value: &str) -> Option<Vec<u64>> {
        let text = value.trim().trim_start_matches(['v', 'V']);
        if text.is_empty() {
            return None;
        }
        let mut numbers = Vec::new();
        for segment in text.split('.') {
            let digits: String = segment.chars().take_while(|item| item.is_ascii_digit()).collect();
            if digits.is_empty() {
                return None;
            }
            numbers.push(digits.parse().ok()?);
        }
        Some(numbers)
    }
    let (Some(left), Some(right)) = (parts(pending), parts(current)) else {
        return false;
    };
    for index in 0..left.len().max(right.len()) {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        if a != b {
            return a > b;
        }
    }
    false
}

#[cfg(test)]
mod version_tests {
    use super::newer_version;

    /// 这类判断反复出过错（用 != 会把客户端拉回旧版本），所以把边界钉死在测试里。
    #[test]
    fn newer_version_边界() {
        assert!(newer_version("0.1.16", "0.1.15"));
        assert!(newer_version("0.2.0", "0.1.99"));
        assert!(newer_version("1.0.0", "0.9.9"));
        // 段数不同按缺位补 0
        assert!(newer_version("0.1.15.1", "0.1.15"));
        assert!(!newer_version("0.1.15", "0.1.15"));
        assert!(!newer_version("0.1.15", "0.1.16"));
        assert!(!newer_version("0.1", "0.1.0"));
        // 允许 v 前缀与 -beta 后缀
        assert!(newer_version("v0.1.16", "0.1.15"));
        assert!(newer_version("0.1.16-beta", "0.1.15"));
        // 解析不出来一律不更新（宁可不动，也不能乱动）
        assert!(!newer_version("", "0.1.15"));
        assert!(!newer_version("abc", "0.1.15"));
        assert!(!newer_version("0.1.16", ""));
    }
}
