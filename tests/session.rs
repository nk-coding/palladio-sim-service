//! The service end to end, over its WebSocket session endpoint, as an execution would use it:
//! RS256 session tokens checked against a JWKS endpoint, the `script-functions` protocol in CBOR,
//! and models imported from SimOxide's corpus, whose `pcmEvaluate` answers must equal the
//! library's evaluation of the same model.

use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use palladio_sim_service::config::Config;
use palladio_sim_service::import::{entry_files, import_files, run_spec};
use palladio_sim_service::protocol::{
    ClientMessage, ServiceMessage, WireMetamodel, WireModel, WireValue, decode_service, encode_client,
};
use palladio_sim_service::evaluate::{self, Monitors, Request};
use palladio_sim_service::pcm::PcmCache;
use palladio_sim_service::server;
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../simoxide/corpus");

fn b64url(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |acc, &b| (acc << 8) | b as u32) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

struct Keys {
    encoding: EncodingKey,
    jwks: String,
}

fn keys() -> &'static Keys {
    static K: OnceLock<Keys> = OnceLock::new();
    K.get_or_init(|| {
        let private = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let pem = private.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
        let public = private.to_public_key();
        let jwks = serde_json::json!({"keys": [{
            "kty": "RSA", "kid": "k1", "alg": "RS256", "use": "sig",
            "n": b64url(&public.n().to_bytes_be()), "e": b64url(&public.e().to_bytes_be())
        }]})
        .to_string();
        Keys { encoding: EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(), jwks }
    })
}

fn token(target: &str, session: &str) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let claims = serde_json::json!({
        "iss": "mdeo-platform", "exp": now + 300, "projectId": "p1", "executionId": "e1",
        "scope": ["plugin:session:connect"], "target": target, "session": session
    });
    let mut h = Header::new(Algorithm::RS256);
    h.kid = Some("k1".into());
    encode(&h, &claims, &keys().encoding).unwrap()
}

/// A fake backend publishing the key set, and the service pointed at it.
async fn start() -> std::net::SocketAddr {
    start_with(8).await
}

async fn start_with(sim_threads: usize) -> std::net::SocketAddr {
    let jwks = keys().jwks.clone();
    let backend = axum::Router::new().route(
        "/api/.well-known/jwks.json",
        axum::routing::get(move || {
            let j = jwks.clone();
            async move { ([(axum::http::header::CONTENT_TYPE, "application/json")], j) }
        }),
    );
    let bl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_addr = bl.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(bl, backend).await.unwrap() });
    let config = Config { backend_api_url: format!("http://{backend_addr}/api"), sim_threads, ..Config::default() };
    let (router, _) = server::app(config);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    addr
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: std::net::SocketAddr, path: &str, token: Option<&str>) -> Ws {
    let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
    if let Some(t) = token {
        req.headers_mut().insert("Authorization", format!("Bearer {t}").parse().unwrap());
    }
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn session(addr: std::net::SocketAddr) -> Ws {
    connect(addr, "/ws/sessions/contrib/palladio-arch/functions?v=1", Some(&token("contrib:palladio-arch", "functions"))).await
}

async fn send(ws: &mut Ws, m: &ClientMessage) {
    ws.send(Message::Binary(encode_client(m).into())).await.unwrap();
}

async fn answer(ws: &mut Ws) -> ServiceMessage {
    loop {
        match ws.next().await.unwrap().unwrap() {
            Message::Binary(b) => return decode_service(&b).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// A corpus model as the platform would send it: references sorted by name.
fn corpus_model(name: &str) -> (PathBuf, simoxide_sim::RunSpec, WireModel) {
    let dir = Path::new(CORPUS).join(name);
    let spec = run_spec(&dir).unwrap();
    let mut model = import_files(&entry_files(&dir, &spec).unwrap(), "/palladio.mm", palladio_sim_service::import::Naming::Ids).unwrap().model;
    for i in &mut model.instances {
        for (_, t) in &mut i.references {
            t.sort();
        }
    }
    (dir, spec, model)
}

/// `pcmEvaluate`'s positional arguments up to the seed: end, warm-up, bins, ms per unit, then
/// null lists and the defaults.
fn evaluate_args(end: f64, seed: i32) -> Vec<WireValue> {
    vec![
        WireValue::Double(end),
        WireValue::Double(0.0),
        WireValue::Double(10.0),
        WireValue::Double(1000.0),
        WireValue::Null,
        WireValue::Null,
        WireValue::Null,
        WireValue::Null,
        WireValue::Null,
        WireValue::Bool(false),
        WireValue::Int(10),
        WireValue::Int(seed),
    ]
}

async fn upload(ws: &mut Ws, model_id: i64, model: WireModel) {
    send(ws, &ClientMessage::Metamodel(WireMetamodel { path: "/palladio.mm".into(), classes: vec![] })).await;
    send(ws, &ClientMessage::Model { model_id, model }).await;
}

fn field<'a>(v: &'a WireValue, name: &str) -> &'a WireValue {
    let WireValue::Record { fields, .. } = v else { panic!("not a record: {v:?}") };
    &fields.iter().find(|(k, _)| k == name).unwrap_or_else(|| panic!("no field {name}")).1
}

fn list<'a>(objects: &'a [palladio_sim_service::protocol::HeapObject], v: &WireValue) -> &'a [WireValue] {
    let WireValue::Ref(id) = v else { panic!("not a list: {v:?}") };
    objects.iter().find(|o| o.id == *id).unwrap().elements.as_deref().unwrap()
}

/// The answer equals the library's evaluation of the model: sessions and operations, counts and
/// percentiles bit for bit.
fn assert_same(answer: &ServiceMessage, model: &WireModel, end: f64, seed: i64) {
    let ServiceMessage::Result { objects, value, .. } = answer else { panic!("{answer:?}") };
    let want = evaluate::run_caller(&PcmCache::new(model), &Request { end_s: end, seed, ..Request::default() }, Monitors::default(), Default::default()).unwrap();
    assert_eq!(field(value, "failed"), &WireValue::Bool(want.failed), "{}", want.message);
    for (name, stats) in [("sessions", &want.sessions), ("operations", &want.operations)] {
        let got = list(objects, field(value, name));
        assert_eq!(got.len(), stats.len(), "{name}");
        for (g, w) in got.iter().zip(stats.iter()) {
            assert_eq!(field(g, "key"), &WireValue::String(w.key.clone()));
            assert_eq!(field(g, "count"), &WireValue::Long(w.count as i64));
            let p: Vec<WireValue> = w.percentiles_ms.iter().map(|x| WireValue::Double(*x)).collect();
            assert_eq!(list(objects, field(g, "percentilesMs")), p.as_slice());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn evaluations_through_the_protocol() {
    let addr = start().await;
    let mut ws = session(addr).await;
    for (i, name) in ["h01_ps_single", "x_sl_mediastore", "h21_stoex_distributions", "h11_fork_sync", "h13_passive_contention"].iter().enumerate() {
        let (_, _, model) = corpus_model(name);
        upload(&mut ws, i as i64 + 1, model.clone()).await;
        let call = ClientMessage::Call { call_id: i as i64 + 1, operation: "pcmEvaluate".into(), objects: vec![], args: evaluate_args(200.0, 1), model_id: Some(i as i64 + 1) };
        send(&mut ws, &call).await;
        let a = answer(&mut ws).await;
        if let ServiceMessage::Result { value, .. } = &a {
            assert_eq!(field(value, "failed"), &WireValue::Bool(false), "{name}: {:?}", field(value, "message"));
        }
        assert_same(&a, &model, 200.0, 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_not_held_is_unknown() {
    let addr = start().await;
    let mut ws = session(addr).await;
    let call = ClientMessage::Call { call_id: 1, operation: "pcmEvaluate".into(), objects: vec![], args: evaluate_args(10.0, 1), model_id: Some(3) };
    send(&mut ws, &call).await;
    let ServiceMessage::Failure { code, .. } = answer(&mut ws).await else { panic!() };
    assert_eq!(code.as_deref(), Some("unknown-model"));
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_are_refused_as_specified() {
    let addr = start().await;
    let close_code = |mut ws: Ws| async move {
        match ws.next().await {
            Some(Ok(Message::Close(Some(f)))) => u16::from(f.code),
            other => panic!("expected a close, got {other:?}"),
        }
    };
    let path = "/ws/sessions/contrib/palladio-arch/functions";
    assert_eq!(close_code(connect(addr, path, None).await).await, 4401);
    assert_eq!(close_code(connect(addr, path, Some("not-a-token")).await).await, 4401);
    // Signed without a key id, as the backend does: accepted with the published key.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let claims = serde_json::json!({"iss": "mdeo-platform", "exp": now + 300, "projectId": "p", "executionId": "e",
        "scope": ["plugin:session:connect"], "target": "contrib:palladio-arch", "session": "functions"});
    let no_kid = encode(&Header::new(Algorithm::RS256), &claims, &keys().encoding).unwrap();
    let mut ws = connect(addr, &format!("{path}?v=1"), Some(&no_kid)).await;
    send(&mut ws, &ClientMessage::Release { ids: vec![], handles: vec![] }).await;
    send(&mut ws, &ClientMessage::Call { call_id: 1, operation: "pcmEvaluate".into(), objects: vec![], args: evaluate_args(10.0, 1), model_id: Some(1) }).await;
    assert!(matches!(answer(&mut ws).await, ServiceMessage::Failure { .. }));
    assert_eq!(close_code(connect(addr, path, Some(&token("contrib:other", "functions"))).await).await, 4401);
    let other = token("contrib:palladio-arch", "other");
    assert_eq!(close_code(connect(addr, "/ws/sessions/contrib/palladio-arch/other", Some(&other)).await).await, 4404);
    let t = token("contrib:palladio-arch", "functions");
    assert_eq!(close_code(connect(addr, &format!("{path}?v=2"), Some(&t)).await).await, 4409);
}

/// Many sessions at once, as an optimizer's workers would open them: every answer is still the
/// library's, and the runs spread over the pool.
#[tokio::test(flavor = "multi_thread")]
async fn many_sessions_in_parallel() {
    let addr = start().await;
    let (_, _, model) = corpus_model("x_sl_mediastore");
    let sessions = 24;
    let started = std::time::Instant::now();
    let mut tasks = Vec::new();
    for k in 0..sessions {
        let model = model.clone();
        tasks.push(tokio::spawn(async move {
            let mut ws = session(addr).await;
            upload(&mut ws, 1, model).await;
            send(&mut ws, &ClientMessage::Call { call_id: 1, operation: "pcmEvaluate".into(), objects: vec![], args: evaluate_args(100.0, 1000 + k), model_id: Some(1) }).await;
            (k, answer(&mut ws).await)
        }));
    }
    let mut answers = Vec::new();
    for t in tasks {
        answers.push(t.await.unwrap());
    }
    eprintln!("{sessions} sessions answered in {:?}", started.elapsed());
    for (k, a) in answers.iter().filter(|(k, _)| k % 8 == 0) {
        assert_same(a, &model, 100.0, 1000 + *k as i64);
    }
}

/// Throughput through the protocol: `BENCH_SESSIONS` sessions (default 22) each make
/// `BENCH_CALLS` calls (default 8) of mediastore (600 s) with a seed of their own, on
/// `SIM_THREADS` simulation threads (default: one per CPU).
///
/// `SIM_THREADS=1 cargo test --release --test session throughput -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn throughput() {
    let env = |k: &str, d: usize| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let threads = env("SIM_THREADS", 0);
    let (sessions, calls) = (env("BENCH_SESSIONS", 22), env("BENCH_CALLS", 8));
    let addr = start_with(threads).await;
    let (_, _, model) = corpus_model("x_sl_mediastore");
    let started = std::time::Instant::now();
    let mut tasks = Vec::new();
    for k in 0..sessions {
        let model = model.clone();
        tasks.push(tokio::spawn(async move {
            let mut ws = session(addr).await;
            upload(&mut ws, 1, model).await;
            for c in 0..calls {
                let args = evaluate_args(600.0, (k * 1000 + c) as i32);
                send(&mut ws, &ClientMessage::Call { call_id: c as i64, operation: "pcmEvaluate".into(), objects: vec![], args, model_id: Some(1) }).await;
                assert!(matches!(answer(&mut ws).await, ServiceMessage::Result { .. }));
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let secs = started.elapsed().as_secs_f64();
    let runs = sessions * calls;
    eprintln!("{runs} runs ({sessions} sessions x {calls} calls) on {} threads: {secs:.2} s, {:.1} runs/s", if threads == 0 { "all".to_string() } else { threads.to_string() }, runs as f64 / secs);
}
