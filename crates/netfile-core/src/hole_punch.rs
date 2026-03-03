use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tokio::time::{timeout, Duration};
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
pub struct HolePunchRequest {
    pub peer_id: String,
    pub peer_addr: SocketAddr,
}

#[derive(Debug, Clone)]
pub struct HolePunchResponse {
    pub success: bool,
    pub local_addr: SocketAddr,
    pub peer_addr: SocketAddr,
}

pub struct UdpHolePuncher {
    local_socket: Arc<UdpSocket>,
    peers: Arc<RwLock<Vec<SocketAddr>>>,
}

impl UdpHolePuncher {
    pub async fn new(local_port: u16) -> Result<Self> {
        let socket = UdpSocket::bind(format!("0.0.0.0:{}", local_port)).await?;
        let local_addr = socket.local_addr()?;
        info!("UDP hole puncher bound to {}", local_addr);

        Ok(Self {
            local_socket: Arc::new(socket),
            peers: Arc::new(RwLock::new(Vec::new())),
        })
    }

    pub async fn punch_hole(&self, peer_addr: SocketAddr) -> Result<HolePunchResponse> {
        info!("Attempting to punch hole to {}", peer_addr);

        let punch_msg = b"PUNCH";
        let mut attempts = 0;
        let max_attempts = 10;

        while attempts < max_attempts {
            self.local_socket.send_to(punch_msg, peer_addr).await?;
            debug!("Sent punch message to {} (attempt {})", peer_addr, attempts + 1);

            match timeout(Duration::from_millis(500), self.wait_for_response()).await {
                Ok(Ok((response_addr, data))) => {
                    if data == b"PUNCH_ACK" {
                        info!("Successfully punched hole to {}", response_addr);
                        self.peers.write().await.push(response_addr);

                        return Ok(HolePunchResponse {
                            success: true,
                            local_addr: self.local_socket.local_addr()?,
                            peer_addr: response_addr,
                        });
                    }
                }
                Ok(Err(e)) => {
                    warn!("Error waiting for response: {}", e);
                }
                Err(_) => {
                    debug!("Timeout waiting for response (attempt {})", attempts + 1);
                }
            }

            attempts += 1;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        warn!("Failed to punch hole to {} after {} attempts", peer_addr, max_attempts);
        Ok(HolePunchResponse {
            success: false,
            local_addr: self.local_socket.local_addr()?,
            peer_addr,
        })
    }

    async fn wait_for_response(&self) -> Result<(SocketAddr, Vec<u8>)> {
        let mut buf = vec![0u8; 1024];
        let (len, addr) = self.local_socket.recv_from(&mut buf).await?;
        buf.truncate(len);
        Ok((addr, buf))
    }

    pub async fn listen_for_punches(&self) -> Result<()> {
        info!("Listening for incoming punch requests");

        loop {
            let mut buf = vec![0u8; 1024];
            match self.local_socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    buf.truncate(len);

                    if buf == b"PUNCH" {
                        info!("Received punch request from {}", addr);

                        let ack_msg = b"PUNCH_ACK";
                        if let Err(e) = self.local_socket.send_to(ack_msg, addr).await {
                            warn!("Failed to send punch ACK to {}: {}", addr, e);
                        } else {
                            debug!("Sent punch ACK to {}", addr);
                            self.peers.write().await.push(addr);
                        }
                    }
                }
                Err(e) => {
                    warn!("Error receiving punch request: {}", e);
                }
            }
        }
    }

    pub async fn send_data(&self, peer_addr: SocketAddr, data: &[u8]) -> Result<()> {
        self.local_socket.send_to(data, peer_addr).await?;
        debug!("Sent {} bytes to {}", data.len(), peer_addr);
        Ok(())
    }

    pub async fn recv_data(&self) -> Result<(SocketAddr, Vec<u8>)> {
        let mut buf = vec![0u8; 65536];
        let (len, addr) = self.local_socket.recv_from(&mut buf).await?;
        buf.truncate(len);
        debug!("Received {} bytes from {}", len, addr);
        Ok((addr, buf))
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.local_socket.local_addr()?)
    }

    pub async fn list_peers(&self) -> Vec<SocketAddr> {
        self.peers.read().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_hole_puncher_creation() {
        let puncher = UdpHolePuncher::new(0).await;
        assert!(puncher.is_ok());

        let puncher = puncher.unwrap();
        let local_addr = puncher.local_addr().unwrap();
        assert!(local_addr.port() > 0);
    }

    #[tokio::test]
    async fn test_peer_list() {
        let puncher = UdpHolePuncher::new(0).await.unwrap();
        let peers = puncher.list_peers().await;
        assert_eq!(peers.len(), 0);
    }
}
