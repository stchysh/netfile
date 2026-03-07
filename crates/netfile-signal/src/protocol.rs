use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FriendInfo {
    pub device_id: String,
    pub instance_name: String,
    pub online: bool,
    pub transfer_addr: Option<String>,
    #[serde(default)]
    pub iroh_addr: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OfflineMsg {
    pub from_device_id: String,
    pub from_instance_name: String,
    pub content: String,
    pub timestamp: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum C2sMsg {
    Register {
        device_id: String,
        instance_name: String,
        transfer_addr: String,
        #[serde(default)]
        nat_type: String,
    },
    GenerateInvite,
    AcceptInvite {
        code: String,
    },
    RelayMessage {
        to_device_id: String,
        content: String,
        timestamp: u64,
    },
    UpdateTransferAddr {
        transfer_addr: String,
    },
    UpdateIrohAddr {
        iroh_addr: String,
    },
    RequestRelay {
        to_device_id: String,
    },
    Heartbeat,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum S2cMsg {
    Registered {
        friends: Vec<FriendInfo>,
        #[serde(default)]
        observed_addr: String,
        #[serde(default)]
        relay_addr: Option<String>,
    },
    InviteCode {
        code: String,
    },
    InviteResult {
        success: bool,
        friend: Option<FriendInfo>,
        error: Option<String>,
    },
    FriendOnline {
        device_id: String,
        instance_name: String,
        transfer_addr: String,
        #[serde(default)]
        iroh_addr: Option<String>,
    },
    FriendOffline {
        device_id: String,
    },
    RelayedMessage {
        from_device_id: String,
        from_instance_name: String,
        content: String,
        timestamp: u64,
    },
    OfflineMessages {
        messages: Vec<OfflineMsg>,
    },
    RelayReady {
        session_key: String,
        relay_addr: String,
    },
    Error {
        message: String,
    },
}

pub fn encode_s2c(msg: &S2cMsg) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(msg)?;
    let len = json.len() as u32;
    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&json);
    Ok(buf)
}

pub async fn read_c2s(stream: &mut TcpStream) -> Result<C2sMsg> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    let msg = serde_json::from_slice(&buf)?;
    Ok(msg)
}

pub async fn write_s2c(stream: &mut TcpStream, msg: &S2cMsg) -> Result<()> {
    let encoded = encode_s2c(msg)?;
    stream.write_all(&encoded).await?;
    Ok(())
}
