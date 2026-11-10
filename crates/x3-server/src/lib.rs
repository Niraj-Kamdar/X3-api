//! Voice Studio local server.
//!
//! - Binds to `127.0.0.1` only (the port is configurable, the interface is
//!   not).
//! - Every route except `/health` requires a bearer token minted by the app.
//! - Requests whose `Host` is not loopback are rejected (DNS-rebinding guard);
//!   no CORS headers are ever sent.
//! - Admin operations (keys, tokens, budgets) are NOT exposed over HTTP; the
//!   desktop app calls [`VoiceService`] / [`db::Db`] in-process.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::DefaultBodyLimit,
    middleware::{from_fn, from_fn_with_state},
    routing::{get, post},
    Json, Router,
};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tokio_util::sync::CancellationToken;
use x3_core::ModelRouter;
use x3_vault::SecretStore;

pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod http;
pub mod mcp;
pub mod service;

pub use auth::Principal;
pub use config::{Paths, Settings, DEFAULT_PORT};
pub use db::{Db, NewToken, TokenInfo};
pub use service::VoiceService;

#[derive(Clone)]
pub struct AppState {
    pub service: Arc<VoiceService>,
}

/// Engines shipped in M0: Apple built-in speech, ElevenLabs, Cartesia.
pub fn default_model_router(store: Arc<dyn SecretStore>) -> ModelRouter {
    let mut r = ModelRouter::new(x3_core::AliasTable::default());
    r.register(Arc::new(x3_apple::AppleEngine::new()));
    r.register(Arc::new(x3_providers::ElevenLabsEngine::new(store.clone())));
    r.register(Arc::new(x3_providers::CartesiaEngine::new(store)));
    r
}

/// Open settings and the database under `paths` and build the service.
pub async fn open_service(paths: Paths, router: ModelRouter) -> anyhow::Result<Arc<VoiceService>> {
    paths.ensure()?;
    let settings = Settings::load(&paths.settings_path)?;
    let db = Db::open(&paths.db_path).await?;
    Ok(Arc::new(VoiceService::new(
        Arc::new(router),
        db,
        paths,
        settings,
    )))
}

/// Build the axum app (HTTP API + MCP) for a service.
pub fn build_app(service: Arc<VoiceService>, ct: CancellationToken) -> Router {
    let state = AppState { service };
    let playback = Arc::new(tokio::sync::Mutex::new(()));
    let mcp_svc = state.service.clone();
    let mcp: StreamableHttpService<mcp::VoiceMcp, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(mcp::VoiceMcp::new(mcp_svc.clone(), playback.clone())),
        Default::default(),
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
    );

    let authed = Router::new()
        .route("/v1/models", get(http::list_models))
        .route("/v1/voices", get(http::list_voices))
        .route("/v1/audio/speech", post(http::speech))
        .route(
            "/v1/audio/transcriptions",
            post(http::transcriptions)
                .layer(DefaultBodyLimit::max(service::MAX_STT_BYTES + 1024 * 1024)),
        )
        .nest_service("/mcp", mcp)
        .layer(from_fn_with_state(state.clone(), auth::require_token));

    Router::new()
        .route(
            "/health",
            get(|| async { Json(serde_json::json!({ "ok": true, "app": "voice-studio" })) }),
        )
        .merge(authed)
        .layer(from_fn(auth::host_guard))
        .with_state(state)
}

/// A running server.
pub struct ServerHandle {
    pub addr: SocketAddr,
    ct: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
    pub async fn shutdown(self) {
        self.ct.cancel();
        let _ = self.join.await;
    }

    pub fn is_running(&self) -> bool {
        !self.join.is_finished()
    }
}

/// Start serving on `127.0.0.1:port` (port 0 picks a free port).
pub async fn start(service: Arc<VoiceService>, port: u16) -> anyhow::Result<ServerHandle> {
    let ct = CancellationToken::new();
    let app = build_app(service, ct.clone());
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    let addr = listener.local_addr()?;
    let stop = ct.clone();
    let join = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.cancelled_owned().await })
            .await
        {
            tracing::error!("server error: {e}");
        }
    });
    tracing::info!("Voice Studio listening on http://{addr}");
    Ok(ServerHandle { addr, ct, join })
}
