// SPDX-License-Identifier: AGPL-3.0-or-later
//! The egress guard (ADR-0005): the only way read-to-me makes outbound HTTP requests.
//!
//! Every hop is checked before any network activity ([`Policy::check_url`]). Hostnames
//! are resolved once, by us, inside the HTTP client's connector: every address returned
//! is checked, and the client connects only to those addresses, so a DNS answer can't
//! change between the check and the connection (rebinding). Redirects are followed by
//! hand so each hop is checked again. Bodies are size-capped after decompression.

mod policy;
mod robots;
mod sniff;

use std::error::Error as _;
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use reqwest::StatusCode;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION};
use rtm_domain::FailureCode;
use url::Url;

pub use policy::{DenyReason, Policy};
pub use sniff::Kind;

/// Longest URL accepted at any hop (FR-SUB-02).
pub const MAX_URL_LEN: usize = 2_048;

/// The product token robots.txt rules are matched against (FR-FET-07).
pub const PRODUCT_TOKEN: &str = "read-to-me";

/// Honest User-Agent: product token, version and project URL (FR-FET-05).
pub const USER_AGENT: &str = concat!(
    "read-to-me/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/huwd/read-to-me-server)"
);

/// Fetch limits and behaviour (FR-FET-03, FR-FET-07/08).
#[derive(Debug, Clone)]
pub struct Config {
    pub policy: Policy,
    pub connect_timeout: Duration,
    /// For the whole fetch, including robots.txt and every redirect.
    pub total_timeout: Duration,
    /// Largest article body accepted, after decompression.
    pub max_body: usize,
    pub max_redirects: usize,
    /// Disrespectful mode (FR-FET-08): skip robots.txt entirely.
    pub ignore_robots: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            policy: Policy::default(),
            connect_timeout: Duration::from_secs(5),
            total_timeout: Duration::from_secs(30),
            max_body: 10 * 1024 * 1024,
            max_redirects: 5,
            ignore_robots: false,
        }
    }
}

/// Why a fetch failed. [`FetchError::code`] maps it to the user-facing failure code;
/// internal detail (addresses, reasons) stays in logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    Denied(DenyReason),
    RobotsDisallowed,
    TooLarge,
    UnsupportedContentType(String),
    TooManyRedirects,
    /// A final response that isn't the article and won't become one on retry.
    Unavailable(u16),
    /// Worth retrying later: network errors, timeouts, 5xx, 429, unreachable robots.txt.
    Transient(String),
}

impl FetchError {
    pub fn code(&self) -> FailureCode {
        match self {
            FetchError::Denied(_) => FailureCode::DestinationNotAllowed,
            FetchError::RobotsDisallowed => FailureCode::RobotsDisallowed,
            FetchError::TooLarge => FailureCode::SourceTooLarge,
            FetchError::UnsupportedContentType(_) => FailureCode::UnsupportedContentType,
            FetchError::TooManyRedirects | FetchError::Unavailable(_) => {
                FailureCode::SourceUnavailable
            }
            FetchError::Transient(_) => FailureCode::FetchFailed,
        }
    }

    /// A message safe to show the user: no addresses or internal reasons.
    pub fn user_message(&self) -> String {
        match self {
            FetchError::Denied(_) => "That address can't be fetched by this server.".into(),
            FetchError::RobotsDisallowed => "The site's robots.txt asks automated readers not \
                 to fetch this page. You can upload the page's HTML or PDF instead."
                .into(),
            FetchError::TooLarge => "The page is too large to fetch.".into(),
            FetchError::UnsupportedContentType(t) => {
                format!("The page isn't an article this server can read ({t}).")
            }
            FetchError::TooManyRedirects => "The page redirects too many times.".into(),
            FetchError::Unavailable(status) => format!("The site answered with HTTP {status}."),
            FetchError::Transient(_) => "The page couldn't be reached; it will be retried.".into(),
        }
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::Denied(r) => write!(f, "destination denied: {}", r.as_str()),
            FetchError::Transient(detail) => write!(f, "transient failure: {detail}"),
            other => write!(f, "{}", other.code().as_str()),
        }
    }
}

impl std::error::Error for FetchError {}

/// A fetched article.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// Where the article was finally found, after redirects.
    pub final_url: Url,
    pub kind: Kind,
    /// The `charset` parameter of the declared content type, if any.
    pub charset: Option<String>,
    pub body: Bytes,
}

/// Hostname lookup, separated out so tests can script DNS answers.
pub trait Lookup: Send + Sync + 'static {
    fn lookup<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send + 'a>>;
}

/// The system's DNS configuration, through hickory.
struct SystemLookup(hickory_resolver::TokioResolver);

impl Lookup for SystemLookup {
    fn lookup<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send + 'a>> {
        Box::pin(async move {
            let found = self.0.lookup_ip(host).await.map_err(io::Error::other)?;
            Ok(found.iter().collect())
        })
    }
}

/// Raised inside the connector when a resolved address is refused; found again by
/// walking the HTTP client's error chain.
#[derive(Debug)]
struct Denied(DenyReason);

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "destination denied: {}", self.0.as_str())
    }
}

impl std::error::Error for Denied {}

/// The HTTP client's resolver: look up, check every address, and hand back exactly the
/// checked addresses. The client connects to nothing else.
struct GuardedResolver {
    lookup: Arc<dyn Lookup>,
    policy: Arc<Policy>,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let lookup = Arc::clone(&self.lookup);
        let policy = Arc::clone(&self.policy);
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = lookup.lookup(&host).await?;
            if let Err(reason) = policy.check_resolved(&addrs) {
                security_event(reason, &host);
                return Err(Box::new(Denied(reason)) as _);
            }
            // The port is replaced by the client with the URL's port.
            let addrs: Addrs = Box::new(addrs.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// Log a refused destination as a security event (ADR-0005). Only the host is
/// recorded: paths and queries can carry secrets.
fn security_event(reason: DenyReason, host: &str) {
    tracing::warn!(
        target: "security",
        event = "egress_blocked",
        reason = reason.as_str(),
        host,
        "outbound request refused"
    );
}

/// Map a client error to a fetch error, finding a refusal from the resolver if there
/// was one.
fn classify(err: &reqwest::Error) -> FetchError {
    let mut source = err.source();
    while let Some(e) = source {
        if let Some(Denied(reason)) = e.downcast_ref::<Denied>() {
            return FetchError::Denied(*reason);
        }
        source = e.source();
    }
    if err.is_timeout() {
        FetchError::Transient("timed out".into())
    } else if err.is_connect() {
        FetchError::Transient("could not connect".into())
    } else {
        FetchError::Transient("request failed".into())
    }
}

/// The egress guard. Cheap to share; holds a connection pool and the robots.txt cache.
pub struct SafeFetcher {
    client: reqwest::Client,
    policy: Arc<Policy>,
    config: Config,
    robots: robots::Cache,
}

/// A final (non-redirect) response from [`SafeFetcher::get`].
struct Response {
    url: Url,
    inner: reqwest::Response,
}

impl SafeFetcher {
    /// A fetcher using the system's DNS configuration.
    pub fn new(config: Config) -> io::Result<Self> {
        let resolver = hickory_resolver::TokioResolver::builder_tokio()
            .and_then(|b| b.build())
            .map_err(io::Error::other)?;
        Ok(Self::with_lookup(config, Arc::new(SystemLookup(resolver))))
    }

    /// A fetcher using `lookup` for DNS. For tests, and for operators who need a
    /// specific resolver.
    pub fn with_lookup(config: Config, lookup: Arc<dyn Lookup>) -> Self {
        let policy = Arc::new(config.policy.clone());
        let resolver: Arc<dyn Resolve> = Arc::new(GuardedResolver {
            lookup,
            policy: Arc::clone(&policy),
        });
        // The one place an HTTP client is built (ADR-0005); the lint ban elsewhere
        // keeps it that way.
        #[expect(clippy::disallowed_methods, reason = "this is the egress guard")]
        let builder = reqwest::Client::builder();
        let client = builder
            .dns_resolver(resolver)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .connect_timeout(config.connect_timeout)
            .timeout(config.total_timeout)
            .user_agent(USER_AGENT)
            .build()
            .expect("static HTTP client configuration is valid");
        Self {
            client,
            policy,
            config,
            robots: robots::Cache::default(),
        }
    }

    /// Fetch an article (FR-FET-01..08): robots.txt permitting, through the guard,
    /// within the size limit, and only if it's a type we can read.
    pub async fn fetch_article(&self, url: &Url) -> Result<Fetched, FetchError> {
        tokio::time::timeout(self.config.total_timeout, self.fetch_article_inner(url))
            .await
            .unwrap_or_else(|_| Err(FetchError::Transient("timed out".into())))
    }

    async fn fetch_article_inner(&self, url: &Url) -> Result<Fetched, FetchError> {
        let response = self.get(url, true).await?;
        let declared = response
            .inner
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        // Refuse unwanted types before reading the body.
        if let Some(declared) = &declared {
            sniff::check_declared(declared)?;
        }
        let final_url = response.url;
        let body = read_capped(response.inner, self.config.max_body).await?;
        let kind = sniff::reconcile(declared.as_deref(), &body)?;
        Ok(Fetched {
            final_url,
            kind,
            charset: declared.as_deref().and_then(sniff::charset),
            body,
        })
    }

    /// GET `url`, following redirects by hand. Every hop is checked by the policy and,
    /// if `respect_robots`, against the hop's robots.txt before it is requested.
    async fn get(&self, url: &Url, respect_robots: bool) -> Result<Response, FetchError> {
        let mut url = url.clone();
        for _ in 0..=self.config.max_redirects {
            if let Err(reason) = self.policy.check_url(&url) {
                security_event(reason, url.host_str().unwrap_or(""));
                return Err(FetchError::Denied(reason));
            }
            if respect_robots && !self.config.ignore_robots && !self.robots_allow(&url).await? {
                return Err(FetchError::RobotsDisallowed);
            }
            let response = self
                .client
                .get(url.clone())
                .send()
                .await
                .map_err(|e| classify(&e))?;
            let status = response.status();
            if status.is_redirection() {
                let next = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|loc| url.join(loc).ok());
                match next {
                    Some(next) => {
                        url = next;
                        continue;
                    }
                    None => return Err(FetchError::Unavailable(status.as_u16())),
                }
            }
            if status.is_success() {
                return Ok(Response {
                    url,
                    inner: response,
                });
            }
            return Err(status_error(status));
        }
        Err(FetchError::TooManyRedirects)
    }

    /// Whether robots.txt for `url`'s origin permits fetching it (FR-FET-07).
    async fn robots_allow(&self, url: &Url) -> Result<bool, FetchError> {
        let origin = url.origin().ascii_serialization();
        if let Some(rules) = self.robots.get(&origin) {
            return Ok(rules.allowed(url));
        }
        let robots_url = url.join("/robots.txt").expect("joining an absolute path");
        // robots.txt itself is fetched through the guard, without a robots check.
        let rules = match Box::pin(self.get(&robots_url, false)).await {
            Ok(response) => {
                let body = read_truncated(response.inner, robots::MAX_SIZE).await?;
                robots::Rules::parse(&body)
            }
            // RFC 9309 §2.3.1.3: "unavailable" (4xx) means no restrictions.
            Err(FetchError::Unavailable(status)) if (400..500).contains(&status) => {
                robots::Rules::AllowAll
            }
            Err(FetchError::TooManyRedirects) => robots::Rules::AllowAll,
            // §2.3.1.4: "unreachable" (5xx, network) means assume complete disallow,
            // so retry later rather than fail the item.
            Err(FetchError::Transient(detail)) => {
                return Err(FetchError::Transient(format!(
                    "robots.txt unreachable: {detail}"
                )));
            }
            Err(other) => return Err(other),
        };
        let allowed = rules.allowed(url);
        self.robots.insert(origin, rules);
        Ok(allowed)
    }
}

fn status_error(status: StatusCode) -> FetchError {
    match status.as_u16() {
        408 | 425 | 429 => FetchError::Transient(format!("HTTP {status}")),
        s if status.is_server_error() => FetchError::Transient(format!("HTTP {s}")),
        s => FetchError::Unavailable(s),
    }
}

/// Read a body, failing once it passes `max` bytes. The client decompresses as it reads,
/// so the cap applies to the decompressed size and a compression bomb stops at `max`.
async fn read_capped(mut response: reqwest::Response, max: usize) -> Result<Bytes, FetchError> {
    let declared = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    // Content-Length counts encoded bytes, so this only catches the obvious cases early.
    if declared.is_some_and(|n| n > max as u64) {
        return Err(FetchError::TooLarge);
    }
    let mut body = BytesMut::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| classify(&e))? {
        if body.len() + chunk.len() > max {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.freeze())
}

/// Read at most `max` bytes of a body and ignore the rest (robots.txt: RFC 9309 §2.5).
async fn read_truncated(mut response: reqwest::Response, max: usize) -> Result<Bytes, FetchError> {
    let mut body = BytesMut::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| classify(&e))? {
        let room = max - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if body.len() == max {
            break;
        }
    }
    Ok(body.freeze())
}
