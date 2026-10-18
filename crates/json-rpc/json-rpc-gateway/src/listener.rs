//! RPC listener setup for bursts of new client connections.

use std::{io, net::SocketAddr};
use tokio::net::{TcpSocket, ToSocketAddrs};

// Tokio's default listener inherits mio's backlog of 128. At high submission
// concurrency, a burst can fill it before the accept task runs again. The OS
// still caps this request at its configured maximum listen backlog.
const ACCEPT_BACKLOG: u32 = 4096;

#[tracing::instrument(skip_all)]
pub(crate) async fn bind(address: impl ToSocketAddrs) -> io::Result<std::net::TcpListener> {
    let mut last_error = None;
    // Preserve TcpListener::bind's hostname resolution and address fallback.
    for address in tokio::net::lookup_host(address).await? {
        match bind_address(address) {
            Ok(listener) => return Ok(listener),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "no socket addresses resolved")
    }))
}

fn bind_address(address: SocketAddr) -> io::Result<std::net::TcpListener> {
    let socket = if address.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    // Match Tokio/mio: Unix can rebind after shutdown; on Windows this option
    // would also allow another process to bind an actively used address.
    #[cfg(not(windows))]
    socket.set_reuseaddr(true)?;
    socket.bind(address)?;
    socket.listen(ACCEPT_BACKLOG)?.into_std()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::{TcpListener, TcpStream};

    #[tokio::test]
    async fn occupied_address_falls_back_to_the_next_address() {
        let occupied = bind("127.0.0.1:0").await.unwrap();
        let addresses = [
            occupied.local_addr().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        ];
        let listener = bind(&addresses[..]).await.unwrap();
        assert_ne!(listener.local_addr().unwrap(), addresses[0]);
        // Conversion back to Tokio also verifies the listener is nonblocking.
        let listener = TcpListener::from_std(listener).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap());
        let (client, accepted) = tokio::join!(client, listener.accept());
        assert!(client.is_ok());
        assert!(accepted.is_ok());
    }

    #[tokio::test]
    async fn empty_addresses_and_occupied_addresses_return_errors() {
        let empty: &[SocketAddr] = &[];
        assert_eq!(
            bind(empty).await.unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let occupied = bind("127.0.0.1:0").await.unwrap();
        assert_eq!(
            bind(occupied.local_addr().unwrap())
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::AddrInUse
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_connection_burst_can_queue_before_the_accept_task_runs() {
        let listener = bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut connections = tokio::task::JoinSet::new();
        for _ in 0..256 {
            connections.spawn(TcpStream::connect(address));
        }
        // Keep sockets alive and deliberately do not accept until the burst
        // has connected. A backlog of 128 cannot accommodate this burst.
        let clients = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let mut clients = Vec::new();
            while let Some(result) = connections.join_next().await {
                clients.push(result.unwrap().unwrap());
            }
            clients
        })
        .await
        .expect("connection burst should fit in the pending accept queue");
        assert_eq!(clients.len(), 256);
    }
}
