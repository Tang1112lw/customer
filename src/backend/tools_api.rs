use super::service::{
    ArchiveRequest, ExportRequest, PrepareRequest, Service, export_service::EXPORT_PROGRAM,
    key_service::KEY_PROGRAM,
};
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use serde_json::json;
use std::sync::Arc;

/// 注册本模块的全部路由。
///
/// 只管"路径 → handler"的映射：不调 `with_state`，也不写鉴权。
/// Bearer 鉴权由 [`super::app`] 在顶层统一施加，因此这里新增接口无需任何额外处理。
pub fn router() -> Router<Arc<Service>> {
    Router::new()
        .route(
            "/api/aicrm/key/version",
            get(|State(s): State<Arc<Service>>| async move { s.key_version().await.map(Json) }),
        )
        .route(
            "/api/aicrm/export/version",
            get(|State(s): State<Arc<Service>>| async move { s.export_version().await.map(Json) }),
        )
        .route(
            "/api/aicrm/key/help",
            get(|State(s): State<Arc<Service>>| async move { s.help(KEY_PROGRAM).await }),
        )
        .route(
            "/api/aicrm/export/help",
            get(|State(s): State<Arc<Service>>| async move { s.help(EXPORT_PROGRAM).await }),
        )
        .route(
            "/api/aicrm/accounts",
            get(|State(s): State<Arc<Service>>| async move { s.accounts().map(Json) }),
        )
        .route(
            "/api/aicrm/prepare",
            post(
                |State(s): State<Arc<Service>>, Json(r): Json<PrepareRequest>| async move {
                    let _job = super::service::agent_service::JobGuard::new();
                    s.prepare(r).await.map(Json)
                },
            ),
        )
        .route(
            "/api/aicrm/export",
            post(
                |State(s): State<Arc<Service>>, Json(r): Json<ExportRequest>| async move {
                    let _job = super::service::agent_service::JobGuard::new();
                    s.export_files(r).await.map(Json)
                },
            ),
        )
        // 数据采集：导出聊天记录并上传到云端任务。浏览器里的前端点「数据采集」就走这里。
        //
        // **必须立刻返回，不能 await 到底**：导出要跑几分钟，挂在请求里的话浏览器
        // 一切页面/刷新/关标签，连接断开、handler 被丢弃，整个采集就被取消了 ——
        // 表现是归档目录写到一半突然不动、云端任务永远停在 created。实测踩过。
        // 所以这里只把活儿丢进后台，立即回 202；前端照旧轮询 GET /api/tasks/{id} 看进度。
        .route(
            "/api/aicrm/collect",
            post(
                |State(s): State<Arc<Service>>, Json(r): Json<ArchiveRequest>| async move {
                    let task_id = r.task_id.clone();
                    let reported = task_id.clone();
                    tokio::spawn(async move {
                        // 采集是「重活」：自更新要等它结束才重启客户端，否则归档会写一半。
                        let _job = super::service::agent_service::JobGuard::new();
                        match s.export_and_upload(r).await {
                            Ok(_) => eprintln!("采集任务 {task_id} 已完成并上传"),
                            Err(error) => {
                                eprintln!("采集任务 {task_id} 失败：{}", error.message())
                            }
                        }
                    });
                    Ok::<_, super::service::ApiError>(Json(json!({
                        "accepted": true,
                        "task_id": reported,
                        "message": "采集已在后台开始，请通过任务状态查看进度",
                    })))
                },
            ),
        )
}
