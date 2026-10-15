// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fetch settings from the environment (FR-FET-08, ADR-0005).

use ipnet::IpNet;
use rtm_fetch::{Config, Policy};

/// Fetch settings, read through `var` so tests needn't touch the process environment.
#[derive(Debug)]
pub struct FetchSettings {
    pub config: Config,
    pub workers: usize,
    /// Ranges re-opened with RTM_FETCH_UNSAFE_ALLOW, for the startup warning.
    pub unsafe_allow: Vec<IpNet>,
}

pub const DEFAULT_WORKERS: usize = 2;
const MAX_WORKERS: usize = 32;

fn flag(var: &impl Fn(&str) -> Option<String>, name: &str) -> Result<bool, String> {
    match var(name).as_deref().map(str::trim) {
        None | Some("") | Some("false") | Some("0") => Ok(false),
        Some("true") | Some("1") => Ok(true),
        Some(other) => Err(format!("{name}={other:?}: expected true or false")),
    }
}

fn list<T: std::str::FromStr>(
    var: &impl Fn(&str) -> Option<String>,
    name: &str,
) -> Result<Vec<T>, String> {
    var(name)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().map_err(|_| format!("{name}: {s:?} is not valid")))
        .collect()
}

/// Read the fetch settings. `ignore_robots_flag` is the `--ignore-robots` argument,
/// equivalent to RTM_FETCH_IGNORE_ROBOTS=true.
pub fn fetch_settings(
    var: impl Fn(&str) -> Option<String>,
    ignore_robots_flag: bool,
) -> Result<FetchSettings, String> {
    let ignore_robots = ignore_robots_flag || flag(&var, "RTM_FETCH_IGNORE_ROBOTS")?;
    let deny: Vec<IpNet> = list(&var, "RTM_FETCH_DENY")?;
    let unsafe_allow: Vec<IpNet> = list(&var, "RTM_FETCH_UNSAFE_ALLOW")?;
    let ports: Vec<u16> = list(&var, "RTM_FETCH_ALLOW_PORTS")?;
    let workers = match var("RTM_FETCH_WORKERS") {
        None => DEFAULT_WORKERS,
        Some(n) => n
            .trim()
            .parse()
            .ok()
            .filter(|n| (1..=MAX_WORKERS).contains(n))
            .ok_or_else(|| format!("RTM_FETCH_WORKERS={n:?}: expected 1 to {MAX_WORKERS}"))?,
    };
    let policy = Policy::default()
        .deny(deny)
        .unsafe_allow(unsafe_allow.clone())
        .allow_ports(ports);
    Ok(FetchSettings {
        config: Config {
            policy,
            ignore_robots,
            ..Config::default()
        },
        workers,
        unsafe_allow,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn settings(vars: &[(&str, &str)], flag: bool) -> Result<FetchSettings, String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        fetch_settings(|k| vars.get(k).cloned(), flag)
    }

    #[test]
    fn defaults_respect_robots() {
        let s = settings(&[], false).unwrap();
        assert!(!s.config.ignore_robots);
        assert_eq!(s.workers, DEFAULT_WORKERS);
        assert!(s.unsafe_allow.is_empty());
    }

    #[test]
    fn disrespectful_mode_by_flag_or_environment() {
        assert!(settings(&[], true).unwrap().config.ignore_robots);
        assert!(
            settings(&[("RTM_FETCH_IGNORE_ROBOTS", "true")], false)
                .unwrap()
                .config
                .ignore_robots
        );
        assert!(
            !settings(&[("RTM_FETCH_IGNORE_ROBOTS", "false")], false)
                .unwrap()
                .config
                .ignore_robots
        );
        // A typo must not silently leave robots.txt on or off.
        assert!(settings(&[("RTM_FETCH_IGNORE_ROBOTS", "yes please")], false).is_err());
    }

    #[test]
    fn ranges_and_ports_are_parsed_strictly() {
        let s = settings(
            &[
                ("RTM_FETCH_UNSAFE_ALLOW", "10.1.0.0/16, fd00::/8"),
                ("RTM_FETCH_DENY", "1.1.1.0/24"),
                ("RTM_FETCH_ALLOW_PORTS", "8080"),
                ("RTM_FETCH_WORKERS", "4"),
            ],
            false,
        )
        .unwrap();
        assert_eq!(s.unsafe_allow.len(), 2);
        assert_eq!(s.workers, 4);
        let p = &s.config.policy;
        assert!(p.check_ip("10.1.2.3".parse().unwrap()).is_ok());
        assert!(p.check_ip("1.1.1.1".parse().unwrap()).is_err());
        for bad in [
            ("RTM_FETCH_UNSAFE_ALLOW", "10.1.0.0"),
            ("RTM_FETCH_DENY", "nonsense"),
            ("RTM_FETCH_ALLOW_PORTS", "99999"),
            ("RTM_FETCH_WORKERS", "0"),
            ("RTM_FETCH_WORKERS", "many"),
        ] {
            assert!(settings(&[bad], false).is_err(), "{bad:?}");
        }
    }
}
