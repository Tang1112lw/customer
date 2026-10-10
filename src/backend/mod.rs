#[path = "../shared/sha256.rs"]
pub(crate) mod sha256;
#[path = "../shared/cache.rs"]
pub(crate) mod cache;
#[path = "../shared/self_name.rs"]
pub(crate) mod self_name;
pub mod service;
pub mod tools_api;

use axum::{
    Router,
    body::Body,
    http::{Method, Request, Response, StatusCode, header},
    middleware::{self, Next},
};
use service::{Service, authorize};
use std::sync::Arc;

/// 允许哪些来源直接调用本客户端接口。
///
/// 桌面版走 Tauri IPC，用不到 CORS；但浏览器里的 AICRM 前端（默认跑在 5000 端口）
/// 要直接连本机客户端时，跨源必须显式放行 —— 否则「数据采集」按钮在浏览器里
/// 连请求都发不出去，只会看到 `isDesktopApp()` 抛的错。
///
/// **只放行本机回环地址**：客户端接口带 Bearer 令牌，放开到任意来源等于把它暴露给
/// 默认允许哪些来源调用本客户端接口。
///
/// **部署到客户机器时必须改**：客户的网页是云端的 HTTPS 域名，不是 localhost。
/// 用命令行的 `--allow-origin` 追加，或设环境变量 `AICRM_ALLOWED_ORIGINS`（逗号分隔）。
const DEFAULT_ALLOWED_ORIGINS: [&str; 4] = [
    "http://localhost:5000",
    "http://127.0.0.1:5000",
    "http://localhost:3000",
    "http://127.0.0.1:3000",
];

/// 客户端连的是哪个云端，就信任来自同一云端地址的网页 —— 页面发起的采集调用
/// 天然只应该来自用户自己部署的前端。基地址支持文件（隧道地址会变），所以
/// 这里存的是「来源」而不是解析结果：每次校验都现读现算，改文件立即生效。
static CLOUD_ORIGIN_SOURCE: std::sync::OnceLock<Option<(Option<String>, Option<std::path::PathBuf>)>> =
    std::sync::OnceLock::new();

/// 注册云端基地址来源：`--cloud-url` / `--cloud-url-file` 的值。
pub fn set_cloud_origin(static_url: Option<String>, file: Option<std::path::PathBuf>) {
    let _ = CLOUD_ORIGIN_SOURCE.set(Some((static_url, file)));
}

/// 从云端基地址推导出允许的网页来源（`scheme://host[:port]`）。
fn cloud_origin() -> Option<String> {
    let (static_url, file) = CLOUD_ORIGIN_SOURCE.get()?.as_ref()?;
    let base_url = match file {
        Some(path) => std::fs::read_to_string(path).ok()?.trim().to_owned(),
        None => static_url.clone()?,
    };
    if base_url.is_empty() {
        return None;
    }
    let (scheme, rest) = base_url.split_once("://")?;
    let authority = rest.split('/').next()?;
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}

/// 运行时来源白名单。由 `main` 启动时注入；没注入就退回默认值。
static ALLOWED_ORIGINS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// 设定来源白名单：环境变量 + 命令行追加值。
pub fn set_allowed_origins(extra: Vec<String>) {
    let mut list: Vec<String> = std::env::var("AICRM_ALLOWED_ORIGINS")
        .map(|value| {
            value
                .split(',')
                .map(|item| item.trim().to_owned())
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_else(|_| {
            DEFAULT_ALLOWED_ORIGINS
                .iter()
                .map(|item| (*item).to_owned())
                .collect()
        });
    for origin in extra {
        let origin = origin.trim().to_owned();
        if !origin.is_empty() && !list.contains(&origin) {
            list.push(origin);
        }
    }
    let _ = ALLOWED_ORIGINS.set(list);
}

fn origin_allowed(origin: &str) -> bool {
    if ALLOWED_ORIGINS
        .get()
        .map(|list| list.iter().any(|item| item == origin))
        .unwrap_or_else(|| DEFAULT_ALLOWED_ORIGINS.contains(&origin))
    {
        return true;
    }
    // 云端同源放行：网页来自客户端所连的云端地址（host:port 完全一致）才通过，
    // 路径、协议差异都算不同来源。
    cloud_origin().is_some_and(|allowed| allowed == origin)
}

/// 给响应补上 CORS 头，并直接应答预检。
async fn cors(request: Request<Body>, next: Next) -> Response<Body> {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let allowed = origin.as_deref().is_some_and(origin_allowed);

    // 预检不进入业务路由：它不带 Authorization，交给鉴权中间件只会得到 401
    if request.method() == Method::OPTIONS {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NO_CONTENT;
        decorate(&mut response, allowed, origin.as_deref());
        return response;
    }

    let mut response = next.run(request).await;
    decorate(&mut response, allowed, origin.as_deref());
    response
}

/// 只有来源在白名单里才回 `Allow-Origin`。刻意不用 `*` —— 接口需要凭据。
fn decorate(response: &mut Response<Body>, allowed: bool, origin: Option<&str>) {
    let headers = response.headers_mut();
    if allowed && let Some(origin) = origin {
        if let Ok(value) = origin.parse() {
            headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
        }
        headers.insert(header::VARY, header::ORIGIN.into());
    }
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        "GET, POST, PUT, DELETE, OPTIONS".parse().unwrap(),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "Authorization, Content-Type".parse().unwrap(),
    );
    headers.insert(header::ACCESS_CONTROL_MAX_AGE, "600".parse().unwrap());
    // 私有网络访问（PNA）：客户的网页是公网 HTTPS 域名，要访问本机 127.0.0.1，
    // Chrome 会先发预检并要求这个头。不回的话请求被浏览器直接拦掉，
    // 而且服务端连日志都没有 —— 请求根本没发出去。
    // 头名必须小写（from_static 的要求）。
    headers.insert(
        header::HeaderName::from_static("access-control-allow-private-network"),
        header::HeaderValue::from_static("true"),
    );
}

/// 组装全部 HTTP 路由，并在**顶层统一施加 Bearer 鉴权**。
///
/// 新增一个模块只需要两步：
///
/// 1. 在模块里写 `pub fn router() -> Router<Arc<Service>>`，**不要**自己调 `with_state`，
///    也**不要**写任何鉴权代码；
/// 2. 在下面加一行 `.merge(你的模块::router())`。
///
/// 鉴权由这里唯一的 `route_layer` 覆盖，所以模块内每个接口都自动受保护，加多少接口都不用管。
///
/// 用 `route_layer` 而不是 `layer`：前者只作用于已注册的路由，未匹配的路径仍然是 404
/// 而不是 401，不会把 404 伪装成鉴权失败。
pub fn app(service: Arc<Service>) -> Router {
    // 需要令牌的部分
    let guarded = Router::new()
        .merge(tools_api::router())
        // 以后的新模块加在这里，例如：.merge(admin_api::router())
        //
        // 注意：所有 `.merge(...)` 都必须写在这一行之前。
        .route_layer(middleware::from_fn_with_state(service.clone(), authorize));

    Router::new()
        // 握手端点单独挂，刻意不经过上面的鉴权层 —— 它存在的意义就是让前端先拿到令牌
        .route("/api/aicrm/session", axum::routing::get(session))
        .merge(guarded)
        .with_state(service)
        // CORS 放最外层：预检请求不带令牌，必须在鉴权之前被拦下并直接应答
        .layer(middleware::from_fn(cors))
}

/// 浏览器里的前端要调用本机客户端，但它读不到 `api.token` 文件（网页没有文件系统权限），
/// 所以需要一个握手端点把令牌交给它。
///
/// **这不是把接口敞开**：只有 `Origin` 在白名单里才返回令牌 —— 浏览器一定会带 `Origin`，
/// 且页面脚本改不了它，所以恶意网页拿不到。本机上的其它进程本来就能直接读令牌文件，
/// 威胁面没有变大。
///
/// 桌面版走 Tauri IPC，用不到这个端点。
async fn session(
    axum::extract::State(service): axum::extract::State<Arc<Service>>,
    request: Request<Body>,
) -> Response<Body> {
    let allowed = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(origin_allowed);
    if !allowed {
        let body = serde_json::json!({
            "success": false,
            "error_code": "FORBIDDEN_ORIGIN",
            "message": "只有本机 AICRM 前端可以获取客户端令牌",
        });
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
    }
    // 只回令牌与基地址，不回密钥、账号等任何业务数据
    let body = serde_json::json!({
        "success": true,
        "token": service.token(),
        "base_url": "http://127.0.0.1:8765",
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use http_body_util::BodyExt;
    use std::fs;
    use tower::ServiceExt;

    /// 全部已注册接口。新增接口时同步补进这张表，用来确认它同样被顶层鉴权覆盖。
    /// 不含 `GET /api/aicrm/session`：那是刻意挂在鉴权层之外的握手端点，见 [`super::session`]。
    const ROUTES: [(&str, &str); 8] = [
        ("GET", "/api/aicrm/key/version"),
        ("GET", "/api/aicrm/export/version"),
        ("GET", "/api/aicrm/key/help"),
        ("GET", "/api/aicrm/export/help"),
        ("GET", "/api/aicrm/accounts"),
        ("POST", "/api/aicrm/prepare"),
        ("POST", "/api/aicrm/export"),
        ("POST", "/api/aicrm/collect"),
    ];

    /// 用空的同名文件顶替两个辅助程序，只为通过 `Service::new` 的存在性检查。
    /// 鉴权在 handler 之前就返回，因此测试不会真正执行它们。
    fn test_app() -> (Router, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let tools = temp.path().join("tools");
        fs::create_dir_all(&tools).unwrap();
        // 用与 Service::new 相同的常量，避免换了辅助程序名后测试夹具悄悄脱节
        for name in [
            service::key_service::KEY_PROGRAM,
            service::export_service::EXPORT_PROGRAM,
        ] {
            fs::write(tools.join(name), b"").unwrap();
        }
        let token_file = temp.path().join("api.token");
        fs::write(&token_file, "t".repeat(32)).unwrap();
        let service = Service::new(
            tools,
            temp.path().join("accounts"),
            temp.path().join("data"),
            token_file,
        )
        .unwrap();
        (app(service), temp)
    }

    fn request(method: &str, path: &str, authorization: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(value) = authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        builder.body(Body::empty()).unwrap()
    }

    /// 顶层 `route_layer` 必须拦住每一个接口：无 token、错 token、其它认证方案都返回 401。
    #[tokio::test]
    async fn every_route_rejects_missing_or_wrong_token() {
        let (app, _temp) = test_app();
        for (method, path) in ROUTES {
            for authorization in [None, Some("Bearer wrong-token"), Some("Basic dXNlcjpwYXNz")] {
                let response = app
                    .clone()
                    .oneshot(request(method, path, authorization))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "{method} {path} 没有被顶层鉴权拦住（authorization={authorization:?}）"
                );
            }
        }
    }

    /// 正确的 token 必须放行到 handler。辅助程序是空文件，所以结果会是 502，
    /// 但绝不能是 401 —— 否则说明鉴权写死了。
    #[tokio::test]
    async fn valid_token_reaches_the_handler() {
        let (app, _temp) = test_app();
        let authorization = format!("Bearer {}", "t".repeat(32));
        let response = app
            .oneshot(request(
                "GET",
                "/api/aicrm/key/version",
                Some(&authorization),
            ))
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// 未匹配的路径保持 404：证明用的是 `route_layer`，不是 `layer`。
    #[tokio::test]
    async fn unknown_path_stays_404_instead_of_401() {
        let (app, _temp) = test_app();
        let response = app
            .oneshot(request("GET", "/api/aicrm/does-not-exist", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// 401 响应体沿用 README 约定的统一错误结构。
    #[tokio::test]
    async fn unauthorized_body_keeps_the_unified_error_shape() {
        let (app, _temp) = test_app();
        let response = app
            .oneshot(request("GET", "/api/aicrm/accounts", None))
            .await
            .unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["success"], false);
        assert_eq!(body["error_code"], "UNAUTHORIZED");
        assert!(body["message"].is_string());
    }
}
