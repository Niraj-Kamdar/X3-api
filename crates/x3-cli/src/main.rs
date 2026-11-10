//! `x3` command-line tool.
//!
//! - `x3 mcp`: stdio MCP server for agents (Claude Code etc.). It forwards
//!   every request to the running Voice Studio app's `/mcp` endpoint using
//!   the local token in `X3_TOKEN`. The shim never sees provider keys.
//! - `x3 serve`: run the server headless (development/testing, incl. Linux).
//! - `x3 token …`: mint/list/revoke local API tokens in the data dir.

use std::{io::BufRead, path::PathBuf, sync::Arc};

use clap::{Parser, Subcommand};
use x3_server::{default_data_dir, Db, NewToken, Paths, DEFAULT_PORT};

mod shim;

#[derive(Parser)]
#[command(name = "x3", version, about = "Voice Studio command-line tool")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a stdio MCP server that forwards to the running app.
    Mcp {
        /// Local API token minted in Voice Studio (x3_live_…).
        #[arg(long, env = "X3_TOKEN", hide_env_values = true)]
        token: String,
        /// MCP endpoint of the running app.
        #[arg(long, env = "X3_URL", default_value_t = format!("http://127.0.0.1:{DEFAULT_PORT}/mcp"))]
        url: String,
    },
    /// Run the HTTP/MCP server without the desktop app.
    Serve {
        /// Port on 127.0.0.1 (overrides settings.json).
        #[arg(long)]
        port: Option<u16>,
        /// Data directory (settings, token DB, outputs).
        #[arg(long, env = "X3_DATA_DIR")]
        data_dir: Option<PathBuf>,
        /// Mint a token with this label at startup and print it to stderr.
        #[arg(long, value_name = "LABEL")]
        mint_token: Option<String>,
        /// Read a provider key (elevenlabs|cartesia) from one line of stdin
        /// and keep it in memory only. Repeatable, one line per provider, in
        /// order. On macOS keys normally come from the Keychain instead.
        #[arg(long, value_name = "PROVIDER")]
        read_key: Vec<String>,
    },
    /// Manage local API tokens.
    Token {
        #[arg(long, env = "X3_DATA_DIR", global = true)]
        data_dir: Option<PathBuf>,
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Mint a token (printed once).
    Mint {
        label: String,
        /// Restrict to these providers (comma-separated), e.g. apple,elevenlabs
        #[arg(long, value_delimiter = ',')]
        providers: Option<Vec<String>>,
        /// Restrict to these tasks (comma-separated): tts,stt
        #[arg(long, value_delimiter = ',')]
        tasks: Option<Vec<String>>,
        /// Lifetime spend cap in USD.
        #[arg(long)]
        cap: Option<f64>,
    },
    /// List tokens (metadata only).
    List,
    /// Revoke a token by ID.
    Revoke { id: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs always go to stderr: stdout carries MCP JSON-RPC in `x3 mcp`.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,x3=info,x3_server=info".into()),
        )
        .init();

    match Cli::parse().cmd {
        Cmd::Mcp { token, url } => shim::run(&url, &token).await,
        Cmd::Serve {
            port,
            data_dir,
            mint_token,
            read_key,
        } => serve(port, data_dir, mint_token, read_key).await,
        Cmd::Token { data_dir, cmd } => token(data_dir, cmd).await,
    }
}

async fn serve(
    port: Option<u16>,
    data_dir: Option<PathBuf>,
    mint_token: Option<String>,
    read_key: Vec<String>,
) -> anyhow::Result<()> {
    let paths = Paths::new(data_dir.unwrap_or_else(default_data_dir));
    let store: Arc<dyn x3_vault::SecretStore> = if read_key.is_empty() {
        Arc::from(x3_vault::default_store(x3_server::BUNDLE_ID))
    } else {
        let mem = x3_vault::MemoryStore::new();
        let stdin = std::io::stdin();
        let mut lines = stdin.lock().lines();
        for provider in &read_key {
            let line = lines
                .next()
                .ok_or_else(|| anyhow::anyhow!("expected a key for {provider} on stdin"))??;
            x3_vault::SecretStore::set(&mem, provider, x3_vault::SecretString::from(line))?;
            eprintln!("loaded {provider} key into memory");
        }
        Arc::new(mem)
    };
    let service =
        x3_server::open_service(paths.clone(), x3_server::default_model_router(store)).await?;
    if let Some(label) = mint_token {
        let (_, token) = service
            .db()
            .mint_token(&NewToken {
                label,
                ..Default::default()
            })
            .await?;
        eprintln!("token: {token}");
    }
    let port = port.unwrap_or(service.settings().port);
    let handle = x3_server::start(service, port).await?;
    eprintln!(
        "Voice Studio server on http://{} (data: {})",
        handle.addr,
        paths.data_dir.display()
    );
    tokio::signal::ctrl_c().await?;
    handle.shutdown().await;
    Ok(())
}

async fn token(data_dir: Option<PathBuf>, cmd: TokenCmd) -> anyhow::Result<()> {
    let paths = Paths::new(data_dir.unwrap_or_else(default_data_dir));
    paths.ensure()?;
    let db = Db::open(&paths.db_path).await?;
    match cmd {
        TokenCmd::Mint {
            label,
            providers,
            tasks,
            cap,
        } => {
            let tasks = tasks
                .map(|t| t.iter().map(|s| x3_core::Task::parse(s)).collect())
                .transpose()?;
            let (info, token) = db
                .mint_token(&NewToken {
                    label,
                    allowed_providers: providers,
                    allowed_tasks: tasks,
                    spend_cap_usd: cap,
                })
                .await?;
            eprintln!("minted {} ({})", info.id, info.label);
            println!("{token}");
        }
        TokenCmd::List => {
            println!(
                "{}",
                serde_json::to_string_pretty(&db.list_tokens().await?)?
            );
        }
        TokenCmd::Revoke { id } => {
            if db.revoke_token(&id).await? {
                eprintln!("revoked {id}");
            } else {
                anyhow::bail!("no active token with id {id}");
            }
        }
    }
    Ok(())
}
