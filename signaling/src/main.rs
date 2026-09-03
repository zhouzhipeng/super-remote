#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod auth;
mod local_clipboard;
mod state;
mod websocket;

use std::{borrow::Cow, net::SocketAddr, sync::Arc};

use anyhow::Context;
use auth::{AuthConfig, LoginRequest, LoginResponse, Principal, Role, TicketResponse};
use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, Query, Request, State},
    http::{
        HeaderMap, HeaderValue, StatusCode, Uri,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use remote_protocol::{device::DeviceSummary, signaling::CreateSessionRequest};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use state::AppState;
use tower_http::trace::TraceLayer;
use tracing::info;

#[derive(RustEmbed)]
#[folder = "../web/dist/"]
struct WebAssets;

#[derive(Debug, Deserialize)]
struct WsQuery {
    ticket: String,
}

#[derive(Serialize)]
struct LocalClipboardResponse {
    text: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("remote_signaling=info".parse()?),
        )
        .init();

    let config = AuthConfig::from_env()?;
    let bind: SocketAddr = std::env::var("REMOTE_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()
        .context("REMOTE_BIND must be a socket address")?;
    let state = Arc::new(AppState::new(config));
    let api = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/login", post(login))
        .route("/ws-ticket", post(browser_ticket))
        .route("/device-ticket", post(device_ticket))
        .route("/devices", get(devices))
        .route("/sessions", post(create_session))
        .route("/turn-credentials", get(turn_credentials))
        .route("/client-report", post(client_report))
        .route("/local-clipboard", get(local_clipboard_text))
        .route("/ws", any(ws_upgrade));

    let app = Router::new()
        .nest("/api", api)
        .fallback(embedded_web_asset)
        // The browser entry point changes whenever the Host is redeployed. A
        // cached HTML document can keep an already-open Chrome tab on an old
        // WebRTC policy indefinitely, so every navigation must revalidate the
        // current hashed assets instead of replaying an earlier app shell.
        .layer(middleware::from_fn(disable_http_cache))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    info!(%bind, "signaling server listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

async fn embedded_web_asset(uri: Uri) -> Response {
    let requested = uri.path().trim_start_matches('/');
    let path = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };
    let (asset, served_path) = match WebAssets::get(path) {
        Some(asset) => (asset, path),
        None => match WebAssets::get("index.html") {
            Some(asset) => (asset, "index.html"),
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "embedded web application is missing",
                )
                    .into_response();
            }
        },
    };
    let content_type = mime_guess::from_path(served_path)
        .first_or_octet_stream()
        .essence_str()
        .to_owned();
    let body = match asset.data {
        Cow::Borrowed(bytes) => Body::from(bytes),
        Cow::Owned(bytes) => Body::from(bytes),
    };
    let mut response = Response::new(body);
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    response
}

async fn disable_http_cache(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
    );
    response
}

async fn login(State(state): State<Arc<AppState>>, Json(request): Json<LoginRequest>) -> Response {
    match state.auth.login(&request) {
        Ok(token) => Json(LoginResponse {
            access_token: token,
            expires_in: 3600,
        })
        .into_response(),
        Err(_) => api_error(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "invalid username or password",
        ),
    }
}

async fn client_report(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(report): Json<serde_json::Value>,
) -> Response {
    let principal = match state.auth.authenticate_user(&headers) {
        Ok(principal) => principal,
        Err(_) => {
            return api_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "valid user authentication is required",
            );
        }
    };
    info!(subject = %principal.subject, report = %report, "browser client report");
    StatusCode::NO_CONTENT.into_response()
}

async fn local_clipboard_text(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let principal = match state.auth.authenticate_user(&headers) {
        Ok(principal) => principal,
        Err(_) => {
            return api_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "valid user authentication is required",
            );
        }
    };
    // This endpoint is deliberately synchronous from the browser's point of
    // view. Cmd+C must finish while the browser still considers the keyboard
    // event a trusted user gesture, otherwise an HTTP LAN page cannot update
    // the client clipboard. Wait briefly for the Host's Ctrl+C to advance the
    // shared Windows clipboard sequence before returning its text.
    match tokio::task::spawn_blocking(|| {
        local_clipboard::read_text_after_copy(std::time::Duration::from_millis(140))
    })
    .await
    {
        Ok(Ok(text)) => {
            info!(subject = %principal.subject, bytes = text.len(), "served Host clipboard to browser copy gesture");
            Json(LocalClipboardResponse { text }).into_response()
        }
        Ok(Err(error)) => api_error(
            StatusCode::CONFLICT,
            "clipboard_unavailable",
            &error.to_string(),
        ),
        Err(error) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "clipboard_worker_failed",
            &error.to_string(),
        ),
    }
}

async fn browser_ticket(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let principal = match state.auth.authenticate_bearer(&headers) {
        Ok(p) if p.role == Role::User => p,
        _ => {
            return api_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "valid user bearer token required",
            );
        }
    };
    let ticket = state.issue_ticket(principal).await;
    Json(TicketResponse {
        ticket,
        expires_in: 60,
    })
    .into_response()
}

async fn device_ticket(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let token = headers.get("x-device-token").and_then(|v| v.to_str().ok());
    let device_id = headers.get("x-device-id").and_then(|v| v.to_str().ok());
    match (token, device_id) {
        (Some(token), Some(device_id)) if state.auth.verify_device_token(token) => {
            let principal = Principal {
                subject: device_id.to_owned(),
                role: Role::Device,
            };
            let ticket = state.issue_ticket(principal).await;
            Json(TicketResponse {
                ticket,
                expires_in: 60,
            })
            .into_response()
        }
        _ => api_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "valid device credentials required",
        ),
    }
}

async fn devices(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if state.auth.authenticate_user(&headers).is_err() {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "valid bearer token required",
        );
    }
    let devices: Vec<DeviceSummary> = state
        .devices
        .read()
        .await
        .values()
        .map(|d| d.summary.clone())
        .collect();
    Json(devices).into_response()
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateSessionRequest>,
) -> Response {
    let user = match state.auth.authenticate_user(&headers) {
        Ok(p) => p,
        Err(_) => {
            return api_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "valid bearer token required",
            );
        }
    };
    match state
        .create_session(&user.subject, &request.device_id)
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(state::CreateSessionError::Offline) => api_error(
            StatusCode::CONFLICT,
            "device_offline",
            "device is not online",
        ),
        Err(state::CreateSessionError::NotFound) => api_error(
            StatusCode::NOT_FOUND,
            "device_not_found",
            "device does not exist",
        ),
    }
}

async fn turn_credentials(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let user = match state.auth.authenticate_user(&headers) {
        Ok(p) => p,
        Err(_) => {
            return api_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "valid bearer token required",
            );
        }
    };
    match state.auth.turn_credentials(&user.subject) {
        Some(credentials) => Json(credentials).into_response(),
        None => api_error(
            StatusCode::NOT_FOUND,
            "turn_not_configured",
            "TURN is not configured",
        ),
    }
}

async fn ws_upgrade(
    ws: axum::extract::ws::WebSocketUpgrade,
    ConnectInfo(peer_address): ConnectInfo<SocketAddr>,
    Query(query): Query<WsQuery>,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state.consume_ticket(&query.ticket).await {
        Some(principal) => ws.max_message_size(1 << 20).on_upgrade(move |socket| {
            websocket::serve(socket, state, principal, peer_address.ip())
        }),
        None => api_error(
            StatusCode::UNAUTHORIZED,
            "invalid_ticket",
            "websocket ticket is invalid or expired",
        ),
    }
}

fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "code": code, "message": message })),
    )
        .into_response()
}

#[cfg(test)]
mod embedded_web_tests {
    use super::WebAssets;

    #[test]
    fn production_web_application_is_embedded() {
        let index = WebAssets::get("index.html").expect("web/dist/index.html must be embedded");
        let html = std::str::from_utf8(index.data.as_ref()).expect("index.html must be UTF-8");
        assert!(html.contains("<div id=\"app\"></div>"));
        assert!(WebAssets::iter().any(|path| path.starts_with("assets/")));
    }
}
