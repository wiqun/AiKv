//! HTTP 控制面: 单页 UI / 配置读写 / 探活.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::config::{ConfigPatch, WorkloadConfig};
use crate::engine::RuntimeState;
use crate::ui::INDEX_HTML;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<ArcSwap<WorkloadConfig>>,
    pub state: Arc<RuntimeState>,
}

#[derive(Debug, Serialize)]
pub struct ConfigResponse {
    pub config: WorkloadConfig,
    pub runtime: RuntimeView,
}

#[derive(Debug, Serialize)]
pub struct RuntimeView {
    pub state: &'static str,
    pub workers: u64,
    pub endpoints: Vec<crate::engine::EndpointStatus>,
    pub last_error: Option<ErrorView>,
}

#[derive(Debug, Serialize)]
pub struct ErrorView {
    pub message: String,
    pub ago_seconds: u64,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

pub fn router(app: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/api/config", get(get_config).put(put_config))
        .with_state(app)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn get_config(State(app): State<AppState>) -> Json<ConfigResponse> {
    Json(view(&app).await)
}

async fn put_config(
    State(app): State<AppState>,
    payload: Result<Json<ConfigPatch>, JsonRejection>,
) -> Result<Json<ConfigResponse>, (StatusCode, Json<ErrorResponse>)> {
    let Json(patch) = payload.map_err(|err| bad_request(format!("请求体解析失败: {err}")))?;
    let current = app.cfg.load_full();
    let next = current
        .patched(&patch)
        .map_err(|err| bad_request(err.to_string()))?;
    app.cfg.store(Arc::new(next));
    Ok(Json(view(&app).await))
}

async fn view(app: &AppState) -> ConfigResponse {
    let config = app.cfg.load_full();
    let endpoints = app.state.endpoints().await;
    let last_error = app.state.last_error().await.map(|info| ErrorView {
        message: info.message,
        ago_seconds: now_unix().saturating_sub(info.at_unix),
    });
    ConfigResponse {
        runtime: RuntimeView {
            state: if config.running { "running" } else { "stopped" },
            workers: app.state.workers.load(Ordering::Relaxed),
            endpoints,
            last_error,
        },
        config: (*config).clone(),
    }
}

fn bad_request(message: String) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse { error: message }),
    )
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app() -> Router {
        let cfg = Arc::new(ArcSwap::from_pointee(WorkloadConfig::default()));
        router(AppState {
            cfg,
            state: Arc::new(RuntimeState::default()),
        })
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn health_is_ok() {
        let response = app()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn index_serves_html() {
        let response = app()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("<html"));
    }

    #[tokio::test]
    async fn get_config_returns_defaults() {
        let response = app()
            .oneshot(Request::get("/api/config").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["config"]["connections"], 32);
        assert_eq!(json["config"]["target_ops"], 20000);
        assert_eq!(json["runtime"]["state"], "stopped");
    }

    #[tokio::test]
    async fn put_config_applies_patch() {
        let request = Request::put("/api/config")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"target_ops": 5000, "running": true}"#))
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["config"]["target_ops"], 5000);
        assert_eq!(json["config"]["running"], true);
        assert_eq!(json["config"]["connections"], 32);
    }

    #[tokio::test]
    async fn put_config_rejects_invalid_value() {
        let request = Request::put("/api/config")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"connections": 0}"#))
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = body_json(response).await;
        assert!(json["error"].as_str().unwrap().contains("connections"));
    }

    #[tokio::test]
    async fn put_config_rejects_unknown_field() {
        let request = Request::put("/api/config")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"no_such_field": 1}"#))
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
