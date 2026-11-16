use std::{
    net::{IpAddr, Ipv6Addr, SocketAddr},
    task::{Context, Poll},
};

use axum::http::Request;
use tower::{Layer, Service};

use crate::config::Cidr;

/// Remote TCP endpoint, recorded before HTTP middleware runs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PeerAddr(pub SocketAddr);

/// IPv4 host or IPv6 /64 client identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ClientKey(IpAddr);

impl From<IpAddr> for ClientKey {
    fn from(ip: IpAddr) -> Self {
        Self(match ip {
            IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
                Some(ip) => IpAddr::V4(ip),
                None => IpAddr::V6(Ipv6Addr::from(u128::from(ip) >> 64 << 64)),
            },
            ip @ IpAddr::V4(_) => ip,
        })
    }
}

#[derive(Clone)]
pub(crate) struct ClientIdentityLayer(pub Vec<Cidr>);

impl<S> Layer<S> for ClientIdentityLayer {
    type Service = ClientIdentityService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ClientIdentityService {
            inner,
            trusted: self.0.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ClientIdentityService<S> {
    inner: S,
    trusted: Vec<Cidr>,
}

fn client_key<B>(req: &Request<B>, trusted: &[Cidr]) -> Option<ClientKey> {
    let peer = req.extensions().get::<PeerAddr>()?.0.ip();
    let is_trusted = |ip| trusted.iter().any(|network| network.contains(ip));
    let mut address = peer;
    if is_trusted(peer) {
        // Reject the entire header on any malformed address. Multiple field lines
        // are part of the same ordered forwarded chain.
        let forwarded = req
            .headers()
            .get_all("x-forwarded-for")
            .iter()
            .map(|header| {
                header.to_str().ok().and_then(|value| {
                    value
                        .split(',')
                        .map(|ip| ip.trim().parse::<IpAddr>().ok())
                        .collect::<Option<Vec<_>>>()
                })
            })
            .collect::<Option<Vec<_>>>();
        if let Some(forwarded) = forwarded {
            address = forwarded
                .into_iter()
                .flatten()
                .rev()
                .find(|ip| !is_trusted(*ip))
                .unwrap_or(peer);
        }
    }
    Some(address.into())
}

impl<S, B> Service<Request<B>> for ClientIdentityService<S>
where
    S: Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        if let Some(key) = client_key(&req, &self.trusted) {
            req.extensions_mut().insert(key);
        }
        self.inner.call(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_chain_only_from_trusted_peers() {
        let trusted = vec!["127.0.0.1/32".parse().unwrap()];
        for (peer, header, expected) in [
            ("192.0.2.1:42", Some("198.51.100.1"), "192.0.2.1"),
            (
                "127.0.0.1:42",
                Some("192.0.2.1, 198.51.100.1"),
                "198.51.100.1",
            ),
            ("127.0.0.1:42", Some("192.0.2.1, 127.0.0.1"), "192.0.2.1"),
            (
                "[::ffff:127.0.0.1]:42",
                Some("192.0.2.1, ::ffff:127.0.0.1"),
                "192.0.2.1",
            ),
            ("127.0.0.1:42", Some("127.0.0.1, 127.0.0.1"), "127.0.0.1"),
            ("127.0.0.1:42", Some("garbage, 192.0.2.1"), "127.0.0.1"),
            ("127.0.0.1:42", None, "127.0.0.1"),
        ] {
            let mut req = Request::new(());
            req.extensions_mut().insert(PeerAddr(peer.parse().unwrap()));
            if let Some(header) = header {
                req.headers_mut()
                    .insert("x-forwarded-for", header.parse().unwrap());
            }
            assert_eq!(
                client_key(&req, &trusted),
                Some(ClientKey::from(expected.parse::<IpAddr>().unwrap()))
            );
        }
    }

    #[test]
    fn ipv6_uses_64_bits_and_mapped_ipv4_is_ipv4() {
        assert_eq!(
            ClientKey::from("2001:db8:1:2::abcd".parse::<IpAddr>().unwrap()),
            ClientKey::from("2001:db8:1:2::1".parse::<IpAddr>().unwrap())
        );
        assert_ne!(
            ClientKey::from("2001:db8:1:2::1".parse::<IpAddr>().unwrap()),
            ClientKey::from("2001:db8:1:3::1".parse::<IpAddr>().unwrap())
        );
        assert_eq!(
            ClientKey::from("::ffff:192.0.2.1".parse::<IpAddr>().unwrap()),
            ClientKey::from("192.0.2.1".parse::<IpAddr>().unwrap())
        );
    }
}
