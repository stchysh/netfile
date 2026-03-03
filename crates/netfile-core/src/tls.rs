use anyhow::Result;
use rcgen::{Certificate, CertificateParams, DistinguishedName, DnType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, ServerConfig};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tracing::{debug, info};

pub struct TlsManager {
    cert_path: PathBuf,
    key_path: PathBuf,
}

impl TlsManager {
    pub fn new(data_dir: PathBuf) -> Self {
        let cert_dir = data_dir.join("certs");
        Self {
            cert_path: cert_dir.join("cert.pem"),
            key_path: cert_dir.join("key.pem"),
        }
    }

    pub async fn ensure_certificate(&self) -> Result<()> {
        if self.cert_path.exists() && self.key_path.exists() {
            debug!("TLS certificate already exists");
            return Ok(());
        }

        info!("Generating self-signed TLS certificate");
        self.generate_self_signed_cert().await?;
        Ok(())
    }

    async fn generate_self_signed_cert(&self) -> Result<()> {
        let mut params = CertificateParams::default();

        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "NetFile");
        dn.push(DnType::OrganizationName, "NetFile");
        params.distinguished_name = dn;

        params.subject_alt_names = vec![
            rcgen::SanType::DnsName(rcgen::Ia5String::try_from("localhost")?),
            rcgen::SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))),
        ];

        let key_pair = rcgen::KeyPair::generate()?;
        let cert = params.self_signed(&key_pair)?;
        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();

        if let Some(parent) = self.cert_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&self.cert_path, cert_pem).await?;
        fs::write(&self.key_path, key_pem).await?;

        info!("Generated self-signed certificate at {:?}", self.cert_path);
        Ok(())
    }

    pub async fn load_server_config(&self) -> Result<Arc<ServerConfig>> {
        self.ensure_certificate().await?;

        let cert_pem = fs::read(&self.cert_path).await?;
        let key_pem = fs::read(&self.key_path).await?;

        let certs = rustls_pemfile::certs(&mut cert_pem.as_slice())
            .collect::<Result<Vec<_>, _>>()?;

        let key = rustls_pemfile::private_key(&mut key_pem.as_slice())?
            .ok_or_else(|| anyhow::anyhow!("No private key found"))?;

        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)?;

        debug!("Loaded TLS server config");
        Ok(Arc::new(config))
    }

    pub async fn load_client_config(&self) -> Result<Arc<ClientConfig>> {
        self.ensure_certificate().await?;

        let cert_pem = fs::read(&self.cert_path).await?;

        let certs = rustls_pemfile::certs(&mut cert_pem.as_slice())
            .collect::<Result<Vec<_>, _>>()?;

        let mut root_store = rustls::RootCertStore::empty();
        for cert in certs {
            root_store.add(cert)?;
        }

        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        debug!("Loaded TLS client config");
        Ok(Arc::new(config))
    }
}
