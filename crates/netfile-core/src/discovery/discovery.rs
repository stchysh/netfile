use super::protocol::DiscoveryMessage;
use crate::stun::StunClient;
use anyhow::Result;
use std::cmp::Ordering;
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

const PUNCH_REQUEST: u8 = 0x10;
const PUNCH_ACK: u8 = 0x11;

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
    pub public_transfer_addr: Option<String>,
    pub discovery_port: u16,
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
    public_transfer_addr: Arc<RwLock<Option<String>>>,
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

        let public_transfer_addr: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

        {
            let addr_ref = public_transfer_addr.clone();
            let msg_ref = Arc::new(RwLock::new(local_message.clone()));
            let tp = transfer_port;
            tokio::spawn(async move {
                let client = StunClient::new();
                match client.get_public_address().await {
                    Ok(mut addr) => {
                        addr.set_port(tp);
                        let addr_str = addr.to_string();
                        info!("STUN discovered public transfer address: {}", addr_str);
                        *addr_ref.write().await = Some(addr_str.clone());
                        msg_ref.write().await.public_transfer_addr = Some(addr_str);
                    }
                    Err(e) => {
                        warn!("STUN query failed: {}", e);
                    }
                }
            });
        }

        Ok(Self {
            socket: Arc::new(socket),
            devices: Arc::new(RwLock::new(HashMap::new())),
            local_message: Arc::new(RwLock::new(local_message)),
            broadcast_interval: Arc::new(RwLock::new(Duration::from_secs(broadcast_interval))),
            recv_timestamps: Arc::new(RwLock::new(HashMap::new())),
            last_broadcast_at: Arc::new(RwLock::new(None)),
            broadcast_notify: Arc::new(Notify::new()),
            local_port: port,
            public_transfer_addr,
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
        let mut last_full_broadcast: Option<Instant> = None;
        const FULL_BROADCAST_INTERVAL: Duration = Duration::from_secs(30);

        loop {
            let interval = *self.broadcast_interval.read().await;
            let forced = tokio::select! {
                _ = time::sleep(interval) => false,
                _ = self.broadcast_notify.notified() => true,
            };

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

            {
                let mut msg = self.local_message.write().await;
                msg.public_transfer_addr = self.public_transfer_addr.read().await.clone();
                msg.timestamp = SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
            }

            let known_peers: Vec<SocketAddr> = self
                .devices
                .read()
                .await
                .values()
                .map(|d| SocketAddr::new(d.ip, d.discovery_port))
                .collect();

            let needs_full = forced
                || known_peers.is_empty()
                || last_full_broadcast.map_or(true, |t| t.elapsed() >= FULL_BROADCAST_INTERVAL);

            if needs_full {
                last_full_broadcast = Some(Instant::now());
                if let Err(e) = self.send_full_broadcast().await {
                    error!("Failed to broadcast: {}", e);
                }
            } else {
                self.send_unicast_to_peers(&known_peers).await;
            }
        }
    }

    async fn send_full_broadcast(&self) -> Result<()> {
        let data = self.local_message.read().await.to_bytes()?;
        for port in BROADCAST_PORT_START..=BROADCAST_PORT_END {
            let addr_str = format!("{}:{}", BROADCAST_ADDR, port);
            let _ = self.socket.send_to(&data, &addr_str).await;
        }
        Ok(())
    }

    async fn send_unicast_to_peers(&self, peers: &[SocketAddr]) {
        let Ok(data) = self.local_message.read().await.to_bytes() else {
            return;
        };
        for &peer_addr in peers {
            let _ = self.socket.send_to(&data, peer_addr).await;
        }
    }

    async fn receive_loop(&self) {
        let mut buf = vec![0u8; 4096];
        loop {
            match self.socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    let _ = self.handle_packet(&buf[..len], addr).await;
                }
                Err(_) => {}
            }
        }
    }

    async fn handle_packet(&self, data: &[u8], addr: SocketAddr) -> Result<()> {
        match data.first() {
            Some(&PUNCH_REQUEST) => {
                debug!("Received PUNCH_REQUEST from {}", addr);
                let mut ack = vec![PUNCH_ACK];
                if let Some(ref our_addr) = *self.public_transfer_addr.read().await {
                    ack.extend_from_slice(our_addr.as_bytes());
                }
                let _ = self.socket.send_to(&ack, addr).await;
                Ok(())
            }
            Some(&PUNCH_ACK) => {
                debug!("Received PUNCH_ACK from {}", addr);
                Ok(())
            }
            _ => self.handle_message(data, addr).await,
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
            recv_ts.insert(
                message.instance_id.clone(),
                (message.timestamp, Instant::now()),
            );
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
            public_transfer_addr: message.public_transfer_addr.clone(),
            discovery_port: addr.port(),
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
            public_transfer_addr: self.public_transfer_addr.read().await.clone(),
            discovery_port: self.local_port,
        };
        drop(msg);
        devices.push(local_device);

        dedupe_devices(devices)
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

    pub async fn get_my_public_transfer_addr(&self) -> Option<String> {
        self.public_transfer_addr.read().await.clone()
    }

    pub async fn set_public_transfer_addr(&self, addr: String) {
        *self.public_transfer_addr.write().await = Some(addr);
    }

    pub async fn send_punch(&self, target_addr: SocketAddr) -> Result<()> {
        let mut msg = vec![PUNCH_REQUEST];
        if let Some(ref our_addr) = *self.public_transfer_addr.read().await {
            msg.extend_from_slice(our_addr.as_bytes());
        }
        self.socket.send_to(&msg, target_addr).await?;
        debug!("Sent PUNCH_REQUEST to {}", target_addr);
        Ok(())
    }
}

fn dedupe_devices(devices: Vec<Device>) -> Vec<Device> {
    let mut merged = HashMap::new();

    for device in devices {
        let key = if device.device_id.is_empty() {
            device.instance_id.clone()
        } else {
            device.device_id.clone()
        };

        match merged.get(&key) {
            Some(existing) if !should_replace_device(existing, &device) => {}
            _ => {
                merged.insert(key, device);
            }
        }
    }

    let mut devices: Vec<Device> = merged.into_values().collect();
    devices.sort_by(|a, b| {
        b.is_self
            .cmp(&a.is_self)
            .then_with(|| a.instance_name.cmp(&b.instance_name))
            .then_with(|| a.device_name.cmp(&b.device_name))
            .then_with(|| a.ip.to_string().cmp(&b.ip.to_string()))
            .then_with(|| a.instance_id.cmp(&b.instance_id))
    });
    devices
}

fn should_replace_device(existing: &Device, candidate: &Device) -> bool {
    match (existing.is_self, candidate.is_self) {
        (false, true) => true,
        (true, false) => false,
        _ => match candidate.last_seen.cmp(&existing.last_seen) {
            Ordering::Greater => true,
            Ordering::Less => false,
            Ordering::Equal => candidate.instance_id < existing.instance_id,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_device(
        device_id: &str,
        instance_id: &str,
        instance_name: &str,
        ip: [u8; 4],
        is_self: bool,
        seen_secs: u64,
    ) -> Device {
        Device {
            device_id: device_id.to_string(),
            instance_id: instance_id.to_string(),
            device_name: "device".to_string(),
            instance_name: instance_name.to_string(),
            ip: IpAddr::V4(Ipv4Addr::from(ip)),
            port: 37030,
            version: "1.0.0".to_string(),
            last_seen: SystemTime::UNIX_EPOCH + Duration::from_secs(seen_secs),
            is_self,
            public_transfer_addr: None,
            discovery_port: 37020,
        }
    }

    #[test]
    fn dedupe_devices_prefers_self_entry() {
        let devices = vec![
            make_device(
                "device-1",
                "session-a",
                "alpha",
                [192, 168, 1, 24],
                false,
                10,
            ),
            make_device("device-1", "session-b", "alpha", [127, 0, 0, 1], true, 5),
        ];

        let deduped = dedupe_devices(devices);

        assert_eq!(deduped.len(), 1);
        assert!(deduped[0].is_self);
        assert_eq!(deduped[0].instance_id, "session-b");
    }

    #[test]
    fn dedupe_devices_keeps_latest_remote_session() {
        let devices = vec![
            make_device(
                "device-1",
                "session-a",
                "alpha",
                [192, 168, 1, 24],
                false,
                10,
            ),
            make_device(
                "device-1",
                "session-b",
                "alpha",
                [192, 168, 1, 24],
                false,
                20,
            ),
            make_device(
                "device-2",
                "session-c",
                "beta",
                [192, 168, 1, 99],
                false,
                15,
            ),
        ];

        let deduped = dedupe_devices(devices);

        assert_eq!(deduped.len(), 2);
        assert!(deduped
            .iter()
            .any(|device| device.device_id == "device-1" && device.instance_id == "session-b"));
        assert!(deduped
            .iter()
            .any(|device| device.device_id == "device-2" && device.instance_id == "session-c"));
    }
}
