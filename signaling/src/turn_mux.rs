//! Share the HTTP port with authenticated TURN/TCP for a raw FRP TCP mapping.
//! HTTP/WebSocket sockets are handed to Axum unchanged, preserving peer addresses.
use std::{io, net::SocketAddr, time::Duration};

use axum::{
    extract::connect_info::Connected,
    serve::{IncomingStream, Listener},
};
use futures_util::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::timeout,
};

type PendingSocket = BoxFuture<'static, Option<(TcpStream, SocketAddr, bool)>>;

pub struct WebTurnListener {
    listener: TcpListener,
    turn: Option<SocketAddr>,
    pending: FuturesUnordered<PendingSocket>,
    tunnels: JoinSet<()>,
}

#[derive(Clone, Copy)]
pub struct PeerAddress(pub SocketAddr);

impl Connected<IncomingStream<'_, WebTurnListener>> for PeerAddress {
    fn connect_info(stream: IncomingStream<'_, WebTurnListener>) -> Self {
        Self(*stream.remote_addr())
    }
}

impl WebTurnListener {
    pub fn new(listener: TcpListener, turn: Option<SocketAddr>) -> Self {
        Self {
            listener,
            turn,
            pending: FuturesUnordered::new(),
            tunnels: JoinSet::new(),
        }
    }
}

impl Listener for WebTurnListener {
    type Io = TcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (TcpStream, SocketAddr) {
        loop {
            tokio::select! {
                accepted = self.listener.accept(), if self.pending.len() < 128 => {
                    match accepted {
                        Ok((stream, address)) => {
                            let _ = stream.set_nodelay(true);
                            if self.turn.is_none() { return (stream, address); }
                            // Chrome can preconnect an idle HTTP socket. Inspect
                            // each socket concurrently, with a bounded deadline.
                            self.pending.push(Box::pin(async move {
                                let mut prefix = [0_u8; 1];
                                if timeout(Duration::from_secs(3), stream.peek(&mut prefix)).await.ok()?.ok()? == 0 {
                                    return None;
                                }
                                // The first TURN packet is a STUN request, whose
                                // first byte is 0. HTTP methods start with ASCII.
                                Some((stream, address, prefix[0] == 0))
                            }));
                        }
                        Err(error) => {
                            tracing::warn!(%error, "TCP accept failed");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
                ready = self.pending.next(), if !self.pending.is_empty() => {
                    if let Some(Some((mut client, address, is_turn))) = ready {
                        if !is_turn { return (client, address); }
                        if self.tunnels.len() >= 128 { continue; }
                        let target = self.turn.expect("TURN inspection requires a configured target");
                        self.tunnels.spawn(async move {
                            let Ok(Ok(mut relay)) = timeout(Duration::from_secs(3), TcpStream::connect(target)).await else { return; };
                            let _ = relay.set_nodelay(true);
                            tracing::info!(peer = %address, "TURN TCP connected through Web port");
                            // Authentication/permissions remain enforced by the
                            // bundled TURN server; this is not an open relay.
                            let _ = copy_bidirectional(&mut client, &mut relay).await;
                        });
                    }
                }
                _ = self.tunnels.join_next(), if !self.tunnels.is_empty() => {}
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

pub fn bridge_target(value: Option<String>) -> anyhow::Result<Option<SocketAddr>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let target: SocketAddr = value.parse()?;
    anyhow::ensure!(
        target.ip().is_loopback() && target.port() != 0,
        "TURN bridge must target a nonzero loopback port"
    );
    Ok(Some(target))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::ConnectInfo, routing::get};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn serve(turn: Option<SocketAddr>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/healthz",
            get(
                |ConnectInfo(PeerAddress(peer)): ConnectInfo<PeerAddress>| async move {
                    format!("ok {}", peer.ip())
                },
            ),
        );
        let task = tokio::spawn(async move {
            axum::serve(
                WebTurnListener::new(listener, turn),
                app.into_make_service_with_connect_info::<PeerAddress>(),
            )
            .await
            .unwrap();
        });
        (address, task)
    }

    async fn check_http(address: SocketAddr) {
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        timeout(Duration::from_secs(2), client.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(
            response.ends_with("ok 127.0.0.1"),
            "HTTP peer address was not preserved"
        );
    }

    #[test]
    fn bridge_is_opt_in_and_restricted_to_loopback() {
        assert_eq!(bridge_target(None).unwrap(), None);
        for address in ["127.0.0.1:3478", "[::1]:3478"] {
            assert_eq!(
                bridge_target(Some(address.into())).unwrap(),
                Some(address.parse().unwrap())
            );
        }
        for address in [
            "",
            "127.0.0.1:0",
            "0.0.0.0:3478",
            "192.168.0.1:3478",
            "203.0.113.1:3478",
            "example.com:3478",
        ] {
            assert!(
                bridge_target(Some(address.into())).is_err(),
                "accepted {address}"
            );
        }
    }

    #[tokio::test]
    async fn disabled_bridge_does_not_wait_for_a_protocol_prefix() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(socket.local_addr().unwrap())
            .await
            .unwrap();
        let mut listener = WebTurnListener::new(socket, None);
        let (_accepted, address) = timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap();
        assert_eq!(address, client.local_addr().unwrap());
    }

    #[tokio::test]
    async fn idle_preconnect_does_not_block_http_or_change_its_peer_address() {
        let (address, server) = serve(Some("127.0.0.1:9".parse().unwrap())).await;
        let _idle = TcpStream::connect(address).await.unwrap();
        check_http(address).await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn turn_prefix_and_payload_are_forwarded_unchanged_alongside_http() {
        let relay = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (address, server) = serve(Some(relay.local_addr().unwrap())).await;
        let echo = tokio::spawn(async move {
            let (mut stream, _) = relay.accept().await.unwrap();
            let mut payload = [0_u8; 8];
            stream.read_exact(&mut payload).await.unwrap();
            stream.write_all(&payload).await.unwrap();
            let mut remaining = Vec::new();
            stream.read_to_end(&mut remaining).await.unwrap();
            assert!(remaining.is_empty());
            payload
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        let payload = [0, 3, 0, 0, 0x21, 0x12, 0xa4, 0x42];
        // The protocol prefix can arrive separately from the rest of STUN.
        client.write_all(&payload[..1]).await.unwrap();
        tokio::task::yield_now().await;
        client.write_all(&payload[1..]).await.unwrap();
        let mut received = [0_u8; 8];
        timeout(Duration::from_secs(2), client.read_exact(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received, payload);
        check_http(address).await;
        drop(client);
        assert_eq!(
            timeout(Duration::from_secs(2), echo)
                .await
                .unwrap()
                .unwrap(),
            payload
        );
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn dropping_the_listener_closes_existing_turn_tunnels() {
        let relay = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (address, server) = serve(Some(relay.local_addr().unwrap())).await;
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[0]).await.unwrap();
        let (mut upstream, _) = timeout(Duration::from_secs(2), relay.accept())
            .await
            .unwrap()
            .unwrap();
        let mut prefix = [0_u8; 1];
        upstream.read_exact(&mut prefix).await.unwrap();
        server.abort();
        let _ = server.await;
        for mut stream in [client, upstream] {
            let read = timeout(Duration::from_secs(2), stream.read(&mut prefix))
                .await
                .unwrap();
            assert!(
                matches!(read, Ok(0) | Err(_)),
                "TURN socket survived listener shutdown"
            );
        }
    }
}
