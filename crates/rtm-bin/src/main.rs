// SPDX-License-Identifier: AGPL-3.0-or-later
//! `rtm`: the read-to-me binary. One binary, several process roles (ADR-0002).

mod fetch_config;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use std::time::Duration;

use rtm_api::StaticToken;
use rtm_blobs::BlobStore;
use rtm_domain::UserId;
use rtm_fetch::SafeFetcher;
use rtm_pipeline::{FetchStage, StageRunner, Worker};
use rtm_store::{QueuePolicy, Store};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
usage: rtm <role> [options]

roles:
  server [--ignore-robots]   HTTP API, workers and feed serving

environment:
  RTM_STATIC_TOKEN         API bearer token, at least 32 characters (required;
                           replaced by OIDC and personal access tokens later)
  RTM_DATA_DIR             directory for the database and blobs (default ./data)
  RTM_LISTEN_ADDR          address to listen on (default 127.0.0.1:8080)
  RTM_LOG                  log filter, e.g. info or rtm_api=debug (default info)
  RTM_FETCH_WORKERS        concurrent fetches (default 2)
  RTM_FETCH_IGNORE_ROBOTS  true to ignore robots.txt (\"disrespectful mode\");
                           same as --ignore-robots
  RTM_FETCH_DENY           extra address ranges never to fetch, comma-separated
  RTM_FETCH_UNSAFE_ALLOW   private ranges to allow anyway, e.g. an intranet; this
                           re-opens them to server-side request forgery
  RTM_FETCH_ALLOW_PORTS    ports to allow besides 80 and 443, comma-separated";

/// How long shutdown waits for in-flight fetches (each is capped at 30 s).
const WORKER_DRAIN: Duration = Duration::from_secs(35);

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
        ["server"] => run_server(false),
        ["server", "--ignore-robots"] => run_server(true),
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
async fn run_server(ignore_robots: bool) -> ExitCode {
    init_logging();
    let config = listen_addr().and_then(|addr| {
        let fetch = fetch_config::fetch_settings(|k| std::env::var(k).ok(), ignore_robots)?;
        Ok((addr, static_token()?, fetch))
    });
    let (addr, token, fetch) = match config {
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
    if fetch.config.ignore_robots {
        // FR-FET-08: disrespectful mode is loud.
        tracing::warn!(
            "robots.txt is being IGNORED (disrespectful mode): sites that ask automated \
             readers not to fetch their pages will be fetched anyway"
        );
    }
    if !fetch.unsafe_allow.is_empty() {
        let ranges: Vec<String> = fetch.unsafe_allow.iter().map(ToString::to_string).collect();
        tracing::warn!(
            ranges = %ranges.join(","),
            "RTM_FETCH_UNSAFE_ALLOW re-opens these private ranges to fetching"
        );
    }
    let fetcher = match SafeFetcher::new(fetch.config) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "could not set up DNS resolution");
            return ExitCode::FAILURE;
        }
    };
    let stage: Arc<dyn StageRunner> =
        Arc::new(FetchStage::new(fetcher, BlobStore::new(dir.join("blobs"))));
    let (stop_workers, workers_stopped) = tokio::sync::watch::channel(false);
    let workers: Vec<_> = (0..fetch.workers)
        .map(|n| {
            let worker = Worker::new(format!("fetch-{n}"), Arc::clone(&store), Arc::clone(&stage));
            tokio::spawn(worker.run(workers_stopped.clone()))
        })
        .collect();

    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%addr, error = %e, "could not bind");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(%addr, version = env!("CARGO_PKG_VERSION"), "rtm server listening");
    let router = rtm_api::router(store, token);
    let served = rtm_api::serve(listener, router, shutdown_signal()).await;

    // Stop claiming new jobs and let in-flight ones finish. Anything still running
    // after the drain period is abandoned; its lease expires and it is retried.
    let _ = stop_workers.send(true);
    let drained = tokio::time::timeout(WORKER_DRAIN, async {
        for worker in workers {
            let _ = worker.await;
        }
    })
    .await;
    if drained.is_err() {
        tracing::warn!("workers did not finish in time; their jobs will be retried");
    }

    match served {
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
