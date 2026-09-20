use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, Query, Request, State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::auth::{Auth, AuthError};
use crate::config::StrategyConfig;
use crate::engine::Engine;
use crate::types::StrategyId;

#[derive(Clone)]
pub struct AppState {
    pub engine: Arc<Engine>,
    pub auth: Arc<Auth>,
}

pub async fn serve(engine: Arc<Engine>) -> anyhow::Result<()> {
    let listen = engine.cfg.listen.clone();
    let web_dir = engine.cfg.web_dir.clone();
    let auth = Arc::new(Auth::load(&engine.cfg.database_url)?);
    let state = AppState { engine, auth };

    let public = Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/status", get(auth_status))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/verify", post(auth_verify))
        .route("/api/auth/logout", post(auth_logout));

    let protected = Router::new()
        .route("/api/auth/me", get(auth_me))
        .route("/api/venues", get(venues))
        .route("/api/instruments", get(instruments))
        .route(
            "/api/strategies",
            get(list_strategies).post(create_strategy),
        )
        .route(
            "/api/strategies/{id}",
            get(get_strategy)
                .put(update_strategy)
                .delete(delete_strategy),
        )
        .route("/api/strategies/{id}/start", post(start_strategy))
        .route("/api/strategies/{id}/stop", post(stop_strategy))
        .route("/api/strategies/{id}/flatten", post(flatten_strategy))
        .route("/api/fills", get(list_fills))
        .route("/api/positions", get(list_positions))
        .route("/api/journal", get(list_journal))
        .route("/api/risk", get(risk_view))
        .route("/api/risk/kill", post(kill))
        .route("/api/risk/resume", post(resume))
        .route("/ws", get(ws_upgrade))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    let mut app = public
        .merge(protected)
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

async fn require_auth(State(st): State<AppState>, req: Request, next: Next) -> Response {
    if st.auth.session_ok(req.headers()) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response()
    }
}

async fn auth_status(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.auth.status())
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

async fn auth_login(State(st): State<AppState>, Json(body): Json<LoginBody>) -> Response {
    match st.auth.login(&body.username, &body.password) {
        Ok(out) => Json(serde_json::json!({
            "ticket": out.ticket,
            "enroll": out.enroll,
            "otpauth_url": out.otpauth_url,
            "secret": out.secret,
            "qr_svg": out.qr_svg,
        }))
        .into_response(),
        Err(AuthError::Locked) => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "尝试次数过多，请稍后再试"})),
        )
            .into_response(),
        Err(AuthError::Disabled) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "未启用登录"})),
        )
            .into_response(),
        Err(AuthError::Denied) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "用户名或密码不正确"})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct VerifyBody {
    ticket: String,
    code: String,
}

async fn auth_verify(State(st): State<AppState>, Json(body): Json<VerifyBody>) -> Response {
    match st.auth.verify(&body.ticket, &body.code) {
        Ok(token) => {
            let mut headers = HeaderMap::new();
            if let Ok(value) = st.auth.session_cookie(&token).parse() {
                headers.insert(header::SET_COOKIE, value);
            }
            (headers, Json(serde_json::json!({"ok": true}))).into_response()
        }
        Err(AuthError::Locked) => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "尝试次数过多，请稍后再试"})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "验证码不正确或已过期"})),
        )
            .into_response(),
    }
}

async fn auth_logout(State(st): State<AppState>) -> Response {
    let mut headers = HeaderMap::new();
    if let Ok(value) = st.auth.logout_cookie().parse() {
        headers.insert(header::SET_COOKIE, value);
    }
    (headers, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn auth_me(State(st): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let username = st
        .auth
        .session_user(&headers)
        .unwrap_or_else(|| st.auth.username.clone());
    Json(serde_json::json!({
        "username": username,
        "enabled": st.auth.enabled,
    }))
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
        Ok(()) => created_ok(),
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
        Ok(()) => ok_json(),
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
        Ok(()) => ok_json(),
        Err(e) => err(e),
    }
}

async fn stop_strategy(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    st.engine.stop_strategy(&StrategyId::new(id)).await;
    ok_json()
}

async fn flatten_strategy(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    st.engine.flatten_strategy(&StrategyId::new(id)).await;
    ok_json()
}

#[derive(Deserialize)]
struct ListQuery {
    strategy_id: Option<String>,
    limit: Option<i64>,
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
    ok_json()
}

async fn resume(State(st): State<AppState>) -> impl IntoResponse {
    st.engine.clear_kill().await;
    ok_json()
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

fn ok_json() -> Response {
    Json(serde_json::json!({"ok": true})).into_response()
}

fn created_ok() -> Response {
    (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response()
}

fn err(e: impl std::fmt::Display) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"error": e.to_string()})),
    )
        .into_response()
}
