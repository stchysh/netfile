use super::protocol::DiscoveryMessage;
use anyhow::Result;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tokio::time;
use tracing::{debug, error, info, warn};

const BROADCAST_ADDR: &str = "255.255.255.255";
const BROADCAST_PORT_START: u16 = 37020;
const BROADCAST_PORT_END: u16 = 37040;
const HEARTBEAT_TIMEOUT: u64 = 15;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Device {
    pub device_id: String,
    pub instance_id: String,
    pub device_name: String,
    pub instance_name: String,
    pub ip: IpAddr,
    pub port: u16,
    pub version: String,
    #[serde(skip)]
    pub last_seen: SystemTime,
    pub is_self: bool,
}

pub struct DiscoveryService {
    socket: Arc<UdpSocket>,
    devices: Arc<RwLock<HashMap<String, Device>>>,
    local_message: DiscoveryMessage,
    broadcast_interval: Duration,
    local_port: u16,
}

impl DiscoveryService {
    pub async fn new(
        discovery_port: u16,
        device_id: String,
        instance_id: String,
        device_name: String,
        instance_name: String,
        transfer_port: u16,
        broadcast_interval: u64,
    ) -> Result<Self> {
        let port = if discovery_port == 0 {
            Self::find_available_port().await?
        } else {
            discovery_port
        };

        let socket = UdpSocket::bind(format!("0.0.0.0:{}", port)).await?;
        socket.set_broadcast(true)?;

        let local_message = DiscoveryMessage::new(
            device_id,
            instance_id,
            device_name,
            instance_name,
            transfer_port,
        );

        Ok(Self {
            socket: Arc::new(socket),
            devices: Arc::new(RwLock::new(HashMap::new())),
            local_message,
            broadcast_interval: Duration::from_secs(broadcast_interval),
            local_port: port,
        })
    }

    async fn find_available_port() -> Result<u16> {
        for port in 37020..37040 {
            if let Ok(socket) = UdpSocket::bind(format!("0.0.0.0:{}", port)).await {
                drop(socket);
                return Ok(port);
            }
        }
        Err(anyhow::anyhow!("No available port found"))
    }

    pub async fn start(self: Arc<Self>) {
        let broadcast_task = {
            let service = self.clone();
            tokio::spawn(async move {
                service.broadcast_loop().await;
            })
        };

        let receive_task = {
            let service = self.clone();
            tokio::spawn(async move {
                service.receive_loop().await;
            })
        };

        let cleanup_task = {
            let service = self.clone();
            tokio::spawn(async move {
                service.cleanup_loop().await;
            })
        };

        let _ = tokio::join!(broadcast_task, receive_task, cleanup_task);
    }

    async fn broadcast_loop(&self) {
        let mut interval = time::interval(self.broadcast_interval);
        loop {
            interval.tick().await;
            if let Err(e) = self.broadcast().await {
                error!("Failed to broadcast: {}", e);
            }
        }
    }

    async fn broadcast(&self) -> Result<()> {
        let data = self.local_message.to_bytes()?;

        for port in BROADCAST_PORT_START..=BROADCAST_PORT_END {
            if port == self.local_port {
                continue;
            }

            // 只发送到本地回环地址，避免广播到不存在的端口产生错误
            let addr_str = format!("127.0.0.1:{}", port);
            let _ = self.socket.send_to(&data, &addr_str).await;
        }

        Ok(())
    }

    async fn receive_loop(&self) {
        let mut buf = vec![0u8; 1024];
        loop {
            match self.socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    let _ = self.handle_message(&buf[..len], addr).await;
                }
                Err(_) => {}
            }
        }
    }

    async fn handle_message(&self, data: &[u8], addr: SocketAddr) -> Result<()> {
        let message = DiscoveryMessage::from_bytes(data)?;

        if message.instance_id == self.local_message.instance_id {
            return Ok(());
        }

        let device = Device {
            device_id: message.device_id.clone(),
            instance_id: message.instance_id.clone(),
            device_name: message.device_name.clone(),
            instance_name: message.instance_name.clone(),
            ip: addr.ip(),
            port: message.port,
            version: message.version.clone(),
            last_seen: SystemTime::now(),
            is_self: false,
        };

        let mut devices = self.devices.write().await;
        devices.insert(message.instance_id.clone(), device);

        Ok(())
    }

    async fn cleanup_loop(&self) {
        let mut interval = time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            self.cleanup_stale_devices().await;
        }
    }

    async fn cleanup_stale_devices(&self) {
        let now = SystemTime::now();
        let timeout = Duration::from_secs(HEARTBEAT_TIMEOUT);

        let mut devices = self.devices.write().await;

        devices.retain(|_, device| {
            if let Ok(elapsed) = now.duration_since(device.last_seen) {
                elapsed < timeout
            } else {
                false
            }
        });
    }

    pub async fn get_devices(&self) -> Vec<Device> {
        let mut devices: Vec<Device> = self.devices.read().await.values().cloned().collect();

        // 添加本机设备
        let local_device = Device {
            device_id: self.local_message.device_id.clone(),
            instance_id: self.local_message.instance_id.clone(),
            device_name: self.local_message.device_name.clone(),
            instance_name: self.local_message.instance_name.clone(),
            ip: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            port: self.local_message.port,
            version: self.local_message.version.clone(),
            last_seen: SystemTime::now(),
            is_self: true,
        };
        devices.push(local_device);

        devices
    }

    pub async fn get_device(&self, instance_id: &str) -> Option<Device> {
        self.devices.read().await.get(instance_id).cloned()
    }

    pub fn local_port(&self) -> Result<u16> {
        Ok(self.local_port)
    }
}
