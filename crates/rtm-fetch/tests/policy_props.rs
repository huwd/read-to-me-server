// SPDX-License-Identifier: AGPL-3.0-or-later
//! Property tests over IP encodings and range edges (ADR-0005 "Testing").

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use proptest::prelude::*;
use rtm_fetch::Policy;
use url::Url;

fn allowed(ip: IpAddr) -> bool {
    Policy::default().check_ip(ip).is_ok()
}

fn nat64(v4: Ipv4Addr) -> Ipv6Addr {
    let [a, b, c, d] = v4.octets();
    Ipv6Addr::new(
        0x64,
        0xff9b,
        0,
        0,
        0,
        0,
        u16::from_be_bytes([a, b]),
        u16::from_be_bytes([c, d]),
    )
}

/// Ranges that must always be refused, with their first and last addresses.
const MUST_DENY_V4: &[(u32, u8)] = &[
    (0x0000_0000, 8),  // 0.0.0.0/8
    (0x0a00_0000, 8),  // 10/8
    (0x6440_0000, 10), // 100.64/10
    (0x7f00_0000, 8),  // 127/8
    (0xa9fe_0000, 16), // 169.254/16
    (0xac10_0000, 12), // 172.16/12
    (0xc0a8_0000, 16), // 192.168/16
    (0xe000_0000, 4),  // 224/4
    (0xf000_0000, 4),  // 240/4
];

proptest! {
    /// IPv4-mapped and NAT64 spellings are judged exactly like the IPv4 address.
    #[test]
    fn embedded_ipv4_is_judged_as_ipv4(bits in any::<u32>()) {
        let v4 = Ipv4Addr::from(bits);
        let verdict = allowed(IpAddr::V4(v4));
        prop_assert_eq!(allowed(IpAddr::V6(v4.to_ipv6_mapped())), verdict);
        prop_assert_eq!(allowed(IpAddr::V6(nat64(v4))), verdict);
    }

    /// Anything inside a must-deny range is refused, however it is spelled in a URL.
    #[test]
    fn denied_ranges_hold_for_every_address(range in 0..MUST_DENY_V4.len(), offset in any::<u32>()) {
        let (base, len) = MUST_DENY_V4[range];
        let host_bits = 32 - u32::from(len);
        let ip = base | (offset & ((1u64 << host_bits) - 1) as u32);
        let v4 = Ipv4Addr::from(ip);
        prop_assert!(!allowed(IpAddr::V4(v4)), "{}", v4);

        let policy = Policy::default();
        for spelling in [
            format!("http://{v4}/"),
            format!("http://{ip}/"),             // decimal
            format!("http://0x{ip:x}/"),         // hex
            format!("http://0{ip:o}/"),          // octal
            format!("http://[::ffff:{v4}]/"),    // mapped
        ] {
            let url = Url::parse(&spelling).unwrap();
            prop_assert!(policy.check_url(&url).is_err(), "{} allowed", spelling);
        }
    }

    /// IPv6 outside global unicast (`2000::/3`) is refused, unless it embeds an IPv4
    /// address (judged above).
    #[test]
    fn only_global_unicast_ipv6_is_considered(bits in any::<u128>()) {
        let v6 = Ipv6Addr::from(bits);
        let in_2000_3 = (v6.segments()[0] & 0xe000) == 0x2000;
        let embeds_v4 = v6.segments()[..6] == [0, 0, 0, 0, 0, 0xffff]
            || v6.segments()[..6] == [0x64, 0xff9b, 0, 0, 0, 0];
        if !in_2000_3 && !embeds_v4 {
            prop_assert!(!allowed(IpAddr::V6(v6)), "{}", v6);
        }
    }
}

/// The addresses either side of each range boundary.
#[test]
fn range_edges() {
    for (outside, inside) in [
        ("9.255.255.255", "10.0.0.0"),
        ("11.0.0.0", "10.255.255.255"),
        ("100.63.255.255", "100.64.0.0"),
        ("100.128.0.0", "100.127.255.255"),
        ("172.15.255.255", "172.16.0.0"),
        ("172.32.0.0", "172.31.255.255"),
        ("192.167.255.255", "192.168.0.0"),
        ("192.169.0.0", "192.168.255.255"),
        ("169.253.255.255", "169.254.0.0"),
        ("223.255.255.255", "224.0.0.0"),
    ] {
        assert!(
            allowed(outside.parse().unwrap()),
            "{outside} should be allowed"
        );
        assert!(
            !allowed(inside.parse().unwrap()),
            "{inside} should be denied"
        );
    }
}
