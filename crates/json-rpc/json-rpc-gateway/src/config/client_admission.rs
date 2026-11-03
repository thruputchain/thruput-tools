use std::{net::IpAddr, str::FromStr};

use serde::{Deserialize, Serialize};

/// An invalid client admission setting.
#[derive(Debug, thiserror::Error)]
#[error("invalid client admission setting: {0}")]
pub struct InvalidClientAdmission(pub &'static str);

/// A nonzero resource bound validated during configuration loading.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(try_from = "usize", into = "usize")]
pub struct ClientLimit<const MAX: usize>(usize);

impl<const MAX: usize> ClientLimit<MAX> {
    /// Return the validated bound.
    pub const fn get(self) -> usize {
        self.0
    }
}

impl<const MAX: usize> TryFrom<usize> for ClientLimit<MAX> {
    type Error = InvalidClientAdmission;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        if (1..=MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidClientAdmission("limit outside supported range"))
        }
    }
}

impl<const MAX: usize> From<ClientLimit<MAX>> for usize {
    fn from(value: ClientLimit<MAX>) -> Self {
        value.0
    }
}

/// A canonical IP network. Host bits must be zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Cidr {
    address: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Whether this network contains the address.
    pub fn contains(self, address: IpAddr) -> bool {
        match (self.address, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                mask(u128::from(u32::from(network)), self.prefix, 32)
                    == mask(u128::from(u32::from(address)), self.prefix, 32)
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                mask(u128::from(network), self.prefix, 128)
                    == mask(u128::from(address), self.prefix, 128)
            }
            // A dual-stack listener can report an IPv4 proxy as mapped IPv6.
            // It must match the same IPv4 trust network as an IPv4 listener.
            (IpAddr::V4(_), IpAddr::V6(address)) => address
                .to_ipv4_mapped()
                .is_some_and(|address| self.contains(IpAddr::V4(address))),
            _ => false,
        }
    }
}

fn mask(value: u128, prefix: u8, bits: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        value >> (bits - prefix) << (bits - prefix)
    }
}

impl FromStr for Cidr {
    type Err = InvalidClientAdmission;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidClientAdmission("expected canonical IPv4 or IPv6 CIDR");
        let (address, prefix) = value.split_once('/').ok_or_else(invalid)?;
        let address: IpAddr = address.parse().map_err(|_| invalid())?;
        let prefix: u8 = prefix.parse().map_err(|_| invalid())?;
        let (number, bits) = match address {
            IpAddr::V4(ip) => (u128::from(u32::from(ip)), 32),
            IpAddr::V6(ip) => (u128::from(ip), 128),
        };
        if prefix > bits || mask(number, prefix, bits) != number {
            return Err(invalid());
        }
        Ok(Self { address, prefix })
    }
}

impl TryFrom<String> for Cidr {
    type Error = InvalidClientAdmission;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Cidr> for String {
    fn from(value: Cidr) -> Self {
        format!("{}/{}", value.address, value.prefix)
    }
}

/// Public RPC client budgets, independent of backend capacity.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, try_from = "RawClientAdmissionConfig")]
pub struct ClientAdmissionConfig {
    /// Maximum active calls across the gateway (1..=1,000,000).
    pub max_in_flight_calls: ClientLimit<1_000_000>,
    /// Maximum active calls on one TCP connection (1..=100,000).
    pub max_in_flight_per_connection: ClientLimit<100_000>,
    /// Tokens replenished per second (1..=1,000,000).
    pub client_rate_per_sec: ClientLimit<1_000_000>,
    /// Token capacity (1..=10,000,000), at least the refill rate.
    pub client_burst: ClientLimit<10_000_000>,
    /// Maximum individually tracked clients (1..=10,000,000).
    pub max_tracked_clients: ClientLimit<10_000_000>,
    /// Proxy networks allowed to supply forwarded client addresses.
    pub trusted_proxies: Vec<Cidr>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawClientAdmissionConfig {
    max_in_flight_calls: ClientLimit<1_000_000>,
    max_in_flight_per_connection: ClientLimit<100_000>,
    client_rate_per_sec: ClientLimit<1_000_000>,
    client_burst: ClientLimit<10_000_000>,
    max_tracked_clients: ClientLimit<10_000_000>,
    trusted_proxies: Vec<Cidr>,
}

impl Default for ClientAdmissionConfig {
    fn default() -> Self {
        Self {
            max_in_flight_calls: ClientLimit(4096),
            max_in_flight_per_connection: ClientLimit(64),
            client_rate_per_sec: ClientLimit(50),
            client_burst: ClientLimit(200),
            max_tracked_clients: ClientLimit(100_000),
            trusted_proxies: ["127.0.0.1/32", "::1/128"]
                .map(|cidr| cidr.parse().expect("bug: valid default proxy CIDR"))
                .into(),
        }
    }
}

impl Default for RawClientAdmissionConfig {
    fn default() -> Self {
        let config = ClientAdmissionConfig::default();
        Self {
            max_in_flight_calls: config.max_in_flight_calls,
            max_in_flight_per_connection: config.max_in_flight_per_connection,
            client_rate_per_sec: config.client_rate_per_sec,
            client_burst: config.client_burst,
            max_tracked_clients: config.max_tracked_clients,
            trusted_proxies: config.trusted_proxies,
        }
    }
}

impl TryFrom<RawClientAdmissionConfig> for ClientAdmissionConfig {
    type Error = InvalidClientAdmission;

    fn try_from(config: RawClientAdmissionConfig) -> Result<Self, Self::Error> {
        if config.client_burst.get() < config.client_rate_per_sec.get() {
            return Err(InvalidClientAdmission(
                "client_burst must be >= client_rate_per_sec",
            ));
        }
        Ok(Self {
            max_in_flight_calls: config.max_in_flight_calls,
            max_in_flight_per_connection: config.max_in_flight_per_connection,
            client_rate_per_sec: config.client_rate_per_sec,
            client_burst: config.client_burst,
            max_tracked_clients: config.max_tracked_clients,
            trusted_proxies: config.trusted_proxies,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_cidr_parsing_and_membership() {
        for (network, inside, outside) in [
            ("192.0.2.0/24", "192.0.2.255", "192.0.3.1"),
            ("192.0.2.0/24", "::ffff:192.0.2.255", "::ffff:192.0.3.1"),
            ("::/0", "2001:db8::1", "192.0.2.1"),
            ("0.0.0.0/0", "192.0.2.1", "::1"),
            ("2001:db8:1:2::/64", "2001:db8:1:2::abcd", "2001:db8:1:3::1"),
            (
                "::ffff:c000:200/120",
                "::ffff:192.0.2.1",
                "::ffff:192.0.3.1",
            ),
            ("::1/128", "::1", "::2"),
        ] {
            let network: Cidr = network.parse().unwrap();
            assert!(network.contains(inside.parse().unwrap()));
            assert!(!network.contains(outside.parse().unwrap()));
        }
        for invalid in [
            "192.0.2.1/24",
            "192.0.2.1/33",
            "::1/64",
            "::/129",
            "::/-1",
            "::",
            "garbage/0",
        ] {
            assert!(invalid.parse::<Cidr>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn defaults_ranges_and_dump_round_trip() {
        let config = ClientAdmissionConfig::default();
        assert_eq!(config.max_in_flight_calls.get(), 4096);
        assert_eq!(config.max_in_flight_per_connection.get(), 64);
        assert_eq!(config.client_rate_per_sec.get(), 50);
        assert_eq!(config.client_burst.get(), 200);
        assert_eq!(config.max_tracked_clients.get(), 100_000);
        for (name, max) in [
            ("max_in_flight_calls", 1_000_000),
            ("max_in_flight_per_connection", 100_000),
            ("client_rate_per_sec", 1_000_000),
            ("client_burst", 10_000_000),
            ("max_tracked_clients", 10_000_000),
        ] {
            for bad in [0, max + 1] {
                assert!(
                    toml::from_str::<ClientAdmissionConfig>(&format!("{name} = {bad}")).is_err()
                );
            }
            for valid in [1, max] {
                let mut config = serde_json::to_value(&config).unwrap();
                config["client_rate_per_sec"] = 1.into();
                config["client_burst"] = 10_000_000.into();
                config[name] = valid.into();
                assert!(serde_json::from_value::<ClientAdmissionConfig>(config).is_ok());
            }
        }
        for invalid in [
            "client_rate_per_sec = 201",
            "trusted_proxies = ['127.0.0.1/8']",
            "unknown = 1",
        ] {
            assert!(toml::from_str::<ClientAdmissionConfig>(invalid).is_err());
        }
        let gateway = super::super::GatewayConfig::default();
        let dump = serde_json::to_value(&gateway).unwrap();
        assert_eq!(
            dump["client_admission"]["trusted_proxies"],
            serde_json::json!(["127.0.0.1/32", "::1/128"])
        );
        let round_trip: super::super::GatewayConfig = serde_json::from_value(dump.clone()).unwrap();
        assert_eq!(serde_json::to_value(round_trip).unwrap(), dump);
    }
}
