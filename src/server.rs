//! The plugin service: the manifest at `GET /` and the session endpoint
//! `GET /ws/sessions/{kind}/{targetId}/{name}`, as `website/docs/develop/sessions.md` specifies
//! them (the same contract as the Kotlin `plugin-service` module).

use crate::functions::{self, CONTRIBUTION_ID, SESSION_NAME};
use crate::session::Session;
use crate::auth::{SCOPE_SESSION_CONNECT, Verifier};
use crate::config::Config;
use crate::protocol::{self, decode_client, encode_service};
use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Where the service listens unless `PORT` says otherwise.
pub const DEFAULT_PORT: u16 = 3022;
pub const FINGERPRINT_HEADER: &str = "x-mdeo-manifest-fingerprint";
const PING_PERIOD: Duration = Duration::from_secs(30);
const PONG_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_MESSAGE_BYTES: usize = 512 << 20;

pub struct AppState {
    pub config: Arc<Config>,
    verifier: Verifier,
    manifest: String,
    fingerprint: HeaderValue,
    open_sessions: AtomicUsize,
    max_sessions: usize,
}

pub fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let mut config = Config::from_env()?;
    if std::env::var_os("PORT").is_none() {
        config.port = DEFAULT_PORT;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(serve(config))
}

pub fn app(config: Config) -> (Router, Arc<AppState>) {
    let manifest = functions::manifest().to_string();
    let fingerprint = Sha256::digest(manifest.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>();
    let state = Arc::new(AppState {
        verifier: Verifier::new(&config.backend_api_url, &config.jwt_issuer),
        max_sessions: config.max_sessions,
        config: Arc::new(config),
        manifest,
        fingerprint: HeaderValue::from_str(&fingerprint).expect("hex"),
        open_sessions: AtomicUsize::new(0),
    });
    let fp = state.fingerprint.clone();
    let router = Router::new()
        .route("/", get(manifest_handler))
        .route("/ws/sessions/{kind}/{target}/{name}", get(session_handler))
        .route("/request/{kind}/{target}/{key}", any(no_requests))
        .route("/pcm/detail", axum::routing::post(detail_handler).layer(axum::extract::DefaultBodyLimit::max(256 << 20)))
        .fallback(not_found)
        // Every answer names the manifest, so the backend notices a redeploy that changed it.
        .layer(axum::middleware::map_response(move |mut r: Response| {
            let fp = fp.clone();
            async move {
                r.headers_mut().insert(FINGERPRINT_HEADER, fp);
                r
            }
        }))
        .with_state(state.clone());
    (router, state)
}

async fn serve(config: Config) -> Result<(), String> {
    let addr = format!("{}:{}", config.host, config.port);
    let (router, state) = app(config);
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    info!(
        "palladio-sim-service (Palladio Simulation) on http://{addr}: {} simulation threads, up to {} sessions, backend {}",
        state.config.threads(),
        state.max_sessions,
        state.config.backend_api_url
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body = serde_json::json!({"error": {"code": code, "message": message}}).to_string();
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn manifest_handler(State(state): State<Arc<AppState>>) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], state.manifest.clone()).into_response()
}

async fn no_requests(Path((kind, target, key)): Path<(String, String, String)>) -> Response {
    error_response(StatusCode::NOT_FOUND, "NotFound", &format!("{kind}:{target} answers no request '{key}'"))
}

/// `POST /pcm/detail` (only with `PCM_DETAIL_ENDPOINT=1`): the design detail of a model
/// (`crate::detail`). Body: the request's fields plus the model, as `.m` text (`modelText`) or as
/// mdeo model data (`model`).
async fn detail_handler(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Response {
    if std::env::var("PCM_DETAIL_ENDPOINT").as_deref() != Ok("1") {
        return error_response(StatusCode::NOT_FOUND, "not_found", "the detail endpoint is off (PCM_DETAIL_ENDPOINT=1)");
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, "bad_request", &format!("not JSON: {e}")),
    };
    let model = match (v.get("modelText").and_then(|x| x.as_str()), v.get("model")) {
        (Some(text), _) => crate::mfile::parse(text),
        (None, Some(m)) => crate::mfile::from_model_data(m),
        (None, None) => Err("the request needs modelText (.m) or model (model data)".to_string()),
    };
    let req = model.and_then(|m| crate::detail::DetailRequest::from_json(&v).map(|_| (m, v.clone())));
    let (model, req) = match req {
        Ok(x) => x,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, "bad_request", &e),
    };
    let runner = crate::evaluate::evaluator(&state.config);
    let limits = runner.limits();
    let _permit = match runner.permits.acquire().await.map_err(|e| e.to_string()) {
        Ok(p) => p,
        Err(e) => return error_response(StatusCode::SERVICE_UNAVAILABLE, "busy", &e),
    };
    let r = tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::detail::detail_json(&model, &req, limits)))
            .unwrap_or_else(|_| Err("the detail run panicked".into()))
    })
    .await;
    match r {
        Ok(Ok(v)) => ([(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response(),
        Ok(Err(e)) => error_response(StatusCode::UNPROCESSABLE_ENTITY, "simulation_failed", &e),
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "abandoned", &e.to_string()),
    }
}

async fn not_found(uri: axum::http::Uri) -> Response {
    error_response(StatusCode::NOT_FOUND, "NotFound", &format!("Route {} not found", uri.path()))
}

async fn session_handler(
    State(state): State<Arc<AppState>>,
    Path((kind, target, name)): Path<(String, String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(String::from);
    let requested: Vec<u32> = query
        .unwrap_or_default()
        .split('&')
        .filter_map(|kv| kv.strip_prefix("v="))
        .flat_map(|v| v.split("%2C").flat_map(|x| x.split(',')).map(String::from).collect::<Vec<_>>())
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| open_session(state, socket, kind, target, name, token, requested))
}

async fn refuse(mut socket: WebSocket, code: u16, reason: &str) {
    warn!("refusing session: {reason}");
    let mut reason = reason.to_string();
    while reason.len() > 123 {
        reason.pop();
    }
    let _ = socket.send(Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await;
}

async fn open_session(
    state: Arc<AppState>,
    socket: WebSocket,
    kind: String,
    target_id: String,
    name: String,
    token: Option<String>,
    requested: Vec<u32>,
) {
    if kind != "lang" && kind != "contrib" {
        return refuse(socket, 4404, "Not a session address").await;
    }
    let target = format!("{kind}:{target_id}");
    let label = format!("{target}/{name}");
    let Some(token) = token else {
        return refuse(socket, 4401, "Missing or invalid token").await;
    };
    let claims = match state.verifier.verify(&token).await {
        Ok(c) => c,
        Err(e) => return refuse(socket, 4401, &format!("Missing or invalid token: {e}")).await,
    };
    if !claims.scope.iter().any(|s| s == SCOPE_SESSION_CONNECT) {
        return refuse(socket, 4401, &format!("Token missing {SCOPE_SESSION_CONNECT} scope")).await;
    }
    if claims.target.as_deref() != Some(target.as_str()) || claims.session.as_deref() != Some(name.as_str()) {
        return refuse(socket, 4401, &format!("Token is not for {label}")).await;
    }
    let (Some(project), Some(execution)) = (claims.project_id, claims.execution_id) else {
        return refuse(socket, 4401, "Token names no project or no execution").await;
    };
    if target != format!("contrib:{CONTRIBUTION_ID}") || name != SESSION_NAME {
        return refuse(socket, 4404, &format!("This service serves no session {label}")).await;
    }
    if !requested.is_empty() && !requested.contains(&protocol::VERSION) {
        return refuse(socket, 4409, &format!("Session {label} speaks script-functions version 1")).await;
    }
    if state.open_sessions.fetch_add(1, Ordering::SeqCst) >= state.max_sessions {
        state.open_sessions.fetch_sub(1, Ordering::SeqCst);
        return refuse(socket, 4503, &format!("All {} sessions of this service are in use", state.max_sessions)).await;
    }
    let short = |s: &str| s.chars().take(8).collect::<String>();
    let label = format!("session {}/{}", short(&project), short(&execution));
    info!("{label}: opened ({} open)", state.open_sessions.load(Ordering::SeqCst));
    let summary = run_session(state.config.clone(), socket, label.clone()).await;
    let open = state.open_sessions.fetch_sub(1, Ordering::SeqCst) - 1;
    info!("{label}: closed after {summary} ({open} open)");
}

/// Reads continuously (keepalives, the next messages), writes from one task, and handles the
/// protocol messages in order on another.
async fn run_session(config: Arc<Config>, socket: WebSocket, label: String) -> String {
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            let close = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || close {
                break;
            }
        }
    });
    let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let replies = out_tx.clone();
    let processor_label = label.clone();
    let processor = tokio::spawn(async move {
        let mut session = Session::new(config, processor_label.clone());
        while let Some(bytes) = in_rx.recv().await {
            let message = match decode_client(&bytes) {
                Ok(m) => m,
                Err(e) => {
                    warn!("{processor_label}: unreadable message: {e}");
                    continue;
                }
            };
            if let Some(answer) = session.handle(message).await
                && replies.send(Message::Binary(encode_service(&answer).into())).await.is_err()
            {
                break;
            }
        }
        session.summary()
    });

    let mut last_seen = Instant::now();
    let mut ticker = tokio::time::interval(PING_PERIOD);
    ticker.tick().await;
    loop {
        tokio::select! {
            frame = stream.next() => {
                last_seen = Instant::now();
                match frame {
                    Some(Ok(Message::Binary(b))) => { let _ = in_tx.send(b.to_vec()); }
                    Some(Ok(Message::Text(t))) => { let _ = in_tx.send(t.as_bytes().to_vec()); }
                    Some(Ok(Message::Ping(p))) => { let _ = out_tx.send(Message::Pong(p)).await; }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                }
            }
            _ = ticker.tick() => {
                if last_seen.elapsed() > PONG_TIMEOUT {
                    warn!("{label}: peer stopped answering keepalives");
                    let _ = out_tx.send(Message::Close(Some(CloseFrame { code: 4408, reason: "No keepalive".into() }))).await;
                    break;
                }
                let _ = out_tx.send(Message::Ping(Vec::new().into())).await;
            }
        }
    }
    drop(in_tx);
    // A run that is still going finishes on the pool and stays cached: a client that reconnects
    // sends the call again and gets it from there.
    let mut processor = processor;
    let summary = match tokio::time::timeout(Duration::from_secs(1), &mut processor).await {
        Ok(Ok(s)) => s,
        _ => {
            processor.abort();
            "an interrupted call".into()
        }
    };
    drop(out_tx);
    let _ = writer.await;
    summary
}
