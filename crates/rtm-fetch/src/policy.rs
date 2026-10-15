// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which destinations may be fetched (ADR-0005 steps 1 and 3).
//!
//! Addresses are checked against the IANA special-purpose registries rather than with
//! `IpAddr::is_global()`, which is unstable and doesn't cover every range we care about.
//! IPv6 is allow-listed: only global unicast (`2000::/3`) is considered, minus its
//! special-purpose blocks. Addresses that embed an IPv4 address are judged by it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use url::{Host, Url};

/// Why a destination was refused. Logged and counted; never shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    Scheme,
    Credentials,
    TooLong,
    Port,
    NoHost,
    /// A special-purpose address: loopback, private, link-local, metadata and so on.
    Address,
    /// Matched an operator-configured deny range.
    Configured,
}

impl DenyReason {
    /// A stable label for logs and metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            DenyReason::Scheme => "scheme",
            DenyReason::Credentials => "credentials",
            DenyReason::TooLong => "too_long",
            DenyReason::Port => "port",
            DenyReason::NoHost => "no_host",
            DenyReason::Address => "address",
            DenyReason::Configured => "configured",
        }
    }
}

/// IPv4 special-purpose ranges (IANA registry), plus multicast, reserved and broadcast.
const DENY_V4: &[&str] = &[
    "0.0.0.0/8",       // "this network"
    "10.0.0.0/8",      // private
    "100.64.0.0/10",   // carrier-grade NAT
    "127.0.0.0/8",     // loopback
    "169.254.0.0/16",  // link-local, including cloud metadata 169.254.169.254
    "172.16.0.0/12",   // private
    "192.0.0.0/24",    // IETF protocol assignments
    "192.0.2.0/24",    // documentation
    "192.31.196.0/24", // AS112
    "192.52.193.0/24", // AMT
    "192.88.99.0/24",  // deprecated 6to4 relay anycast
    "192.168.0.0/16",  // private
    "192.175.48.0/24", // AS112 direct delegation
    "198.18.0.0/15",   // benchmarking
    "198.51.100.0/24", // documentation
    "203.0.113.0/24",  // documentation
    "224.0.0.0/4",     // multicast
    "240.0.0.0/4",     // reserved, including broadcast
];

/// Special-purpose blocks inside IPv6 global unicast (`2000::/3`).
const DENY_V6: &[&str] = &[
    "2001::/23",     // IETF protocol assignments: Teredo, ORCHID, benchmarking, ...
    "2001:db8::/32", // documentation
    "2002::/16",     // 6to4: embeds an IPv4 address and relays through it
    "3fff::/20",     // documentation
];

fn parse_nets<T: std::str::FromStr>(nets: &[&str]) -> Vec<T>
where
    T::Err: std::fmt::Debug,
{
    nets.iter()
        .map(|n| n.parse().expect("valid built-in CIDR"))
        .collect()
}

/// The IPv4 address an IPv6 address stands for, if it embeds one that is what would
/// actually be reached: IPv4-mapped (`::ffff:a.b.c.d`) and NAT64 (`64:ff9b::/96`).
/// IPv4-compatible addresses (`::a.b.c.d`) are deprecated and fall outside global
/// unicast, so they are denied outright.
fn embedded_v4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = v6.segments();
    let tail = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
    let prefix_is = |p: [u16; 6]| s[..6] == p;
    if prefix_is([0, 0, 0, 0, 0, 0xffff]) || prefix_is([0x64, 0xff9b, 0, 0, 0, 0]) {
        Some(tail)
    } else {
        None
    }
}

/// The destination policy: the built-in deny-list plus operator configuration.
#[derive(Debug, Clone)]
pub struct Policy {
    deny_v4: Vec<Ipv4Net>,
    deny_v6: Vec<Ipv6Net>,
    extra_deny: Vec<IpNet>,
    unsafe_allow: Vec<IpNet>,
    ports: Vec<u16>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            deny_v4: parse_nets(DENY_V4),
            deny_v6: parse_nets(DENY_V6),
            extra_deny: Vec::new(),
            unsafe_allow: Vec::new(),
            ports: vec![80, 443],
        }
    }
}

impl Policy {
    /// Also refuse these ranges.
    pub fn deny(mut self, nets: impl IntoIterator<Item = IpNet>) -> Self {
        self.extra_deny.extend(nets);
        self
    }

    /// Permit these ranges even though the built-in list refuses them, for example an
    /// intranet. This re-opens the server to SSRF against them; hence the name.
    pub fn unsafe_allow(mut self, nets: impl IntoIterator<Item = IpNet>) -> Self {
        self.unsafe_allow.extend(nets);
        self
    }

    /// Permit these ports in addition to 80 and 443.
    pub fn allow_ports(mut self, ports: impl IntoIterator<Item = u16>) -> Self {
        self.ports.extend(ports);
        self
    }

    /// Whether `ip` may be connected to.
    ///
    /// An address that reaches an IPv4 destination is judged in every spelling (IPv4,
    /// mapped, NAT64): a configured deny of any spelling always wins over an allow.
    pub fn check_ip(&self, ip: IpAddr) -> Result<(), DenyReason> {
        let reached = match ip {
            IpAddr::V6(v6) => embedded_v4(v6),
            IpAddr::V4(v4) => Some(v4),
        };
        // Every way of writing the destination, so configured ranges match whichever
        // spelling the operator used.
        let spellings = || {
            std::iter::once(ip).chain(reached.into_iter().flat_map(|v4| {
                let [a, b, c, d] = v4.octets();
                let nat64 = Ipv6Addr::new(
                    0x64,
                    0xff9b,
                    0,
                    0,
                    0,
                    0,
                    u16::from_be_bytes([a, b]),
                    u16::from_be_bytes([c, d]),
                );
                [
                    IpAddr::V4(v4),
                    IpAddr::V6(v4.to_ipv6_mapped()),
                    IpAddr::V6(nat64),
                ]
            }))
        };
        if spellings().any(|a| self.extra_deny.iter().any(|n| n.contains(&a))) {
            return Err(DenyReason::Configured);
        }
        if spellings().any(|a| self.unsafe_allow.iter().any(|n| n.contains(&a))) {
            return Ok(());
        }
        match (ip, reached) {
            // Judge by what would really be reached.
            (_, Some(v4)) => self.check_v4(v4),
            (IpAddr::V6(v6), None) => self.check_v6(v6),
            (IpAddr::V4(_), None) => unreachable!("an IPv4 address reaches itself"),
        }
    }

    fn check_v4(&self, v4: Ipv4Addr) -> Result<(), DenyReason> {
        if self.deny_v4.iter().any(|n| n.contains(&v4)) {
            Err(DenyReason::Address)
        } else {
            Ok(())
        }
    }

    fn check_v6(&self, v6: Ipv6Addr) -> Result<(), DenyReason> {
        let global_unicast = (v6.segments()[0] & 0xe000) == 0x2000;
        if !global_unicast || self.deny_v6.iter().any(|n| n.contains(&v6)) {
            Err(DenyReason::Address)
        } else {
            Ok(())
        }
    }

    /// Check a URL before any network activity: scheme, credentials, length, port, and
    /// the host if it's an IP literal. Hostnames are checked when resolved.
    pub fn check_url(&self, url: &Url) -> Result<(), DenyReason> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(DenyReason::Scheme);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(DenyReason::Credentials);
        }
        if url.as_str().len() > crate::MAX_URL_LEN {
            return Err(DenyReason::TooLong);
        }
        let port = url.port_or_known_default().ok_or(DenyReason::Port)?;
        if !self.ports.contains(&port) {
            return Err(DenyReason::Port);
        }
        match url.host() {
            Some(Host::Ipv4(v4)) => self.check_ip(IpAddr::V4(v4)),
            Some(Host::Ipv6(v6)) => self.check_ip(IpAddr::V6(v6)),
            Some(Host::Domain(d)) if !d.is_empty() => Ok(()),
            _ => Err(DenyReason::NoHost),
        }
    }

    /// Check every resolved address: if any is refused, the destination is refused.
    /// Trying the next address instead would let an attacker mix a public decoy with a
    /// private target.
    pub fn check_resolved(&self, addrs: &[IpAddr]) -> Result<(), DenyReason> {
        if addrs.is_empty() {
            return Err(DenyReason::NoHost);
        }
        addrs.iter().try_for_each(|ip| self.check_ip(*ip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn denied(s: &str) -> bool {
        Policy::default().check_ip(s.parse().unwrap()).is_err()
    }

    #[test]
    fn special_purpose_addresses_are_denied() {
        for ip in [
            "0.0.0.0",
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.5",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "100.64.0.1",
            "169.254.169.254",
            "192.0.0.170",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "::8.8.8.8",
            "64:ff9b::10.0.0.1",
            "64:ff9b:1::1",
            "fc00::1",
            "fd00:ec2::254",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "100::1",
            "2001::1",
            "2001:db8::1",
            "2002:7f00:1::1",
            "3fff::1",
        ] {
            assert!(denied(ip), "{ip} should be denied");
        }
    }

    #[test]
    fn public_addresses_are_allowed() {
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.215.14",
            "172.15.255.255",
            "172.32.0.1",
            "100.128.0.1",
            "::ffff:1.1.1.1",
            "64:ff9b::808:808",
            "2606:4700:4700::1111",
            "2a00:1450:4009:81f::200e",
        ] {
            assert!(!denied(ip), "{ip} should be allowed");
        }
    }

    #[test]
    fn configuration_adds_and_removes_ranges() {
        let p = Policy::default()
            .deny(["1.1.1.0/24".parse().unwrap()])
            .unsafe_allow(["10.1.0.0/16".parse().unwrap()]);
        assert_eq!(
            p.check_ip("1.1.1.1".parse().unwrap()),
            Err(DenyReason::Configured)
        );
        assert_eq!(
            p.check_ip("::ffff:1.1.1.1".parse().unwrap()),
            Err(DenyReason::Configured)
        );
        assert!(p.check_ip("10.1.2.3".parse().unwrap()).is_ok());
        assert!(p.check_ip("::ffff:10.1.2.3".parse().unwrap()).is_ok());
        assert!(p.check_ip("10.2.0.1".parse().unwrap()).is_err());
    }

    /// A deny always beats an allow, whichever spelling (IPv4, mapped, NAT64) each uses.
    #[test]
    fn denies_win_across_spellings() {
        let deny_v4 = Policy::default()
            .deny(["10.1.2.3/32".parse().unwrap()])
            .unsafe_allow([
                "::ffff:10.1.0.0/112".parse().unwrap(),
                "64:ff9b::a01:0/112".parse().unwrap(),
                "10.1.0.0/16".parse().unwrap(),
            ]);
        for ip in ["10.1.2.3", "::ffff:10.1.2.3", "64:ff9b::a01:203"] {
            assert_eq!(
                deny_v4.check_ip(ip.parse().unwrap()),
                Err(DenyReason::Configured),
                "{ip}"
            );
        }
        for ip in ["10.1.2.4", "::ffff:10.1.2.4", "64:ff9b::a01:204"] {
            assert!(deny_v4.check_ip(ip.parse().unwrap()).is_ok(), "{ip}");
        }

        let deny_v6 = Policy::default()
            .deny(["::ffff:10.1.2.3/128".parse().unwrap()])
            .unsafe_allow(["10.1.0.0/16".parse().unwrap()]);
        for ip in ["10.1.2.3", "::ffff:10.1.2.3", "64:ff9b::a01:203"] {
            assert_eq!(
                deny_v6.check_ip(ip.parse().unwrap()),
                Err(DenyReason::Configured),
                "{ip}"
            );
        }
    }

    #[test]
    fn urls_are_checked_before_any_lookup() {
        let p = Policy::default();
        let check = |u: &str| p.check_url(&Url::parse(u).unwrap());
        assert_eq!(check("file:///etc/passwd"), Err(DenyReason::Scheme));
        assert_eq!(check("gopher://example.com/"), Err(DenyReason::Scheme));
        assert_eq!(
            check("https://u:p@example.com/"),
            Err(DenyReason::Credentials)
        );
        assert_eq!(check("https://example.com:8443/"), Err(DenyReason::Port));
        assert_eq!(check("http://127.0.0.1/"), Err(DenyReason::Address));
        assert_eq!(check("http://[::1]/"), Err(DenyReason::Address));
        assert_eq!(
            check("http://169.254.169.254/latest/meta-data/"),
            Err(DenyReason::Address)
        );
        // Alternative IPv4 spellings are normalised by the WHATWG parser.
        assert_eq!(check("http://0x7f000001/"), Err(DenyReason::Address));
        assert_eq!(check("http://017700000001/"), Err(DenyReason::Address));
        assert_eq!(check("http://2130706433/"), Err(DenyReason::Address));
        assert_eq!(check("http://127.1/"), Err(DenyReason::Address));
        assert_eq!(check("http://[::ffff:7f00:1]/"), Err(DenyReason::Address));
        assert!(check("https://example.com/a").is_ok());
        assert!(check("http://1.1.1.1/").is_ok());
        assert!(
            Policy::default()
                .allow_ports([8443])
                .check_url(&Url::parse("https://example.com:8443/").unwrap())
                .is_ok()
        );
    }

    #[test]
    fn one_bad_record_refuses_the_lot() {
        let p = Policy::default();
        let addrs = ["1.1.1.1".parse().unwrap(), "10.0.0.1".parse().unwrap()];
        assert_eq!(p.check_resolved(&addrs), Err(DenyReason::Address));
        assert_eq!(p.check_resolved(&[]), Err(DenyReason::NoHost));
    }
}
