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
                "stun.miwifi.com:3478".to_string(),
                "stun.cloudflare.com:3478".to_string(),
                "stun.qq.com:3478".to_string(),
                "stun.syncthing.net:3478".to_string(),
                "stun.stunprotocol.org:3478".to_string(),
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

    pub async fn get_public_address_with_socket(&self, socket: &tokio::net::UdpSocket) -> Result<SocketAddr> {
        for server in &self.stun_servers {
            match Self::query_with_socket(socket, server).await {
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

    pub async fn detect_nat_type_with_socket(&self, socket: &tokio::net::UdpSocket) -> Result<NatType> {
        if self.stun_servers.len() < 2 {
            return Err(anyhow::anyhow!("Need at least 2 STUN servers for NAT type detection"));
        }

        let addr1 = Self::query_with_socket(socket, &self.stun_servers[0]).await;
        let addr2 = Self::query_with_socket(socket, &self.stun_servers[1]).await;

        match (addr1, addr2) {
            (Ok(a1), Ok(a2)) => {
                let local_addr = socket.local_addr()?;
                if a1.ip() == local_addr.ip() {
                    info!("NAT type: NoNat (public IP)");
                    return Ok(NatType::NoNat);
                }
                if a1.port() == a2.port() {
                    info!("NAT type: ConeNat (port {} consistent across servers {} and {})",
                        a1.port(), self.stun_servers[0], self.stun_servers[1]);
                    Ok(NatType::ConeNat)
                } else {
                    info!("NAT type: SymmetricNat (port {} vs {} from servers {} and {})",
                        a1.port(), a2.port(), self.stun_servers[0], self.stun_servers[1]);
                    Ok(NatType::SymmetricNat)
                }
            }
            (Ok(a1), Err(_)) => {
                let local_addr = socket.local_addr()?;
                if a1.ip() == local_addr.ip() {
                    Ok(NatType::NoNat)
                } else {
                    warn!("Only one STUN server responded, assuming ConeNat");
                    Ok(NatType::ConeNat)
                }
            }
            _ => Err(anyhow::anyhow!("Failed to query STUN servers for NAT detection")),
        }
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
    ConeNat,
    SymmetricNat,
}

impl NatType {
    pub fn as_str(&self) -> &'static str {
        match self {
            NatType::NoNat => "no_nat",
            NatType::ConeNat => "cone",
            NatType::SymmetricNat => "symmetric",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "no_nat" => NatType::NoNat,
            "cone" => NatType::ConeNat,
            "symmetric" => NatType::SymmetricNat,
            _ => NatType::SymmetricNat,
        }
    }

    pub fn is_punchable(&self) -> bool {
        matches!(self, NatType::NoNat | NatType::ConeNat)
    }
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
