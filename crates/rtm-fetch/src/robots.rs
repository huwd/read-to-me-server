// SPDX-License-Identifier: AGPL-3.0-or-later
//! robots.txt rules and their per-origin cache (FR-FET-07, RFC 9309).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use url::Url;

/// RFC 9309 §2.5: parse at least 500 KiB; anything past that is ignored.
pub const MAX_SIZE: usize = 500 * 1024;

/// How long an origin's rules are reused (FR-FET-07; RFC 9309 §2.4 allows up to 24 h).
const TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Bound the cache so a stream of distinct origins can't grow it without limit.
const MAX_ORIGINS: usize = 1_024;

pub enum Rules {
    AllowAll,
    Parsed(texting_robots::Robot),
}

impl Rules {
    /// Rules for our product token, falling back to the `*` group.
    pub fn parse(body: &[u8]) -> Rules {
        match texting_robots::Robot::new(crate::PRODUCT_TOKEN, body) {
            Ok(robot) => Rules::Parsed(robot),
            // Unparseable files impose nothing (RFC 9309 §2.2: ignore invalid lines).
            Err(_) => Rules::AllowAll,
        }
    }

    pub fn allowed(&self, url: &Url) -> bool {
        match self {
            Rules::AllowAll => true,
            Rules::Parsed(robot) => robot.allowed(url.as_str()),
        }
    }
}

#[derive(Default)]
pub struct Cache {
    entries: Mutex<HashMap<String, (Instant, Arc<Rules>)>>,
}

impl Cache {
    pub fn get(&self, origin: &str) -> Option<Arc<Rules>> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(origin)
            .filter(|(at, _)| at.elapsed() < TTL)
            .map(|(_, rules)| Arc::clone(rules))
    }

    pub fn insert(&self, origin: String, rules: Rules) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.len() >= MAX_ORIGINS {
            entries.retain(|_, (at, _)| at.elapsed() < TTL);
            if entries.len() >= MAX_ORIGINS {
                entries.clear();
            }
        }
        entries.insert(origin, (Instant::now(), Arc::new(rules)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(robots: &str, path: &str) -> bool {
        let url = Url::parse("https://example.org")
            .unwrap()
            .join(path)
            .unwrap();
        Rules::parse(robots.as_bytes()).allowed(&url)
    }

    #[test]
    fn our_group_wins_over_the_wildcard() {
        let robots = "User-agent: *\nDisallow: /\n\nUser-agent: read-to-me\nAllow: /\n";
        assert!(allowed(robots, "/article"));
        let robots = "User-agent: *\nAllow: /\n\nUser-agent: read-to-me\nDisallow: /private\n";
        assert!(!allowed(robots, "/private/a"));
        assert!(allowed(robots, "/public"));
    }

    #[test]
    fn the_wildcard_applies_when_we_have_no_group() {
        let robots = "User-agent: GPTBot\nDisallow: /\n\nUser-agent: *\nDisallow: /admin\n";
        assert!(!allowed(robots, "/admin/x"));
        assert!(allowed(robots, "/essay"));
    }

    #[test]
    fn empty_or_garbage_files_allow_everything() {
        assert!(allowed("", "/x"));
        assert!(allowed("\u{0}\u{1}not robots", "/x"));
    }
}
