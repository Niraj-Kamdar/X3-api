//! Tauri commands backing the Providers and Server screens.
//!
//! Key handling: `provider_set_key` receives the key once from the UI,
//! wraps it in `SecretString` immediately and stores it in the Keychain.
//! No command returns a key; status only includes the last-4 hint.

use serde::Serialize;
use tauri::State;
use x3_core::Task;
use x3_server::{NewToken, TokenInfo};
use x3_vault::{SecretString, PROVIDERS};

use crate::AppState;

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize)]
pub struct ProviderStatus {
    provider: String,
    has_key: bool,
    /// e.g. "…a1b2"; never more than 4 characters of the key.
    hint: Option<String>,
    monthly_budget_usd: Option<f64>,
}

#[tauri::command]
pub async fn providers_status(state: State<'_, AppState>) -> CmdResult<Vec<ProviderStatus>> {
    let settings = state.service.settings();
    PROVIDERS
        .iter()
        .map(|p| {
            let hint = state.store.hint(p).map_err(err)?;
            Ok(ProviderStatus {
                provider: p.to_string(),
                has_key: hint.is_some(),
                hint,
                monthly_budget_usd: settings.budgets.provider_monthly_usd.get(*p).copied(),
            })
        })
        .collect()
}

#[tauri::command]
pub async fn provider_set_key(
    state: State<'_, AppState>,
    provider: String,
    key: String,
) -> CmdResult<Option<String>> {
    let key = SecretString::from(key);
    state.store.set(&provider, key).map_err(err)?;
    state.store.hint(&provider).map_err(err)
}

#[tauri::command]
pub async fn provider_delete_key(state: State<'_, AppState>, provider: String) -> CmdResult<()> {
    state.store.delete(&provider).map_err(err)
}

#[derive(Serialize)]
pub struct ProviderTest {
    ok: bool,
    message: String,
}

/// Verify a key by listing voices (free on both providers).
#[tauri::command]
pub async fn provider_test(
    state: State<'_, AppState>,
    provider: String,
) -> CmdResult<ProviderTest> {
    let engine = state
        .service
        .router()
        .engine(&provider)
        .ok_or_else(|| format!("unknown provider '{provider}'"))?;
    Ok(match engine.list_voices().await {
        Ok(v) => ProviderTest {
            ok: true,
            message: format!("Key works: {} voices available.", v.len()),
        },
        Err(e) => ProviderTest {
            ok: false,
            message: e.to_string(),
        },
    })
}

#[tauri::command]
pub async fn provider_set_budget(
    state: State<'_, AppState>,
    provider: String,
    monthly_usd: Option<f64>,
) -> CmdResult<()> {
    if !PROVIDERS.contains(&provider.as_str()) {
        return Err(format!("unknown provider '{provider}'"));
    }
    if monthly_usd.is_some_and(|v| !v.is_finite() || v < 0.0) {
        return Err("budget must be a non-negative number".into());
    }
    state
        .service
        .update_settings(|s| match monthly_usd {
            Some(v) => {
                s.budgets.provider_monthly_usd.insert(provider.clone(), v);
            }
            None => {
                s.budgets.provider_monthly_usd.remove(&provider);
            }
        })
        .map(|_| ())
        .map_err(err)
}

#[derive(Serialize)]
pub struct ServerStatus {
    running: bool,
    port: u16,
    url: String,
}

#[tauri::command]
pub async fn server_status(state: State<'_, AppState>) -> CmdResult<ServerStatus> {
    let guard = state.server.lock().await;
    let (running, port) = match guard.as_ref() {
        Some(h) if h.is_running() => (true, h.addr.port()),
        _ => (false, state.service.settings().port),
    };
    Ok(ServerStatus {
        running,
        port,
        url: format!("http://127.0.0.1:{port}"),
    })
}

#[tauri::command]
pub async fn server_start(state: State<'_, AppState>) -> CmdResult<ServerStatus> {
    state.start_server().await.map_err(|e| format!("{e:#}"))?;
    server_status(state).await
}

#[tauri::command]
pub async fn server_stop(state: State<'_, AppState>) -> CmdResult<ServerStatus> {
    state.stop_server().await;
    server_status(state).await
}

/// Change the port; restarts the server if it is running.
#[tauri::command]
pub async fn server_set_port(state: State<'_, AppState>, port: u16) -> CmdResult<ServerStatus> {
    if port < 1024 {
        return Err("choose a port between 1024 and 65535".into());
    }
    state
        .service
        .update_settings(|s| s.port = port)
        .map_err(err)?;
    let was_running = state
        .server
        .lock()
        .await
        .as_ref()
        .is_some_and(|h| h.is_running());
    if was_running {
        state.stop_server().await;
        state.start_server().await.map_err(|e| format!("{e:#}"))?;
    }
    server_status(state).await
}

#[tauri::command]
pub async fn tokens_list(state: State<'_, AppState>) -> CmdResult<Vec<TokenInfo>> {
    state.service.db().list_tokens().await.map_err(err)
}

#[derive(Serialize)]
pub struct MintedToken {
    info: TokenInfo,
    /// Shown once in the UI, then discarded.
    token: String,
    /// Ready-to-paste MCP config for Claude Code.
    mcp_config: String,
}

#[tauri::command]
pub async fn token_mint(
    state: State<'_, AppState>,
    label: String,
    providers: Option<Vec<String>>,
    tasks: Option<Vec<String>>,
    spend_cap_usd: Option<f64>,
) -> CmdResult<MintedToken> {
    let tasks = tasks
        .map(|t| {
            t.iter()
                .map(|s| Task::parse(s))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()
        .map_err(err)?;
    let (info, token) = state
        .service
        .db()
        .mint_token(&NewToken {
            label,
            allowed_providers: providers.filter(|p| !p.is_empty()),
            allowed_tasks: tasks.filter(|t| !t.is_empty()),
            spend_cap_usd,
        })
        .await
        .map_err(err)?;
    let port = state.service.settings().port;
    let mut env = serde_json::json!({ "X3_TOKEN": token });
    if port != x3_server::DEFAULT_PORT {
        env["X3_URL"] = serde_json::json!(format!("http://127.0.0.1:{port}/mcp"));
    }
    let mcp_config = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "voice": { "command": "x3", "args": ["mcp"], "env": env }
        }
    }))
    .map_err(err)?;
    Ok(MintedToken {
        info,
        token,
        mcp_config,
    })
}

#[tauri::command]
pub async fn token_revoke(state: State<'_, AppState>, id: String) -> CmdResult<bool> {
    state.service.db().revoke_token(&id).await.map_err(err)
}

#[tauri::command]
pub async fn usage_recent(state: State<'_, AppState>) -> CmdResult<Vec<serde_json::Value>> {
    state.service.db().recent_usage(50).await.map_err(err)
}
