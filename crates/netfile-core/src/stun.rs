use anyhow::Result;
use std::net::SocketAddr;
use stun::message::{Getter, Message, BINDING_REQUEST};
use stun::xoraddr::XorMappedAddress;
use tracing::{debug, info, warn};

pub struct StunClient {
    stun_servers: Vec<String>,
}

impl StunClient {
    pub fn new() -> Self {
        Self {
            stun_servers: vec![
                "stun.cloudflare.com:3478".to_string(),
                "stun.l.google.com:19302".to_string(),
                "stun1.l.google.com:19302".to_string(),
                "stun2.l.google.com:19302".to_string(),
            ],
        }
    }

    pub fn with_servers(servers: Vec<String>) -> Self {
        Self {
            stun_servers: servers,
        }
    }

    pub async fn get_public_address(&self) -> Result<SocketAddr> {
        for server in &self.stun_servers {
            let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
            match Self::query_with_socket(&socket, server).await {
                Ok(addr) => {
                    info!("Got public address from {}: {}", server, addr);
                    return Ok(addr);
                }
                Err(e) => {
                    warn!("Failed to query STUN server {}: {}", server, e);
                    continue;
                }
            }
        }
        Err(anyhow::anyhow!("Failed to get public address from all STUN servers"))
    }

    pub async fn get_public_addr_for_port(&self, local_port: u16) -> Result<SocketAddr> {
        let socket = tokio::net::UdpSocket::bind(format!("0.0.0.0:{}", local_port)).await?;
        for server in &self.stun_servers {
            match Self::query_with_socket(&socket, server).await {
                Ok(addr) => {
                    info!("Got public address for port {} from {}: {}", local_port, server, addr);
                    return Ok(addr);
                }
                Err(e) => {
                    warn!("Failed to query STUN server {} for port {}: {}", server, local_port, e);
                    continue;
                }
            }
        }
        Err(anyhow::anyhow!("Failed to get public address from all STUN servers"))
    }

    async fn query_with_socket(socket: &tokio::net::UdpSocket, server: &str) -> Result<SocketAddr> {
        debug!("Querying STUN server: {}", server);

        let server_addr: SocketAddr = tokio::net::lookup_host(server)
            .await?
            .next()
            .ok_or_else(|| anyhow::anyhow!("Failed to resolve STUN server"))?;

        let mut msg = Message::new();
        msg.build(&[Box::new(BINDING_REQUEST)])?;

        socket.send_to(&msg.raw, server_addr).await?;

        let mut buf = vec![0u8; 1500];
        let timeout = tokio::time::Duration::from_secs(3);
        let (len, _) = tokio::time::timeout(timeout, socket.recv_from(&mut buf)).await??;

        let mut response = Message::new();
        response.raw = buf[..len].to_vec();
        response.decode()?;

        let mut xor_addr = XorMappedAddress::default();
        xor_addr.get_from(&response)?;

        let public_addr = SocketAddr::new(xor_addr.ip, xor_addr.port);
        debug!("Public address from {}: {}", server, public_addr);

        Ok(public_addr)
    }

    pub async fn detect_nat_type(&self) -> Result<NatType> {
        let public_addr = self.get_public_address().await?;

        let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
        let local_addr = socket.local_addr()?;

        if public_addr.ip() == local_addr.ip() {
            info!("NAT type: No NAT (public IP)");
            return Ok(NatType::NoNat);
        }

        info!("NAT type: Behind NAT");
        Ok(NatType::SymmetricNat)
    }
}

impl Default for StunClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatType {
    NoNat,
    FullCone,
    RestrictedCone,
    PortRestrictedCone,
    SymmetricNat,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stun_client_creation() {
        let client = StunClient::new();
        assert!(!client.stun_servers.is_empty());
    }

    #[tokio::test]
    async fn test_custom_stun_servers() {
        let servers = vec!["stun.example.com:3478".to_string()];
        let client = StunClient::with_servers(servers.clone());
        assert_eq!(client.stun_servers, servers);
    }
}
