// SPDX-License-Identifier: AGPL-3.0-or-later
//! The egress guard against a scripted local HTTP server and scripted DNS: the AC-FET
//! scenarios, redirects, limits, content types and robots.txt.
//!
//! The server listens on 127.0.0.1, which the guard refuses by default, so tests that
//! need to reach it opt in with an unsafe allow for exactly 127.0.0.1/32 and its port.
//! Everything else in 127.0.0.0/8 stays refused.

use std::collections::HashMap;
use std::io::Write;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rtm_domain::FailureCode;
use rtm_fetch::{Config, DenyReason, FetchError, Kind, Lookup, Policy, SafeFetcher};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

// ---------------------------------------------------------------------------
// A scripted HTTP/1.1 server.

#[derive(Clone)]
enum Reply {
    /// Status line, headers and body, with Content-Length added.
    Fixed(u16, Vec<(&'static str, String)>, Vec<u8>),
    /// A 200 with no Content-Length, delimited by closing the connection, so the size is
    /// only discovered while reading.
    Unsized(&'static str, Vec<u8>),
    /// A 200 with no Content-Length that streams bytes until the client hangs up.
    Endless(&'static str),
}

fn ok(content_type: &str, body: impl Into<Vec<u8>>) -> Reply {
    Reply::Fixed(
        200,
        vec![("content-type", content_type.into())],
        body.into(),
    )
}

fn status(code: u16) -> Reply {
    Reply::Fixed(
        code,
        vec![("content-type", "text/plain".into())],
        b"no".to_vec(),
    )
}

fn redirect(to: &str) -> Reply {
    Reply::Fixed(302, vec![("location", to.into())], Vec::new())
}

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    headers: HashMap<String, String>,
}

type Handler = Arc<dyn Fn(&str) -> Reply + Send + Sync>;

struct Server {
    port: u16,
    connections: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    async fn start(handler: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler: Handler = Arc::new(handler);
        let (c, s) = (Arc::clone(&connections), Arc::clone(&seen));
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                c.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(serve(stream, Arc::clone(&handler), Arc::clone(&s)));
            }
        });
        Server {
            port,
            connections,
            seen,
        }
    }

    fn paths(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.path.clone())
            .collect()
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

async fn serve(mut stream: tokio::net::TcpStream, handler: Handler, seen: Arc<Mutex<Vec<Seen>>>) {
    let mut buf = Vec::new();
    loop {
        // Read one request head (GET only, so no body).
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        buf.drain(..head_end);
        let mut lines = head.split("\r\n");
        let path = lines.next().unwrap().split(' ').nth(1).unwrap().to_string();
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        seen.lock().unwrap().push(Seen {
            path: path.clone(),
            headers,
        });

        match handler(&path) {
            Reply::Fixed(code, headers, body) => {
                let mut out = format!("HTTP/1.1 {code} X\r\ncontent-length: {}\r\n", body.len());
                for (k, v) in headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                let mut out = out.into_bytes();
                out.extend_from_slice(&body);
                if stream.write_all(&out).await.is_err() {
                    return;
                }
            }
            Reply::Unsized(content_type, body) => {
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n"
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
                return;
            }
            Reply::Endless(content_type) => {
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n"
                );
                if stream.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                let chunk = vec![b'a'; 64 * 1024];
                while stream.write_all(&chunk).await.is_ok() {}
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Scripted DNS.

/// Answers from a table; each host can have a sequence of answers (the last repeats),
/// to simulate records that change between lookups.
#[derive(Default)]
struct Dns {
    answers: Mutex<HashMap<String, Vec<Vec<IpAddr>>>>,
    lookups: AtomicUsize,
}

impl Dns {
    fn with(entries: &[(&str, &[&str])]) -> Arc<Dns> {
        let dns = Dns::default();
        for (host, ips) in entries {
            dns.answer(host, ips);
        }
        Arc::new(dns)
    }

    fn answer(&self, host: &str, ips: &[&str]) {
        let ips = ips.iter().map(|ip| ip.parse().unwrap()).collect();
        self.answers
            .lock()
            .unwrap()
            .entry(host.into())
            .or_default()
            .push(ips);
    }

    fn lookups(&self) -> usize {
        self.lookups.load(Ordering::SeqCst)
    }
}

impl Lookup for Dns {
    fn lookup<'a>(
        &'a self,
        host: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<Vec<IpAddr>>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            let mut answers = self.answers.lock().unwrap();
            let seq = answers
                .get_mut(host)
                .ok_or_else(|| std::io::Error::other("NXDOMAIN"))?;
            Ok(if seq.len() > 1 {
                seq.remove(0)
            } else {
                seq[0].clone()
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Fetchers.

/// A fetcher that may reach the test server (127.0.0.1/32 on its port) and nothing
/// else private.
fn fetcher(server: &Server, dns: Arc<Dns>, tweak: impl FnOnce(&mut Config)) -> SafeFetcher {
    let mut config = Config {
        policy: Policy::default()
            .unsafe_allow(["127.0.0.1/32".parse().unwrap()])
            .allow_ports([server.port]),
        ..Config::default()
    };
    tweak(&mut config);
    SafeFetcher::with_lookup(config, dns)
}

/// A fetcher with the default policy, which refuses the test server's address.
fn strict_fetcher(server: &Server, dns: Arc<Dns>) -> SafeFetcher {
    let config = Config {
        policy: Policy::default().allow_ports([server.port]),
        ..Config::default()
    };
    SafeFetcher::with_lookup(config, dns)
}

fn url(server: &Server, host: &str, path: &str) -> Url {
    Url::parse(&format!("http://{host}:{}{path}", server.port)).unwrap()
}

const ARTICLE: &str = "<!doctype html><title>A</title><p>Words.</p>";

/// The usual site: no robots.txt, one article.
fn site(path: &str) -> Reply {
    match path {
        "/robots.txt" => status(404),
        _ => ok("text/html; charset=utf-8", ARTICLE),
    }
}

// ---------------------------------------------------------------------------
// AC-FET-01: internal destinations are blocked, without a connection attempt.

#[tokio::test]
async fn internal_destinations_are_blocked_before_connecting() {
    let server = Server::start(site).await;
    let dns = Dns::with(&[
        ("internal.test", &["127.0.0.1"]),
        ("ten.test", &["10.0.0.5"]),
        ("mixed.test", &["93.184.215.14", "127.0.0.1"]),
        ("mapped.test", &["::ffff:127.0.0.1"]),
    ]);
    let guard = strict_fetcher(&server, Arc::clone(&dns));
    let port = server.port;
    for target in [
        format!("http://127.0.0.1:{port}/"),
        format!("http://[::1]:{port}/"),
        format!("http://169.254.169.254:{port}/latest/meta-data/"),
        format!("http://10.0.0.5:{port}/"),
        format!("http://0x7f000001:{port}/"),
        format!("http://2130706433:{port}/"),
        format!("http://[::ffff:7f00:1]:{port}/"),
        format!("http://internal.test:{port}/"),
        format!("http://ten.test:{port}/"),
        format!("http://mixed.test:{port}/"),
        format!("http://mapped.test:{port}/"),
    ] {
        let result = guard.fetch_article(&Url::parse(&target).unwrap()).await;
        assert!(
            matches!(result, Err(FetchError::Denied(DenyReason::Address))),
            "{target}: {result:?}"
        );
        assert_eq!(
            result.unwrap_err().code(),
            FailureCode::DestinationNotAllowed
        );
    }
    assert_eq!(
        server.connections(),
        0,
        "a blocked destination was connected to"
    );
}

#[tokio::test]
async fn other_schemes_and_ports_are_blocked() {
    let server = Server::start(site).await;
    let guard = strict_fetcher(&server, Dns::with(&[]));
    for (target, reason) in [
        ("file:///etc/passwd", DenyReason::Scheme),
        ("gopher://example.com/", DenyReason::Scheme),
        ("ftp://example.com/", DenyReason::Scheme),
        ("http://user:pw@example.com/", DenyReason::Credentials),
        ("http://example.com:22/", DenyReason::Port),
    ] {
        let result = guard.fetch_article(&Url::parse(target).unwrap()).await;
        assert_eq!(result.unwrap_err(), FetchError::Denied(reason), "{target}");
    }
}

/// Rebinding: the check and the connection use the same single lookup, so a record
/// that flips to a private address afterwards is never connected to.
#[tokio::test]
async fn dns_rebinding_cannot_swap_the_address_after_the_check() {
    let server = Server::start(site).await;
    let dns = Dns::with(&[("rebind.test", &["127.0.0.1"])]);
    dns.answer("rebind.test", &["10.0.0.1"]);
    let guard = fetcher(&server, Arc::clone(&dns), |c| c.ignore_robots = true);

    let fetched = guard
        .fetch_article(&url(&server, "rebind.test", "/a"))
        .await
        .unwrap();
    assert_eq!(fetched.body, ARTICLE.as_bytes());
    assert_eq!(dns.lookups(), 1, "resolved more than once");
}

/// The first answer is private, the next would be public: the first decides.
#[tokio::test]
async fn a_private_first_answer_is_not_retried_into_a_public_one() {
    let server = Server::start(site).await;
    let dns = Dns::with(&[("flip.test", &["127.0.0.2"])]);
    dns.answer("flip.test", &["127.0.0.1"]);
    let guard = fetcher(&server, Arc::clone(&dns), |c| c.ignore_robots = true);
    let result = guard.fetch_article(&url(&server, "flip.test", "/a")).await;
    assert_eq!(result.unwrap_err(), FetchError::Denied(DenyReason::Address));
    assert_eq!(server.connections(), 0);
}

// ---------------------------------------------------------------------------
// FR-FET-02: redirects, each hop checked.

#[tokio::test]
async fn redirects_to_internal_destinations_are_blocked() {
    let port = Arc::new(AtomicUsize::new(0));
    let p = Arc::clone(&port);
    let server = Server::start(move |path| match path {
        "/robots.txt" => status(404),
        "/to-ten" => redirect("http://10.0.0.1/"),
        "/to-metadata" => redirect("http://169.254.169.254/latest/meta-data/"),
        "/to-private-name" => redirect(&format!(
            "http://private.test:{}/",
            p.load(Ordering::SeqCst)
        )),
        "/to-file" => redirect("file:///etc/passwd"),
        _ => ok("text/html", ARTICLE),
    })
    .await;
    port.store(server.port as usize, Ordering::SeqCst);
    let dns = Dns::with(&[
        ("public.test", &["127.0.0.1"]),
        ("private.test", &["127.0.0.2"]),
    ]);
    let guard = fetcher(&server, dns, |c| c.ignore_robots = true);

    for (path, reason) in [
        ("/to-ten", DenyReason::Address),
        ("/to-metadata", DenyReason::Address),
        ("/to-private-name", DenyReason::Address),
        ("/to-file", DenyReason::Scheme),
    ] {
        let result = guard
            .fetch_article(&url(&server, "public.test", path))
            .await;
        assert_eq!(result.unwrap_err(), FetchError::Denied(reason), "{path}");
    }
    // Only the redirecting requests reached the server.
    assert_eq!(
        server.paths(),
        ["/to-ten", "/to-metadata", "/to-private-name", "/to-file"]
    );
}

#[tokio::test]
async fn redirects_are_followed_up_to_the_limit() {
    let server = Server::start(|path| match path {
        "/robots.txt" => status(404),
        "/hop1" => redirect("/hop2"),
        "/hop2" => redirect("/article"),
        "/loop" => redirect("/loop"),
        _ => ok("text/html", ARTICLE),
    })
    .await;
    let guard = fetcher(
        &server,
        Dns::with(&[("public.test", &["127.0.0.1"])]),
        |c| {
            c.ignore_robots = true;
        },
    );

    let fetched = guard
        .fetch_article(&url(&server, "public.test", "/hop1"))
        .await
        .unwrap();
    assert_eq!(fetched.final_url.path(), "/article");

    let result = guard
        .fetch_article(&url(&server, "public.test", "/loop"))
        .await;
    assert_eq!(result.unwrap_err(), FetchError::TooManyRedirects);
    // The first request plus five redirects.
    assert_eq!(server.paths().iter().filter(|p| *p == "/loop").count(), 6);
}

// ---------------------------------------------------------------------------
// AC-FET-02/03: size limits.

#[tokio::test]
async fn an_endless_body_is_cut_off_at_the_limit() {
    let server = Server::start(|path| match path {
        "/robots.txt" => status(404),
        _ => Reply::Endless("text/html"),
    })
    .await;
    let guard = fetcher(&server, Dns::with(&[("big.test", &["127.0.0.1"])]), |c| {
        c.ignore_robots = true;
        c.max_body = 1024 * 1024;
    });
    let result = guard.fetch_article(&url(&server, "big.test", "/")).await;
    assert_eq!(result.unwrap_err().code(), FailureCode::SourceTooLarge);
}

#[tokio::test]
async fn a_streamed_body_past_the_limit_fails() {
    // No Content-Length: the cap applies while reading.
    let server = Server::start(|path| match path {
        "/robots.txt" => status(404),
        "/fits" => Reply::Unsized("text/html", ARTICLE.as_bytes().to_vec()),
        _ => Reply::Unsized("text/html", vec![b'a'; 2 * 1024 * 1024]),
    })
    .await;
    let guard = fetcher(&server, Dns::with(&[("big.test", &["127.0.0.1"])]), |c| {
        c.ignore_robots = true;
        c.max_body = 1024 * 1024;
    });
    let result = guard.fetch_article(&url(&server, "big.test", "/")).await;
    assert_eq!(result.unwrap_err(), FetchError::TooLarge);
    let fits = guard
        .fetch_article(&url(&server, "big.test", "/fits"))
        .await
        .unwrap();
    assert_eq!(fits.body, ARTICLE.as_bytes());
}

#[tokio::test]
async fn a_declared_length_past_the_limit_fails() {
    let server = Server::start(|path| match path {
        "/robots.txt" => status(404),
        _ => ok("text/html", vec![b'a'; 2 * 1024 * 1024]),
    })
    .await;
    let guard = fetcher(&server, Dns::with(&[("big.test", &["127.0.0.1"])]), |c| {
        c.ignore_robots = true;
        c.max_body = 1024 * 1024;
    });
    let result = guard.fetch_article(&url(&server, "big.test", "/")).await;
    assert_eq!(result.unwrap_err(), FetchError::TooLarge);
}

#[tokio::test]
async fn a_decompression_bomb_stops_at_the_limit() {
    // 64 MiB of zeros compresses to about 64 KiB.
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    let zeros = vec![0u8; 1024 * 1024];
    for _ in 0..64 {
        gz.write_all(&zeros).unwrap();
    }
    let bomb = gz.finish().unwrap();
    assert!(bomb.len() < 200 * 1024);
    let server = Server::start(move |path| match path {
        "/robots.txt" => status(404),
        _ => Reply::Fixed(
            200,
            vec![
                ("content-type", "text/html".into()),
                ("content-encoding", "gzip".into()),
            ],
            bomb.clone(),
        ),
    })
    .await;
    let guard = fetcher(&server, Dns::with(&[("bomb.test", &["127.0.0.1"])]), |c| {
        c.ignore_robots = true;
        c.max_body = 1024 * 1024;
    });
    let result = guard.fetch_article(&url(&server, "bomb.test", "/")).await;
    assert_eq!(result.unwrap_err(), FetchError::TooLarge);
}

#[tokio::test]
async fn compressed_articles_are_decompressed() {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(ARTICLE.as_bytes()).unwrap();
    let body = gz.finish().unwrap();
    let server = Server::start(move |path| match path {
        "/robots.txt" => status(404),
        _ => Reply::Fixed(
            200,
            vec![
                ("content-type", "text/html".into()),
                ("content-encoding", "gzip".into()),
            ],
            body.clone(),
        ),
    })
    .await;
    let guard = fetcher(&server, Dns::with(&[("gz.test", &["127.0.0.1"])]), |_| {});
    let fetched = guard
        .fetch_article(&url(&server, "gz.test", "/"))
        .await
        .unwrap();
    assert_eq!(fetched.body, ARTICLE.as_bytes());
}

// ---------------------------------------------------------------------------
// FR-FET-04: content types.

#[tokio::test]
async fn content_types_are_allow_listed_and_sniffed() {
    let server = Server::start(|path| match path {
        "/robots.txt" => status(404),
        "/png" => ok("image/png", b"\x89PNG\r\n\x1a\n".to_vec()),
        "/pdf" => ok("application/pdf", b"%PDF-1.7".to_vec()),
        "/liar" => ok("text/html", b"\x89PNG\r\n\x1a\nnot html".to_vec()),
        "/plain" => ok("text/plain; charset=iso-8859-1", "Just words.".as_bytes()),
        "/undeclared" => Reply::Fixed(200, vec![], ARTICLE.as_bytes().to_vec()),
        _ => ok("text/html; charset=UTF-8", ARTICLE),
    })
    .await;
    let guard = fetcher(&server, Dns::with(&[("t.test", &["127.0.0.1"])]), |_| {});
    let get = |p: &'static str| {
        let target = url(&server, "t.test", p);
        let guard = &guard;
        async move { guard.fetch_article(&target).await }
    };

    for p in ["/png", "/pdf", "/liar"] {
        assert_eq!(
            get(p).await.unwrap_err().code(),
            FailureCode::UnsupportedContentType,
            "{p}"
        );
    }
    let html = get("/article").await.unwrap();
    assert_eq!(
        (html.kind, html.charset.as_deref()),
        (Kind::Html, Some("utf-8"))
    );
    let plain = get("/plain").await.unwrap();
    assert_eq!(
        (plain.kind, plain.charset.as_deref()),
        (Kind::PlainText, Some("iso-8859-1"))
    );
    assert_eq!(get("/undeclared").await.unwrap().kind, Kind::Html);
}

#[tokio::test]
async fn http_errors_are_classified() {
    let server = Server::start(|path| match path {
        "/robots.txt" => status(404),
        "/404" => status(404),
        "/410" => status(410),
        "/403" => status(403),
        "/429" => status(429),
        "/503" => status(503),
        _ => ok("text/html", ARTICLE),
    })
    .await;
    let guard = fetcher(&server, Dns::with(&[("e.test", &["127.0.0.1"])]), |_| {});
    for (p, code) in [
        ("/404", FailureCode::SourceUnavailable),
        ("/410", FailureCode::SourceUnavailable),
        ("/403", FailureCode::SourceUnavailable),
        ("/429", FailureCode::FetchFailed),
        ("/503", FailureCode::FetchFailed),
    ] {
        let err = guard
            .fetch_article(&url(&server, "e.test", p))
            .await
            .unwrap_err();
        assert_eq!(err.code(), code, "{p}");
    }
    let err = guard
        .fetch_article(&url(&server, "nxdomain.test", "/"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), FailureCode::FetchFailed);
}

// ---------------------------------------------------------------------------
// FR-FET-05/06: honest identity, no ambient credentials.

#[tokio::test]
async fn requests_are_honest_and_carry_no_cookies() {
    let server = Server::start(|path| match path {
        "/robots.txt" => status(404),
        _ => Reply::Fixed(
            200,
            vec![
                ("content-type", "text/html".into()),
                ("set-cookie", "session=abc".into()),
            ],
            ARTICLE.as_bytes().to_vec(),
        ),
    })
    .await;
    let guard = fetcher(&server, Dns::with(&[("c.test", &["127.0.0.1"])]), |_| {});
    guard
        .fetch_article(&url(&server, "c.test", "/1"))
        .await
        .unwrap();
    guard
        .fetch_article(&url(&server, "c.test", "/2"))
        .await
        .unwrap();

    let seen = server.seen.lock().unwrap().clone();
    assert!(!seen.is_empty());
    for s in &seen {
        let ua = &s.headers["user-agent"];
        assert!(
            ua.starts_with("read-to-me/") && ua.contains("(+https://"),
            "{ua}"
        );
        assert!(
            !s.headers.contains_key("cookie"),
            "sent a cookie: {:?}",
            s.headers
        );
        assert!(!s.headers.contains_key("authorization"));
    }
}

// ---------------------------------------------------------------------------
// AC-FET-04/05: robots.txt.

fn robots_site(robots: Reply) -> impl Fn(&str) -> Reply + Send + Sync + 'static {
    move |path| match path {
        "/robots.txt" => robots.clone(),
        _ => ok("text/html", ARTICLE),
    }
}

#[tokio::test]
async fn robots_disallow_stops_the_article_request() {
    let robots = "User-agent: *\nAllow: /\n\nUser-agent: read-to-me\nDisallow: /private\n";
    let server = Server::start(robots_site(ok("text/plain", robots))).await;
    let guard = fetcher(&server, Dns::with(&[("r.test", &["127.0.0.1"])]), |_| {});

    let err = guard
        .fetch_article(&url(&server, "r.test", "/private/essay"))
        .await
        .unwrap_err();
    assert_eq!(err, FetchError::RobotsDisallowed);
    assert_eq!(err.code(), FailureCode::RobotsDisallowed);
    assert!(
        err.user_message().contains("upload"),
        "{}",
        err.user_message()
    );
    assert_eq!(
        server.paths(),
        ["/robots.txt"],
        "the article itself was requested"
    );

    // Other paths are allowed, and robots.txt is cached for the origin.
    guard
        .fetch_article(&url(&server, "r.test", "/public"))
        .await
        .unwrap();
    assert_eq!(server.paths(), ["/robots.txt", "/public"]);
}

#[tokio::test]
async fn disrespectful_mode_skips_robots_txt() {
    let server = Server::start(robots_site(ok(
        "text/plain",
        "User-agent: *\nDisallow: /\n",
    )))
    .await;
    let guard = fetcher(&server, Dns::with(&[("r.test", &["127.0.0.1"])]), |c| {
        c.ignore_robots = true;
    });
    guard
        .fetch_article(&url(&server, "r.test", "/essay"))
        .await
        .unwrap();
    assert_eq!(server.paths(), ["/essay"]);
}

#[tokio::test]
async fn missing_robots_allows_and_failing_robots_retries() {
    let missing = Server::start(robots_site(status(404))).await;
    let guard = fetcher(&missing, Dns::with(&[("r.test", &["127.0.0.1"])]), |_| {});
    guard
        .fetch_article(&url(&missing, "r.test", "/essay"))
        .await
        .unwrap();

    let failing = Server::start(robots_site(status(503))).await;
    let guard = fetcher(&failing, Dns::with(&[("r.test", &["127.0.0.1"])]), |_| {});
    let err = guard
        .fetch_article(&url(&failing, "r.test", "/essay"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), FailureCode::FetchFailed);
    assert_eq!(failing.paths(), ["/robots.txt"]);
}

/// RFC 9309 §2.3.1.3: every 4xx means "no restrictions", including the ones that are
/// retried for articles.
#[tokio::test]
async fn every_client_error_for_robots_allows() {
    for code in [400, 401, 403, 404, 408, 410, 425, 429, 451] {
        let server = Server::start(robots_site(status(code))).await;
        let guard = fetcher(&server, Dns::with(&[("r.test", &["127.0.0.1"])]), |_| {});
        let result = guard.fetch_article(&url(&server, "r.test", "/essay")).await;
        assert!(result.is_ok(), "robots.txt {code}: {result:?}");
        assert_eq!(
            server.paths(),
            ["/robots.txt", "/essay"],
            "robots.txt {code}"
        );
    }
    for code in [500, 502, 503] {
        let server = Server::start(robots_site(status(code))).await;
        let guard = fetcher(&server, Dns::with(&[("r.test", &["127.0.0.1"])]), |_| {});
        let err = guard
            .fetch_article(&url(&server, "r.test", "/essay"))
            .await
            .unwrap_err();
        assert_eq!(err.code(), FailureCode::FetchFailed, "robots.txt {code}");
    }
}

#[tokio::test]
async fn robots_txt_on_a_redirect_target_is_checked_too() {
    let port = Arc::new(AtomicUsize::new(0));
    let p = Arc::clone(&port);
    let server = Server::start(move |path| match path {
        "/robots.txt" => ok("text/plain", "User-agent: *\nDisallow: /blocked\n"),
        "/go" => redirect(&format!(
            "http://other.test:{}/blocked",
            p.load(Ordering::SeqCst)
        )),
        _ => ok("text/html", ARTICLE),
    })
    .await;
    port.store(server.port as usize, Ordering::SeqCst);
    let dns = Dns::with(&[("one.test", &["127.0.0.1"]), ("other.test", &["127.0.0.1"])]);
    let guard = fetcher(&server, dns, |_| {});
    let err = guard
        .fetch_article(&url(&server, "one.test", "/go"))
        .await
        .unwrap_err();
    assert_eq!(err, FetchError::RobotsDisallowed);
    assert_eq!(server.paths(), ["/robots.txt", "/go", "/robots.txt"]);
}
