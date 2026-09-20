use std::sync::Arc;

use axum::extract::{Path, Query, State, WebSocketUpgrade};
use axum::extract::ws::{Message, WebSocket};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::config::StrategyConfig;
use crate::engine::Engine;
use crate::types::StrategyId;

#[derive(Clone)]
pub struct AppState {
    pub engine: Arc<Engine>,
}

pub async fn serve(engine: Arc<Engine>) -> anyhow::Result<()> {
    let listen = engine.cfg.listen.clone();
    let web_dir = engine.cfg.web_dir.clone();
    let state = AppState { engine };

    let mut app = Router::new()
        .route("/api/health", get(health))
        .route("/api/venues", get(venues))
        .route("/api/instruments", get(instruments))
        .route("/api/strategies", get(list_strategies).post(create_strategy))
        .route(
            "/api/strategies/{id}",
            get(get_strategy)
                .put(update_strategy)
                .delete(delete_strategy),
        )
        .route("/api/strategies/{id}/start", post(start_strategy))
        .route("/api/strategies/{id}/stop", post(stop_strategy))
        .route("/api/strategies/{id}/flatten", post(flatten_strategy))
        .route("/api/orders", get(list_orders))
        .route("/api/fills", get(list_fills))
        .route("/api/positions", get(list_positions))
        .route("/api/journal", get(list_journal))
        .route("/api/risk", get(risk_view))
        .route("/api/risk/kill", post(kill))
        .route("/api/risk/resume", post(resume))
        .route("/ws", get(ws_upgrade))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state);

    if web_dir.exists() {
        let index = web_dir.join("index.html");
        let assets = web_dir.join("assets");
        app = app
            .nest_service("/assets", ServeDir::new(assets))
            .fallback(get(move || {
                let index = index.clone();
                async move {
                    match tokio::fs::read_to_string(index).await {
                        Ok(html) => Html(html).into_response(),
                        Err(_) => StatusCode::NOT_FOUND.into_response(),
                    }
                }
            }));
    }

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!(%listen, "api listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health(State(st): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "ok": true,
        "env": format!("{:?}", st.engine.cfg.trading_env),
    }))
}

async fn venues(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.engine.health_list())
}

async fn instruments(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.engine.catalog.read().listed())
}

async fn list_strategies(State(st): State<AppState>) -> impl IntoResponse {
    match st.engine.store.list_strategies().await {
        Ok(cfgs) => {
            let snaps = st.engine.snapshots.read();
            let rows: Vec<serde_json::Value> = cfgs
                .into_iter()
                .map(|c| {
                    let snap = snaps.get(c.id.as_str()).cloned();
                    serde_json::json!({
                        "config": c,
                        "snapshot": snap,
                    })
                })
                .collect();
            (StatusCode::OK, Json(serde_json::json!(rows))).into_response()
        }
        Err(e) => err(e),
    }
}

async fn get_strategy(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    let sid = StrategyId::new(id);
    match st.engine.store.get_strategy(&sid).await {
        Ok(Some(cfg)) => {
            let snap = st.engine.snapshot(sid.as_str());
            Json(serde_json::json!({"config": cfg, "snapshot": snap})).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => err(e),
    }
}

async fn create_strategy(State(st): State<AppState>, Json(cfg): Json<StrategyConfig>) -> Response {
    match st.engine.upsert_strategy(cfg).await {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => err(e),
    }
}

async fn update_strategy(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(mut cfg): Json<StrategyConfig>,
) -> Response {
    cfg.id = StrategyId::new(id);
    match st.engine.upsert_strategy(cfg).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => err(e),
    }
}

async fn delete_strategy(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match st.engine.delete_strategy(&StrategyId::new(id)).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => err(e),
    }
}

async fn start_strategy(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match st.engine.start_strategy(&StrategyId::new(id)).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => err(e),
    }
}

async fn stop_strategy(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    st.engine.stop_strategy(&StrategyId::new(id)).await;
    StatusCode::OK.into_response()
}

async fn flatten_strategy(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    st.engine.flatten_strategy(&StrategyId::new(id)).await;
    StatusCode::OK.into_response()
}

#[derive(Deserialize)]
struct ListQuery {
    strategy_id: Option<String>,
    open: Option<bool>,
    limit: Option<i64>,
}

async fn list_orders(State(st): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    match st
        .engine
        .store
        .list_orders(q.strategy_id.as_deref(), q.open.unwrap_or(false), q.limit.unwrap_or(200))
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(e),
    }
}

async fn list_fills(State(st): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    match st
        .engine
        .store
        .list_fills(q.strategy_id.as_deref(), q.limit.unwrap_or(200))
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(e),
    }
}

async fn list_positions(State(st): State<AppState>) -> Response {
    match st.engine.store.list_positions().await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(e),
    }
}

async fn list_journal(State(st): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    match st
        .engine
        .store
        .recent_journal(q.strategy_id.as_deref(), q.limit.unwrap_or(100))
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(e),
    }
}

async fn risk_view(State(st): State<AppState>) -> impl IntoResponse {
    let snaps = st.engine.snapshot_list();
    Json(st.engine.risk.view(&snaps))
}

async fn kill(State(st): State<AppState>) -> impl IntoResponse {
    st.engine.kill_all().await;
    StatusCode::OK
}

async fn resume(State(st): State<AppState>) -> impl IntoResponse {
    st.engine.clear_kill().await;
    StatusCode::OK
}

async fn ws_upgrade(State(st): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_loop(st, socket))
}

async fn ws_loop(st: AppState, mut socket: WebSocket) {
    let mut rx = st.engine.ws.subscribe();
    let hello = serde_json::json!({
        "type": "hello",
        "venues": st.engine.health_list(),
        "snapshots": st.engine.snapshot_list(),
        "risk": st.engine.risk.view(&st.engine.snapshot_list()),
    });
    let _ = socket.send(Message::Text(hello.to_string().into())).await;
    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(m) => {
                        if let Ok(t) = serde_json::to_string(&m) {
                            if socket.send(Message::Text(t.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            incoming = socket.recv() => {
                if incoming.is_none() { break; }
            }
        }
    }
}

use tokio::sync::broadcast;

fn err(e: impl std::fmt::Display) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"error": e.to_string()})),
    )
        .into_response()
}
