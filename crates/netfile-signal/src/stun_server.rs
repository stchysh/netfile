use std::net::{IpAddr, SocketAddr};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

pub async fn run_stun_server(bind_addr: SocketAddr) {
    let socket = match UdpSocket::bind(bind_addr).await {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to bind STUN server on {}: {}", bind_addr, e);
            return;
        }
    };
    info!("STUN server listening on {}", bind_addr);

    let mut buf = [0u8; 1500];
    loop {
        let (len, src) = match socket.recv_from(&mut buf).await {
            Ok(r) => r,
            Err(e) => {
                warn!("STUN recv error: {}", e);
                continue;
            }
        };

        if len < 20 {
            continue;
        }

        if u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) != 0x2112_A442 {
            continue;
        }

        if u16::from_be_bytes([buf[0], buf[1]]) != 0x0001 {
            continue;
        }

        let transaction_id = buf[8..20].to_vec();
        let port = src.port();

        let attr = match src.ip() {
            IpAddr::V4(ipv4) => {
                let ip_bytes = ipv4.octets();
                let xor_port = port ^ 0x2112u16;
                let magic = 0x2112_A442u32.to_be_bytes();
                let xor_ip = [
                    ip_bytes[0] ^ magic[0],
                    ip_bytes[1] ^ magic[1],
                    ip_bytes[2] ^ magic[2],
                    ip_bytes[3] ^ magic[3],
                ];
                let mut a = Vec::with_capacity(12);
                a.extend_from_slice(&[0x00, 0x20]);
                a.extend_from_slice(&[0x00, 0x08]);
                a.extend_from_slice(&[0x00, 0x01]);
                a.push((xor_port >> 8) as u8);
                a.push((xor_port & 0xFF) as u8);
                a.extend_from_slice(&xor_ip);
                a
            }
            IpAddr::V6(_) => continue,
        };

        let mut response = Vec::with_capacity(20 + attr.len());
        response.extend_from_slice(&[0x01, 0x01]);
        response.extend_from_slice(&(attr.len() as u16).to_be_bytes());
        response.extend_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
        response.extend_from_slice(&transaction_id);
        response.extend_from_slice(&attr);

        if let Err(e) = socket.send_to(&response, src).await {
            debug!("STUN send error to {}: {}", src, e);
        }
    }
}
