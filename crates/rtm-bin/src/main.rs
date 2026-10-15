// SPDX-License-Identifier: AGPL-3.0-or-later
//! `rtm`: the read-to-me binary. One binary, several process roles (ADR-0002).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use rtm_api::StaticToken;
use rtm_domain::UserId;
use rtm_store::{QueuePolicy, Store};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
usage: rtm <role>

roles:
  server      HTTP API, workers and feed serving

environment:
  RTM_STATIC_TOKEN  API bearer token, at least 32 characters (required; replaced by
                    OIDC and personal access tokens in a later release)
  RTM_DATA_DIR      directory for the database (default ./data)
  RTM_LISTEN_ADDR   address to listen on (default 127.0.0.1:8080)
  RTM_LOG           log filter, e.g. info or rtm_api=debug (default info)";

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8080";
const DEFAULT_DATA_DIR: &str = "data";

/// The single user the static token authenticates as.
const OWNER: &str = "owner";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["server"] => run_server(),
        ["--version" | "-V"] => {
            println!("rtm {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        ["--help" | "-h"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn init_logging() {
    let filter = EnvFilter::try_from_env("RTM_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(false)
        .init();
}

fn listen_addr() -> Result<SocketAddr, String> {
    let raw = std::env::var("RTM_LISTEN_ADDR").unwrap_or_else(|_| DEFAULT_LISTEN_ADDR.into());
    raw.parse()
        .map_err(|e| format!("RTM_LISTEN_ADDR {raw:?} is not a socket address: {e}"))
}

fn static_token() -> Result<StaticToken, String> {
    let token = std::env::var("RTM_STATIC_TOKEN")
        .map_err(|_| "RTM_STATIC_TOKEN is not set; the API has no other authentication yet")?;
    StaticToken::new(&token, UserId(OWNER.into())).map_err(|e| format!("RTM_STATIC_TOKEN: {e}"))
}

fn data_dir() -> PathBuf {
    std::env::var_os("RTM_DATA_DIR").map_or_else(|| DEFAULT_DATA_DIR.into(), PathBuf::from)
}

#[tokio::main]
async fn run_server() -> ExitCode {
    init_logging();
    let config = listen_addr().and_then(|addr| Ok((addr, static_token()?)));
    let (addr, token) = match config {
        Ok(config) => config,
        Err(e) => {
            tracing::error!(error = %e, "invalid configuration");
            return ExitCode::FAILURE;
        }
    };
    let dir = data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::error!(dir = %dir.display(), error = %e, "could not create data directory");
        return ExitCode::FAILURE;
    }
    let store = match Store::open(&dir.join("rtm.db"), QueuePolicy::default()).await {
        Ok(store) => Arc::new(store),
        Err(e) => {
            tracing::error!(dir = %dir.display(), error = %e, "could not open database");
            return ExitCode::FAILURE;
        }
    };
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%addr, error = %e, "could not bind");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(%addr, version = env!("CARGO_PKG_VERSION"), "rtm server listening");
    let router = rtm_api::router(store, token);
    match rtm_api::serve(listener, router, shutdown_signal()).await {
        Ok(()) => {
            tracing::info!("rtm server stopped");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(error = %e, "server failed");
            ExitCode::FAILURE
        }
    }
}

/// Resolves on Ctrl-C, or SIGTERM on Unix (what `docker stop` sends).
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "could not listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "could not listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received; draining requests");
}
