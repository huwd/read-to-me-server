// SPDX-License-Identifier: AGPL-3.0-or-later
//! `rtm`: the read-to-me binary. One binary, several process roles (ADR-0002).

use std::net::SocketAddr;
use std::process::ExitCode;

use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
usage: rtm <role>

roles:
  server      HTTP API, workers and feed serving

environment:
  RTM_LISTEN_ADDR   address to listen on (default 127.0.0.1:8080)
  RTM_LOG           log filter, e.g. info or rtm_api=debug (default info)";

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8080";

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

#[tokio::main]
async fn run_server() -> ExitCode {
    init_logging();
    let addr = match listen_addr() {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!(error = %e, "invalid configuration");
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
    match rtm_api::serve(listener, shutdown_signal()).await {
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
