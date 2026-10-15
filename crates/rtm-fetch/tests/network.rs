// SPDX-License-Identifier: AGPL-3.0-or-later
//! Real DNS and TLS, not scripted. Needs the internet, so ignored by default:
//! `cargo test -p rtm-fetch --test network -- --ignored`.

use rtm_fetch::{Config, FetchError, Kind, SafeFetcher};
use url::Url;

#[tokio::test]
#[ignore = "needs network access"]
async fn fetches_a_real_page_over_tls() {
    let guard = SafeFetcher::new(Config::default()).unwrap();
    let page = guard
        .fetch_article(&Url::parse("https://example.com/").unwrap())
        .await
        .unwrap();
    assert_eq!(page.kind, Kind::Html);
    assert!(String::from_utf8_lossy(&page.body).contains("Example Domain"));
}

/// Public DNS names that resolve to loopback (a classic SSRF trick) are refused.
#[tokio::test]
#[ignore = "needs network access"]
async fn public_names_for_private_addresses_are_refused() {
    let guard = SafeFetcher::new(Config::default()).unwrap();
    for target in ["http://localtest.me/", "http://127.0.0.1.nip.io/"] {
        let err = guard
            .fetch_article(&Url::parse(target).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Denied(_)), "{target}: {err:?}");
    }
}
