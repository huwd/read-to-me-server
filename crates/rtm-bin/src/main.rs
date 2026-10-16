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
use rtm_pipeline::{
    DEFAULT_MAX_WORDS, EncodeConfig, EncodeStage, ExtractConfig, ExtractStage, FetchStage,
    PublishStage, ScriptStage, StageRunner, SynthesiseConfig, SynthesiseStage, Worker,
};
use rtm_store::{QueuePolicy, Store};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
usage: rtm <role> [options]

roles:
  server [--ignore-robots]   HTTP API, workers and feed serving
  extract-worker             sandboxed article extraction; started by the server,
                             one document per run over stdin and stdout
  tts                        speech synthesis over HTTP, for the server's
                             synthesise stage

environment:
  RTM_STATIC_TOKEN         API bearer token, at least 32 characters (required;
                           replaced by OIDC and personal access tokens later)
  RTM_DATA_DIR             directory for the database and blobs (default ./data)
  RTM_LISTEN_ADDR          address to listen on (default 127.0.0.1:8080)
  RTM_LOG                  log filter, e.g. info or rtm_api=debug (default info)
  RTM_FETCH_WORKERS        concurrent fetches (default 2)
  RTM_EXTRACT_WORKERS      concurrent extractions (default 1)
  RTM_MAX_WORDS            longest article narrated, in words (default 30000)
  RTM_TTS_URL              the rtm tts process (default http://127.0.0.1:8090)
  RTM_TTS_VOICE            narration voice (default bf_emma)
  RTM_PUBLIC_URL           how podcast apps reach this server, for feed URLs, e.g.
                           http://192.168.1.10:8080 (default: the listen address)
  RTM_FFMPEG               the ffmpeg binary (default: ffmpeg on PATH)

  Get your feed URL (shown once; calling again rotates it):
    curl -X POST -H \"Authorization: Bearer $RTM_STATIC_TOKEN\" <server>/v1/feed/token
  RTM_FETCH_IGNORE_ROBOTS  true to ignore robots.txt (\"disrespectful mode\");
                           same as --ignore-robots
  RTM_FETCH_DENY           extra address ranges never to fetch, comma-separated
  RTM_FETCH_UNSAFE_ALLOW   private ranges to allow anyway, e.g. an intranet; this
                           re-opens them to server-side request forgery
  RTM_FETCH_ALLOW_PORTS    ports to allow besides 80 and 443, comma-separated

tts environment:
  RTM_TTS_LISTEN_ADDR      address to listen on (default 127.0.0.1:8090); keep it
                           on an internal network: it has no authentication
  RTM_TTS_ENGINE           the speech engine (required): kokoro (in builds with the
                           kokoro feature), or fake, a test engine that makes tones
  RTM_KOKORO_DIR           Kokoro model files (fetch with `cargo xtask kokoro-models`)
  RTM_KOKORO_THREADS       inference threads (default: all cores)";

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
        // Logs would corrupt the protocol on stdout, so this role logs nothing; errors
        // go to stderr, which the server records.
        ["tts"] => run_tts(),
        ["extract-worker"] => ExitCode::from(rtm_extract::worker::run(
            &rtm_extract::worker::WorkerLimits::default(),
        )),
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

/// RTM_EXTRACT_WORKERS: concurrent extract workers, each a sandboxed process using up
/// to 512 MB.
fn extract_worker_count() -> Result<usize, String> {
    match std::env::var("RTM_EXTRACT_WORKERS") {
        Err(_) => Ok(1),
        Ok(n) => n
            .trim()
            .parse()
            .ok()
            .filter(|n| (1..=16).contains(n))
            .ok_or_else(|| format!("RTM_EXTRACT_WORKERS={n:?}: expected 1 to 16")),
    }
}

/// How long one piece may take to synthesise: generous for slow CPUs (a Raspberry Pi
/// runs below real time on a minute or two of speech).
const TTS_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// RTM_TTS_URL: where the rtm tts process listens.
fn tts_client() -> Result<rtm_speech::client::TtsClient, String> {
    let url = std::env::var("RTM_TTS_URL").unwrap_or_else(|_| "http://127.0.0.1:8090".into());
    rtm_speech::client::TtsClient::new(&url, TTS_REQUEST_TIMEOUT)
        .map_err(|e| format!("RTM_TTS_URL: {e}"))
}

/// RTM_FFMPEG, or ffmpeg on PATH, as an absolute path: the encoder runs with a cleared
/// environment, so it can't search PATH itself.
fn ffmpeg() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("RTM_FFMPEG") {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!("RTM_FFMPEG={}: no such file", path.display()))
        };
    }
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("ffmpeg"))
                .find(|p| p.is_file())
        })
        .ok_or_else(|| "ffmpeg isn't on PATH: install it or set RTM_FFMPEG".into())
}

/// RTM_PUBLIC_URL: how podcast apps reach this server, for absolute feed URLs.
fn public_url(listen: SocketAddr) -> Result<url::Url, String> {
    let raw = std::env::var("RTM_PUBLIC_URL").unwrap_or_else(|_| format!("http://{listen}"));
    let url: url::Url = raw
        .parse()
        .map_err(|e| format!("RTM_PUBLIC_URL {raw:?} is not a URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(format!(
            "RTM_PUBLIC_URL {raw:?}: expected http(s)://host[:port]"
        ));
    }
    Ok(url)
}

/// RTM_MAX_WORDS: the longest article narrated (FR-TTS-06).
fn max_words() -> Result<u64, String> {
    match std::env::var("RTM_MAX_WORDS") {
        Err(_) => Ok(DEFAULT_MAX_WORDS),
        Ok(n) => n
            .trim()
            .parse()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| format!("RTM_MAX_WORDS={n:?}: expected a positive number of words")),
    }
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
    let blobs = BlobStore::new(dir.join("blobs"));
    // The extract worker is this same binary in another role (ADR-0006).
    let this_binary = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            tracing::error!(error = %e, "could not locate this binary to start extract workers");
            return ExitCode::FAILURE;
        }
    };
    let config = extract_worker_count()
        .and_then(|n| Ok((n, max_words()?, tts_client()?, ffmpeg()?, public_url(addr)?)));
    let (extract_workers, max_words, tts, ffmpeg, public_url) = match config {
        Ok(config) => config,
        Err(e) => {
            tracing::error!(error = %e, "invalid configuration");
            return ExitCode::FAILURE;
        }
    };
    let fetch_stage: Arc<dyn StageRunner> = Arc::new(FetchStage::new(fetcher, blobs.clone()));
    let extract_stage: Arc<dyn StageRunner> = Arc::new(ExtractStage::new(
        this_binary,
        vec!["extract-worker".into()],
        blobs.clone(),
        ExtractConfig::default(),
    ));
    let synthesise_stage: Arc<dyn StageRunner> = Arc::new(SynthesiseStage::new(
        tts,
        blobs.clone(),
        SynthesiseConfig {
            voice: std::env::var("RTM_TTS_VOICE").unwrap_or_else(|_| "bf_emma".into()),
            ..SynthesiseConfig::default()
        },
    ));
    let encode_stage: Arc<dyn StageRunner> = Arc::new(EncodeStage::new(
        blobs.clone(),
        EncodeConfig {
            ffmpeg,
            timeout: Duration::from_secs(1_800),
        },
    ));
    let publish_stage: Arc<dyn StageRunner> = Arc::new(PublishStage);
    let script_stage: Arc<dyn StageRunner> = Arc::new(ScriptStage::new(
        blobs.clone(),
        rtm_narrate::Options::default(),
        max_words,
    ));
    let (stop_workers, workers_stopped) = tokio::sync::watch::channel(false);
    let pools = [
        ("fetch", fetch.workers, &fetch_stage),
        ("extract", extract_workers, &extract_stage),
        ("script", 1, &script_stage),
        // One at a time: the TTS process synthesises one piece at a time anyway.
        ("synthesise", 1, &synthesise_stage),
        ("encode", 1, &encode_stage),
        ("publish", 1, &publish_stage),
    ];
    let workers: Vec<_> = pools
        .iter()
        .flat_map(|(name, count, stage)| {
            (0..*count).map(move |n| (format!("{name}-{n}"), Arc::clone(stage)))
        })
        .map(|(name, stage)| {
            let worker = Worker::new(name, Arc::clone(&store), stage);
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
    if public_url
        .host_str()
        .is_some_and(|h| h == "127.0.0.1" || h == "localhost" || h == "::1")
    {
        tracing::warn!(
            %public_url,
            "RTM_PUBLIC_URL is a loopback address: podcast apps on other devices can't reach \
             feed URLs; set it to how they reach this server"
        );
    }
    let router = rtm_api::router(store, token, rtm_api::FeedConfig { blobs, public_url });
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

const DEFAULT_TTS_LISTEN_ADDR: &str = "127.0.0.1:8090";

/// The engines this build has.
const ENGINES: &str = if cfg!(feature = "kokoro") {
    "kokoro, fake"
} else {
    "fake; kokoro needs a build with the kokoro feature"
};

/// The Kokoro engine, from RTM_KOKORO_DIR, checked against its pinned hashes.
#[cfg(feature = "kokoro")]
fn kokoro_engine() -> Result<Arc<dyn rtm_speech::SpeechEngine>, String> {
    let dir = std::env::var_os("RTM_KOKORO_DIR")
        .ok_or("RTM_KOKORO_DIR is not set: point it at the Kokoro model files")?;
    let threads = match std::env::var("RTM_KOKORO_THREADS") {
        Err(_) => std::thread::available_parallelism().map_or(4, |n| n.get()),
        Ok(n) => n
            .trim()
            .parse()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| format!("RTM_KOKORO_THREADS={n:?}: expected a positive number"))?,
    };
    let started = std::time::Instant::now();
    let engine = rtm_kokoro::KokoroEngine::load(std::path::Path::new(&dir), threads)
        .map_err(|e| format!("loading Kokoro: {e}"))?;
    tracing::info!(
        threads,
        took_ms = started.elapsed().as_millis() as u64,
        "Kokoro loaded"
    );
    Ok(Arc::new(engine))
}

/// The engine named by RTM_TTS_ENGINE.
fn tts_engine() -> Result<Arc<dyn rtm_speech::SpeechEngine>, String> {
    match std::env::var("RTM_TTS_ENGINE").as_deref() {
        Ok("fake") => Ok(Arc::new(rtm_speech::FakeEngine)),
        #[cfg(feature = "kokoro")]
        Ok("kokoro") => kokoro_engine(),
        Ok(other) => Err(format!(
            "RTM_TTS_ENGINE={other:?}: unknown engine (available: {ENGINES})"
        )),
        Err(_) => Err(format!("RTM_TTS_ENGINE is not set (available: {ENGINES})")),
    }
}

#[tokio::main]
async fn run_tts() -> ExitCode {
    init_logging();
    let raw =
        std::env::var("RTM_TTS_LISTEN_ADDR").unwrap_or_else(|_| DEFAULT_TTS_LISTEN_ADDR.into());
    let config = raw
        .parse::<SocketAddr>()
        .map_err(|e| format!("RTM_TTS_LISTEN_ADDR {raw:?} is not a socket address: {e}"))
        .and_then(|addr| Ok((addr, tts_engine()?)));
    let (addr, engine) = match config {
        Ok(config) => config,
        Err(e) => {
            tracing::error!(error = %e, "invalid configuration");
            return ExitCode::FAILURE;
        }
    };
    if engine.id() == "fake" {
        tracing::warn!("using the fake speech engine: episodes will be tones, not speech");
    }
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%addr, error = %e, "could not bind");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(%addr, engine = engine.id(), "rtm tts listening");
    let served = axum::serve(listener, rtm_tts::router(engine))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "tts server failed");
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
