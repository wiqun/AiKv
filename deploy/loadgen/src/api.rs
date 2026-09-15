//! HTTP 控制面: 单页 UI / 配置读写 / 探活.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::catalog::{heavy_hint, CATALOG};
use crate::config::{ConfigPatch, WorkloadConfig};
use crate::engine::RuntimeState;
use crate::ui::{INDEX_HTML, WIQUN_SVG};

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
        .route("/wiqun.svg", get(wiqun_icon))
        .route("/favicon.ico", get(wiqun_icon))
        .route("/health", get(health))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/commands", get(list_commands))
        .with_state(app)
}

async fn index() -> impl IntoResponse {
    ([(CACHE_CONTROL, "no-store")], Html(INDEX_HTML))
}

async fn wiqun_icon() -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, "image/svg+xml; charset=utf-8"),
            (CACHE_CONTROL, "no-store"),
        ],
        WIQUN_SVG,
    )
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

#[derive(Debug, Serialize)]
struct CommandView {
    name: &'static str,
    mix_key: &'static str,
    group: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    heavy_hint: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct CommandsResponse {
    commands: Vec<CommandView>,
}

async fn list_commands() -> Json<CommandsResponse> {
    Json(CommandsResponse {
        commands: CATALOG
            .iter()
            .map(|s| CommandView {
                name: s.redis,
                mix_key: s.mix_key,
                group: s.group,
                heavy_hint: heavy_hint(s),
            })
            .collect(),
    })
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
    if patch.running == Some(true) {
        crate::conn::check_ready(&next).await.map_err(bad_request)?;
        app.state.resume_dispatch().await;
        app.state.bump_run_epoch();
    } else if patch.running == Some(false) {
        app.state.resume_dispatch().await;
    } else if patch.paused == Some(true) {
        if !next.running {
            return Err(bad_request("未运行不能暂停".into()));
        }
        app.state.set_user_paused(true);
    } else if patch.paused == Some(false) {
        app.state.resume_dispatch().await;
    }
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
            state: runtime_state_label(config.running, app.state.is_paused()),
            workers: app.state.workers.load(Ordering::Relaxed),
            endpoints,
            last_error,
        },
        config: (*config).clone(),
    }
}

fn runtime_state_label(running: bool, paused: bool) -> &'static str {
    if !running {
        "stopped"
    } else if paused {
        "paused"
    } else {
        "running"
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

    #[test]
    fn runtime_state_label_covers_pause() {
        assert_eq!(runtime_state_label(false, true), "stopped");
        assert_eq!(runtime_state_label(true, true), "paused");
        assert_eq!(runtime_state_label(true, false), "running");
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
        let cache = response
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(cache, "no-store");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("<html"));
        assert!(
            html.contains("readForm(), running: true"),
            "启动必须提交整张表单, 不能只发 running"
        );
        assert!(html.contains("id=\"endpoint_host\""));
        assert!(html.contains("id=\"endpoint_port\""));
        assert!(html.contains("rel=\"icon\""));
        assert!(html.contains("src=\"/wiqun.svg\""));
        assert!(!html.contains("endpoints_input"));
        assert!(
            html.contains("/api/commands"),
            "前端必须从引擎目录拉取可发压命令"
        );
        assert!(html.contains("未运行"), "未开泵应为灰色未运行");
        assert!(html.contains("id=\"mode_badge\""), "顶栏应显示 Cluster/Single");
        assert!(html.contains("id=\"task_info_btn\""), "运行中/暂停徽章内应有当前任务叹号");
        assert!(html.contains("id=\"btn_primary\""), "顶栏只留一颗主按钮");
        assert!(html.contains("id=\"task_stop_btn\""), "终止任务放在状态徽章上");
        assert!(html.contains("paused: true"), "运行中点暂停而不是拆泵");
        assert!(html.contains("paused: false"), "暂停后点继续恢复派发");
        assert!(html.contains("命令占比"));
        assert!(!html.contains("id=\"task_params\""));
        let endpoints_at = html.find("id=\"endpoints\"").expect("endpoints");
        let mode_at = html.find("id=\"mode_badge\"").expect("mode_badge");
        let state_at = html.find("id=\"state\"").expect("state");
        assert!(
            endpoints_at < mode_at && mode_at < state_at,
            "探活 → 拓扑 → 状态"
        );
        assert!(!html.contains("triggerReload"), "刷新按钮已删除, 轮询已足够");
        assert!(html.contains("id=\"dirty_hint\""), "运行中改表单应有草稿未生效提示");
        assert!(html.contains("id=\"heavy_dialog\""), "重命令选择时应弹窗提醒且不拦截");
        assert!(
            !html.contains("patch.readonly") && !html.contains("id=\"readonly\""),
            "引擎 readonly 开关不得出现在控制台"
        );
    }

    #[tokio::test]
    async fn serves_wiqun_icon() {
        let response = app()
            .oneshot(Request::get("/wiqun.svg").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let ctype = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ctype.starts_with("image/svg+xml"));
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let svg = String::from_utf8_lossy(&bytes);
        assert!(svg.contains("<svg"));
        assert!(svg.contains("orange-grad"));
    }

    #[tokio::test]
    async fn get_config_returns_defaults() {
        let response = app()
            .oneshot(Request::get("/api/config").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["config"]["connections"], 8);
        assert_eq!(json["config"]["target_ops"], 5000);
        assert_eq!(json["runtime"]["state"], "stopped");
        assert!(json["config"].get("readonly").is_none());
    }

    #[tokio::test]
    async fn lists_dispatchable_aikv_commands() {
        let response = app()
            .oneshot(Request::get("/api/commands").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        let names: Vec<&str> = json["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        for need in ["GET", "HGET", "HSET", "MSET", "JSON.GET", "JSON.SET", "LPUSH", "ZADD"] {
            assert!(names.contains(&need), "missing {need}");
        }
        assert!(!names.iter().any(|n| *n == "FLUSHALL"));
        assert!(!names.iter().any(|n| *n == "KEYS"), "KEYS 不进入 mix 目录");
        assert!(json["commands"].as_array().unwrap().len() > 80);
        let info = json["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["mix_key"] == "info")
            .expect("INFO");
        assert!(info["heavy_hint"].as_str().unwrap().contains("统计"));
        let get = json["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["mix_key"] == "get")
            .expect("GET");
        assert!(get.get("heavy_hint").is_none());
    }

    #[tokio::test]
    async fn put_config_applies_patch() {
        let request = Request::put("/api/config")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"target_ops": 8000}"#))
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["config"]["target_ops"], 8000);
        assert_eq!(json["config"]["running"], false);
        assert_eq!(json["config"]["connections"], 8);
    }

    #[tokio::test]
    async fn put_without_start_does_not_bump_epoch() {
        let state = Arc::new(RuntimeState::default());
        let app = router(AppState {
            cfg: Arc::new(ArcSwap::from_pointee(WorkloadConfig::default())),
            state: state.clone(),
        });
        let request = Request::put("/api/config")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"target_ops": 5000}"#))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.run_epoch(), 0);
    }

    #[tokio::test]
    async fn put_pause_keeps_running_and_does_not_bump_epoch() {
        let state = Arc::new(RuntimeState::default());
        let cfg = WorkloadConfig {
            running: true,
            ..Default::default()
        };
        let app = router(AppState {
            cfg: Arc::new(ArcSwap::from_pointee(cfg)),
            state: state.clone(),
        });
        let request = Request::put("/api/config")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"paused": true}"#))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["config"]["running"], true);
        assert_eq!(json["runtime"]["state"], "paused");
        assert_eq!(state.run_epoch(), 0);
        assert!(state.is_paused());
    }

    #[tokio::test]
    async fn put_pause_when_stopped_is_rejected() {
        let request = Request::put("/api/config")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"paused": true}"#))
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = body_json(response).await;
        assert!(json["error"].as_str().unwrap().contains("未运行不能暂停"));
    }

    #[tokio::test]
    async fn put_resume_clears_user_pause_without_bump() {
        let state = Arc::new(RuntimeState::default());
        state.set_user_paused(true);
        let cfg = WorkloadConfig {
            running: true,
            ..Default::default()
        };
        let app = router(AppState {
            cfg: Arc::new(ArcSwap::from_pointee(cfg)),
            state: state.clone(),
        });
        let request = Request::put("/api/config")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"paused": false}"#))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["config"]["running"], true);
        assert_eq!(json["runtime"]["state"], "running");
        assert_eq!(state.run_epoch(), 0);
        assert!(!state.is_paused());
    }

    #[tokio::test]
    async fn put_start_failure_does_not_bump_epoch() {
        let state = Arc::new(RuntimeState::default());
        let app = router(AppState {
            cfg: Arc::new(ArcSwap::from_pointee(WorkloadConfig::default())),
            state: state.clone(),
        });
        let request = Request::put("/api/config")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"running":true,"mode":"single","endpoints":["127.0.0.1:1"],"timeout_ms":50}"#,
            ))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(state.run_epoch(), 0);
    }

    #[tokio::test]
    async fn put_config_rejects_start_when_seed_down() {
        let request = Request::put("/api/config")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"running":true,"mode":"single","endpoints":["127.0.0.1:1"],"timeout_ms":50}"#,
            ))
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = body_json(response).await;
        let err = json["error"].as_str().unwrap();
        assert!(err.contains("拒绝启动") || err.contains("不通"), "{err}");
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
