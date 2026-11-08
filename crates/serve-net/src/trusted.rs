use std::net::IpAddr;

use ipnet::IpNet;

use crate::NetError;

/// The peers whose PROXY headers are believed. The default trusts nobody.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<IpNet>);

impl TrustedProxies {
    /// Reads entries that are IP addresses or CIDR ranges. An entry that is neither is
    /// [`NetError::InvalidProxy`]; nothing is trusted then.
    pub fn parse<S: AsRef<str>>(entries: &[S]) -> Result<Self, NetError> {
        entries
            .iter()
            .map(|entry| {
                let entry = entry.as_ref().trim();
                entry
                    .parse::<IpNet>()
                    .or_else(|_| entry.parse::<IpAddr>().map(IpNet::from))
                    .map_err(|_| NetError::InvalidProxy(entry.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    /// Whether `ip` is trusted. An IPv4-mapped IPv6 address counts as its IPv4 address.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.0.iter().any(|net| net.contains(&ip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn reads_ranges_and_single_addresses() {
        let trusted =
            TrustedProxies::parse(&["10.0.0.0/8", "198.51.100.7", " 2001:db8::/32 "]).unwrap();
        assert!(trusted.contains(ip("10.1.2.3")));
        assert!(trusted.contains(ip("198.51.100.7")));
        assert!(trusted.contains(ip("2001:db8::1")));
        assert!(!trusted.contains(ip("11.0.0.1")));
        assert!(!trusted.contains(ip("198.51.100.8")));
        assert!(!trusted.contains(ip("2001:db9::1")));
    }

    #[test]
    fn a_mapped_address_counts_as_ipv4() {
        let trusted = TrustedProxies::parse(&["10.0.0.0/8"]).unwrap();
        assert!(trusted.contains(ip("::ffff:10.0.0.1")));
    }

    #[test]
    fn nobody_is_trusted_by_default() {
        assert!(!TrustedProxies::default().contains(ip("127.0.0.1")));
    }

    #[test]
    fn refuses_nonsense() {
        assert!(matches!(
            TrustedProxies::parse(&["10.0.0.0/8", "proxy.example.com"]),
            Err(NetError::InvalidProxy(entry)) if entry == "proxy.example.com"
        ));
        assert!(TrustedProxies::parse(&["10.0.0.0/33"]).is_err());
    }
}
