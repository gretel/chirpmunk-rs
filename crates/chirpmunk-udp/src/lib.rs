// SPDX-License-Identifier: GPL-3.0-only

//! UDP fanout for chirpmunk apps.
//!
//! Mirrors `gr4-lora/apps/udp_state.hpp`. A client sends a CBOR
//! `subscribe` datagram to register; the daemon then broadcasts CBOR
//! frames back. Clients with `sync_word` filters receive only matching
//! frames; unfiltered clients receive everything.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tracing::{debug, info, trace, warn};

const MAX_DATAGRAM: usize = 64 * 1024;
const MAX_SEND_FAILURES: u32 = 10;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("cbor: {0}")]
    Cbor(#[from] chirpmunk_cbor::Error),
}

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, Clone)]
struct Client {
    sync_words: Vec<u16>,
    send_failures: u32,
}

/// UDP server: accepts subscribers, broadcasts CBOR frames.
#[derive(Clone)]
pub struct Server {
    socket: Arc<UdpSocket>,
    clients: Arc<Mutex<HashMap<SocketAddr, Client>>>,
}

impl Server {
    pub async fn bind(addr: impl tokio::net::ToSocketAddrs) -> Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        let local = socket.local_addr()?;
        info!(?local, "chirpmunk-udp bound");
        Ok(Self {
            socket: Arc::new(socket),
            clients: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Process incoming datagrams forever. Spawn this in a tokio task.
    pub async fn run(self) -> Result<()> {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        loop {
            let (n, peer) = match self.socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "recv_from failed");
                    continue;
                }
            };
            self.handle_datagram(&buf[..n], peer).await;
        }
    }

    async fn handle_datagram(&self, bytes: &[u8], peer: SocketAddr) {
        let ty = chirpmunk_cbor::peek_type(bytes).ok();
        match ty.as_deref() {
            Some("subscribe") => match chirpmunk_cbor::Subscribe::from_slice(bytes) {
                Ok(sub) => self.add_client(peer, sub.sync_words).await,
                Err(e) => warn!(?peer, error = %e, "bad subscribe"),
            },
            Some(other) => {
                trace!(?peer, frame_type = other, "ignored frame");
            }
            None => {
                self.add_client(peer, Vec::new()).await;
            }
        }
    }

    async fn add_client(&self, peer: SocketAddr, sync_words: Vec<u16>) {
        let mut clients = self.clients.lock().await;
        clients.insert(
            peer,
            Client {
                sync_words: sync_words.clone(),
                send_failures: 0,
            },
        );
        let total = clients.len();
        info!(?peer, filter = ?sync_words, total, "client subscribed");
    }

    /// Snapshot of currently-registered clients (test/inspection only).
    pub async fn client_count(&self) -> usize {
        self.clients.lock().await.len()
    }

    /// Broadcast `bytes` to all clients whose filter accepts `sync_word`.
    /// `None` means broadcast to every client (subscribe filter ignored).
    pub async fn broadcast(&self, bytes: &[u8], sync_word: Option<u16>) -> Result<()> {
        let snapshot: Vec<(SocketAddr, Vec<u16>)> = {
            let clients = self.clients.lock().await;
            clients
                .iter()
                .map(|(addr, c)| (*addr, c.sync_words.clone()))
                .collect()
        };

        let mut to_evict: Vec<SocketAddr> = Vec::new();

        for (addr, filter) in snapshot {
            let accepts = match sync_word {
                Some(sw) => filter.is_empty() || filter.contains(&sw),
                None => true,
            };
            if !accepts {
                continue;
            }
            match self.socket.send_to(bytes, addr).await {
                Ok(_) => {
                    if let Some(c) = self.clients.lock().await.get_mut(&addr) {
                        c.send_failures = 0;
                    }
                }
                Err(e) => {
                    debug!(?addr, error = %e, "send_to failed");
                    let mut clients = self.clients.lock().await;
                    if let Some(c) = clients.get_mut(&addr) {
                        c.send_failures += 1;
                        if c.send_failures >= MAX_SEND_FAILURES {
                            to_evict.push(addr);
                        }
                    }
                }
            }
        }

        if !to_evict.is_empty() {
            let mut clients = self.clients.lock().await;
            for addr in to_evict {
                clients.remove(&addr);
                info!(?addr, "client evicted (send failures)");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chirpmunk_cbor::{Frame, Subscribe};
    use tokio::time::{Duration, timeout};

    async fn make_server() -> Server {
        Server::bind("127.0.0.1:0").await.expect("bind")
    }

    #[tokio::test]
    async fn subscribe_registers_client() {
        let server = make_server().await;
        let server_addr = server.local_addr().unwrap();

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let buf = chirpmunk_cbor::to_vec(&Subscribe::default()).unwrap();
        client.send_to(&buf, server_addr).await.unwrap();

        let s = server.clone();
        tokio::spawn(async move { s.run().await });

        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if server.client_count().await == 1 {
                return;
            }
        }
        panic!("client never registered");
    }

    #[tokio::test]
    async fn broadcast_reaches_subscriber() {
        let server = make_server().await;
        let server_addr = server.local_addr().unwrap();

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sub_buf = chirpmunk_cbor::to_vec(&Subscribe::default()).unwrap();
        client.send_to(&sub_buf, server_addr).await.unwrap();

        let s = server.clone();
        tokio::spawn(async move { s.run().await });

        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if server.client_count().await == 1 {
                break;
            }
        }
        assert_eq!(server.client_count().await, 1);

        let mut payload = Vec::new();
        let mut e = minicbor::Encoder::new(&mut payload);
        e.map(2).unwrap();
        e.str("type")
            .unwrap()
            .str(<Subscribe as Frame>::TYPE)
            .unwrap();
        e.str("seq").unwrap().u32(1).unwrap();

        server.broadcast(&payload, None).await.unwrap();

        let mut rx = vec![0u8; 1024];
        let n = timeout(Duration::from_millis(500), client.recv(&mut rx))
            .await
            .expect("timeout")
            .unwrap();
        assert_eq!(&rx[..n], &payload[..]);
    }

    #[tokio::test]
    async fn filter_blocks_mismatch() {
        let server = make_server().await;
        let server_addr = server.local_addr().unwrap();

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sub = Subscribe {
            sync_words: vec![0x12],
        };
        let sub_buf = chirpmunk_cbor::to_vec(&sub).unwrap();
        client.send_to(&sub_buf, server_addr).await.unwrap();

        let s = server.clone();
        tokio::spawn(async move { s.run().await });

        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if server.client_count().await == 1 {
                break;
            }
        }
        assert_eq!(server.client_count().await, 1);

        server.broadcast(b"x", Some(0x2B)).await.unwrap();
        let mut rx = vec![0u8; 16];
        let r = timeout(Duration::from_millis(150), client.recv(&mut rx)).await;
        assert!(r.is_err(), "client should not have received the frame");
    }
}
