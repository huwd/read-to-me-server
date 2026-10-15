// SPDX-License-Identifier: AGPL-3.0-or-later
//! AC-FET-01: blocked destinations are logged as `egress_blocked` security events, with
//! the host only (paths and queries can carry secrets).
//!
//! Its own test binary, with a global subscriber (see rtm-api's log_redaction test).

use std::io::Write;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use rtm_fetch::{Config, Lookup, SafeFetcher};
use url::Url;

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Private;

impl Lookup for Private {
    fn lookup<'a>(
        &'a self,
        _host: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<Vec<IpAddr>>> + Send + 'a>,
    > {
        Box::pin(async { Ok(vec!["10.0.0.1".parse().unwrap()]) })
    }
}

#[tokio::test]
async fn blocked_destinations_are_logged_without_paths() {
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let guard = SafeFetcher::with_lookup(Config::default(), Arc::new(Private));
    for target in [
        "http://169.254.169.254/latest/meta-data/SECRET?token=SECRET",
        "http://intranet.test/SECRET?token=SECRET",
    ] {
        assert!(
            guard
                .fetch_article(&Url::parse(target).unwrap())
                .await
                .is_err()
        );
    }

    let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    let events: Vec<&str> = logs
        .lines()
        .filter(|l| l.contains("egress_blocked"))
        .collect();
    assert_eq!(events.len(), 2, "{logs}");
    assert!(
        events[0].contains("host=\"169.254.169.254\"") && events[0].contains("reason=\"address\"")
    );
    assert!(events[1].contains("host=\"intranet.test\""));
    assert!(events.iter().all(|e| e.contains("security")));
    assert!(!logs.contains("SECRET"), "path or query leaked:\n{logs}");
}
