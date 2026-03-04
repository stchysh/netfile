use super::protocol::DiscoveryMessage;
use anyhow::Result;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::net::UdpSocket;
use tokio::sync::{Notify, RwLock};
use tokio::time;
use tracing::{debug, error, info, warn};

const BROADCAST_ADDR: &str = "255.255.255.255";
const BROADCAST_PORT_START: u16 = 37020;
const BROADCAST_PORT_END: u16 = 37040;
const HEARTBEAT_TIMEOUT: u64 = 15;
const MIN_RECV_INTERVAL: Duration = Duration::from_millis(500);
const MIN_BROADCAST_INTERVAL: Duration = Duration::from_secs(3);

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
    local_message: Arc<RwLock<DiscoveryMessage>>,
    broadcast_interval: Arc<RwLock<Duration>>,
    recv_timestamps: Arc<RwLock<HashMap<String, (u64, Instant)>>>,
    last_broadcast_at: Arc<RwLock<Option<Instant>>>,
    broadcast_notify: Arc<Notify>,
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
            local_message: Arc::new(RwLock::new(local_message)),
            broadcast_interval: Arc::new(RwLock::new(Duration::from_secs(broadcast_interval))),
            recv_timestamps: Arc::new(RwLock::new(HashMap::new())),
            last_broadcast_at: Arc::new(RwLock::new(None)),
            broadcast_notify: Arc::new(Notify::new()),
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
        loop {
            let interval = *self.broadcast_interval.read().await;
            tokio::select! {
                _ = time::sleep(interval) => {}
                _ = self.broadcast_notify.notified() => {}
            }

            // 强制最小发送间隔
            {
                let last = self.last_broadcast_at.read().await;
                if let Some(last_at) = *last {
                    let elapsed = last_at.elapsed();
                    if elapsed < MIN_BROADCAST_INTERVAL {
                        let wait = MIN_BROADCAST_INTERVAL - elapsed;
                        drop(last);
                        time::sleep(wait).await;
                    }
                }
            }

            *self.last_broadcast_at.write().await = Some(Instant::now());
            if let Err(e) = self.broadcast().await {
                error!("Failed to broadcast: {}", e);
            }
        }
    }

    async fn broadcast(&self) -> Result<()> {
        {
            let mut msg = self.local_message.write().await;
            msg.timestamp = SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
        }
        let data = self.local_message.read().await.to_bytes()?;

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

        if message.instance_id == self.local_message.read().await.instance_id {
            return Ok(());
        }

        {
            let mut recv_ts = self.recv_timestamps.write().await;
            if let Some(&(last_ts, last_at)) = recv_ts.get(&message.instance_id) {
                if message.timestamp <= last_ts || last_at.elapsed() < MIN_RECV_INTERVAL {
                    return Ok(());
                }
            }
            recv_ts.insert(message.instance_id.clone(), (message.timestamp, Instant::now()));
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

        let mut stale_ids = Vec::new();
        let mut devices = self.devices.write().await;
        devices.retain(|instance_id, device| {
            let keep = match now.duration_since(device.last_seen) {
                Ok(elapsed) => elapsed < timeout,
                Err(_) => false,
            };
            if !keep {
                stale_ids.push(instance_id.clone());
            }
            keep
        });
        drop(devices);

        if !stale_ids.is_empty() {
            let mut recv_ts = self.recv_timestamps.write().await;
            for id in &stale_ids {
                recv_ts.remove(id);
            }
        }
    }

    pub async fn get_devices(&self) -> Vec<Device> {
        let mut devices: Vec<Device> = self.devices.read().await.values().cloned().collect();

        let msg = self.local_message.read().await;
        let local_device = Device {
            device_id: msg.device_id.clone(),
            instance_id: msg.instance_id.clone(),
            device_name: msg.device_name.clone(),
            instance_name: msg.instance_name.clone(),
            ip: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            port: msg.port,
            version: msg.version.clone(),
            last_seen: SystemTime::now(),
            is_self: true,
        };
        drop(msg);
        devices.push(local_device);

        devices
    }

    pub async fn update_device_info(&self, device_name: String, instance_name: String) {
        let mut msg = self.local_message.write().await;
        msg.device_name = device_name;
        msg.instance_name = instance_name;
        drop(msg);
        self.broadcast_notify.notify_one();
    }

    pub async fn update_broadcast_interval(&self, secs: u64) {
        *self.broadcast_interval.write().await = Duration::from_secs(secs);
        self.broadcast_notify.notify_one();
    }

    pub async fn get_device(&self, instance_id: &str) -> Option<Device> {
        self.devices.read().await.get(instance_id).cloned()
    }

    pub fn local_port(&self) -> Result<u16> {
        Ok(self.local_port)
    }
}
