//! SSRF protection for webhook targets.
//!
//! Webhook URLs must be `http(s)` and resolve only to public addresses:
//! loopback, private (RFC 1918 / ULA), link-local (incl. cloud metadata
//! `169.254.169.254`), CGNAT, multicast, unspecified and documentation
//! ranges are refused unless the host/IP/CIDR is allow-listed by the admin
//! (`BGH_WEBHOOK_ALLOWED_HOSTS` and the `webhooks.allowed_hosts` site
//! setting: a JSON array of the same entries; `*` allows everything).
//!
//! Delivery resolves the host once, checks every address, and pins the
//! HTTP client to the checked addresses (no DNS-rebinding window).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use bgh_core::AppState;

/// Message GitHub uses for unreachable webhook targets.
pub const NOT_PUBLIC: &str =
    "url is not supported because it isn't reachable over the public Internet";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Any,
    Host(String),
    Net(IpAddr, u8),
}

/// Admin allow-list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    entries: Vec<Entry>,
}

impl Policy {
    pub fn new<S: AsRef<str>>(entries: impl IntoIterator<Item = S>) -> Self {
        let entries = entries
            .into_iter()
            .filter_map(|e| parse_entry(e.as_ref().trim()))
            .collect();
        Self { entries }
    }

    /// Allow-list from config plus the `webhooks.allowed_hosts` site setting.
    pub async fn load(state: &AppState) -> Self {
        let mut list = state.config.webhook_allowed_hosts.clone();
        let extra: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT value FROM site_settings WHERE key = 'webhooks.allowed_hosts'",
        )
        .fetch_optional(&state.db)
        .await
        .unwrap_or_default();
        if let Some(serde_json::Value::Array(items)) = extra {
            list.extend(
                items
                    .into_iter()
                    .filter_map(|v| v.as_str().map(String::from)),
            );
        }
        Self::new(list)
    }

    fn host_allowed(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.entries.iter().any(|e| match e {
            Entry::Any => true,
            Entry::Host(h) => match h.strip_prefix("*.") {
                Some(suffix) => host.ends_with(&format!(".{suffix}")),
                None => *h == host,
            },
            Entry::Net(..) => false,
        })
    }

    fn ip_allowed(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        self.entries.iter().any(|e| match e {
            Entry::Any => true,
            Entry::Net(net, len) => in_net(ip, *net, *len),
            Entry::Host(_) => false,
        })
    }

    /// Whether `ip` may be contacted for `host`.
    pub fn permits(&self, host: &str, ip: IpAddr) -> bool {
        !is_internal(ip) || self.host_allowed(host) || self.ip_allowed(ip)
    }
}

fn parse_entry(s: &str) -> Option<Entry> {
    if s.is_empty() {
        return None;
    }
    if s == "*" {
        return Some(Entry::Any);
    }
    if let Some((addr, len)) = s.split_once('/') {
        let ip: IpAddr = addr.parse().ok()?;
        let len: u8 = len.parse().ok()?;
        let max = if ip.is_ipv4() { 32 } else { 128 };
        return (len <= max).then_some(Entry::Net(canonical(ip), len));
    }
    if let Ok(ip) = s.trim_matches(['[', ']']).parse::<IpAddr>() {
        let ip = canonical(ip);
        return Some(Entry::Net(ip, if ip.is_ipv4() { 32 } else { 128 }));
    }
    Some(Entry::Host(s.trim_end_matches('.').to_ascii_lowercase()))
}

/// IPv4-mapped IPv6 → IPv4.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => ip,
        },
        v4 => v4,
    }
}

fn in_net(ip: IpAddr, net: IpAddr, len: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let mask = if len == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(len))
            };
            u32::from(a) & mask == u32::from(b) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let mask = if len == 0 {
                0
            } else {
                u128::MAX << (128 - u32::from(len))
            };
            u128::from(a) & mask == u128::from(b) & mask
        }
        _ => false,
    }
}

/// Addresses that are not on the public Internet.
pub fn is_internal(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(v4) => is_internal_v4(v4),
        IpAddr::V6(v6) => is_internal_v6(v6),
    }
}

fn is_internal_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18.0.0/15 benchmarking
        || o[0] >= 240 // reserved
}

fn is_internal_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
        || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link local
        || (s[0] & 0xffc0) == 0xfec0 // fec0::/10 site local (deprecated)
        || (s[0] == 0x64 && s[1] == 0xff9b) // 64:ff9b::/96 NAT64 (may reach v4 internals)
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || ip.to_ipv4().is_some_and(|v4| s[0..6] == [0; 6] && is_internal_v4(v4)) // ::a.b.c.d
}

/// Parsed and checked target.
#[derive(Debug, Clone)]
pub struct Target {
    pub url: url::Url,
    pub host: String,
    /// Checked addresses to connect to.
    pub addrs: Vec<SocketAddr>,
}

/// Syntactic validation used when creating/updating a hook: scheme, host,
/// and (for literal IPs / `localhost`) the address policy.
pub fn validate_url(policy: &Policy, raw: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(raw.trim()).map_err(|_| "url is not a valid URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("url must use http or https".into());
    }
    let Some(host) = url.host() else {
        return Err("url must have a host".into());
    };
    let literal = match host {
        url::Host::Ipv4(v4) => Some(IpAddr::V4(v4)),
        url::Host::Ipv6(v6) => Some(IpAddr::V6(v6)),
        url::Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            (d == "localhost" || d.ends_with(".localhost"))
                .then_some(IpAddr::V4(Ipv4Addr::LOCALHOST))
        }
    };
    if let Some(ip) = literal
        && !policy.permits(url.host_str().unwrap_or_default(), ip)
    {
        return Err(format!(
            "{NOT_PUBLIC} ({})",
            url.host_str().unwrap_or_default()
        ));
    }
    Ok(url)
}

/// Resolve and check a URL right before delivery.
pub async fn resolve(policy: &Policy, raw: &str) -> Result<Target, String> {
    let url = validate_url(policy, raw)?;
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_matches(['[', ']'])
        .to_string();
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|e| format!("failed to resolve {host}: {e}"))?
            .collect(),
    };
    if addrs.is_empty() {
        return Err(format!("failed to resolve {host}"));
    }
    if let Some(bad) = addrs.iter().find(|a| !policy.permits(&host, a.ip())) {
        return Err(format!("{NOT_PUBLIC} ({host} resolves to {})", bad.ip()));
    }
    Ok(Target { url, host, addrs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn classifies_addresses() {
        for internal in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_internal(ip(internal)), "{internal}");
        }
        for public in [
            "8.8.8.8",
            "140.82.112.3",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_internal(ip(public)), "{public}");
        }
    }

    #[test]
    fn allow_list() {
        let p = Policy::new(["127.0.0.1", "10.0.0.0/8", "hooks.internal", "*.corp"]);
        assert!(p.permits("x", ip("127.0.0.1")));
        assert!(p.permits("x", ip("10.9.9.9")));
        assert!(!p.permits("x", ip("192.168.1.1")));
        assert!(p.permits("hooks.internal", ip("192.168.1.1")));
        assert!(p.permits("ci.corp", ip("192.168.1.1")));
        assert!(!p.permits("corpx", ip("192.168.1.1")));
        assert!(p.permits("anything", ip("8.8.8.8")));
        assert!(Policy::new(["*"]).permits("x", ip("127.0.0.1")));
        assert!(!Policy::default().permits("x", ip("::1")));
    }

    #[test]
    fn validates_urls() {
        let p = Policy::default();
        assert!(validate_url(&p, "https://example.com/hook").is_ok());
        assert!(validate_url(&p, "ftp://example.com/").is_err());
        assert!(validate_url(&p, "not a url").is_err());
        let err = validate_url(&p, "http://127.0.0.1:8080/x").unwrap_err();
        assert!(err.starts_with(NOT_PUBLIC), "{err}");
        assert!(validate_url(&p, "http://localhost/x").is_err());
        assert!(validate_url(&p, "http://[::1]/x").is_err());
        assert!(validate_url(&Policy::new(["127.0.0.1"]), "http://127.0.0.1:8080/x").is_ok());
    }

    #[tokio::test]
    async fn resolve_checks_literal_ips() {
        assert!(
            resolve(&Policy::default(), "http://169.254.169.254/latest")
                .await
                .is_err()
        );
        let t = resolve(&Policy::new(["127.0.0.1"]), "http://127.0.0.1:9/x")
            .await
            .unwrap();
        assert_eq!(t.addrs, vec!["127.0.0.1:9".parse().unwrap()]);
    }
}
