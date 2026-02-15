use std::sync::Arc;

#[cfg(feature = "tls")]
use std::fs;

#[cfg(feature = "tls")]
use anyhow::{anyhow, Context, Result};

#[cfg(feature = "tls")]
use axum_server::tls_rustls::RustlsConfig;

#[cfg(feature = "tls")]
use rustls::{
    server::{danger::ClientCertVerifier, WebPkiClientVerifier},
    RootCertStore, ServerConfig as RustlsServerConfig,
};

#[cfg(feature = "tls")]
use rustls_pemfile::{certs, read_one, Item};

#[cfg(feature = "tls")]
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

#[cfg(feature = "tls")]
use crate::config::{ClientAuthMode, TlsServerConfig};

/// Header names that may carry a precomputed client certificate SHA-256 fingerprint.
pub const CLIENT_FINGERPRINT_HEADER_CANDIDATES: &[&str] = &[
    "x-signalfish-client-cert-sha256",
    "x-forwarded-client-cert-sha256",
    "x-amzn-mtls-clientcert",
];

/// Captured client certificate fingerprint metadata propagated through request extensions.
#[derive(Debug, Clone)]
pub struct ClientCertificateFingerprint {
    pub fingerprint: Arc<str>,
    pub source_header: &'static str,
}

#[cfg(feature = "tls")]
/// Build an [`axum_server`] TLS configuration based on the user-provided config.
pub fn build_rustls_config(tls: &TlsServerConfig) -> Result<RustlsConfig> {
    let server = Arc::new(build_server_config(tls)?);
    Ok(RustlsConfig::from_config(server))
}

#[cfg(feature = "tls")]
fn build_server_config(tls: &TlsServerConfig) -> Result<RustlsServerConfig> {
    let cert_chain = load_cert_chain(tls)?;
    let private_key = load_private_key(tls)?;
    let verifier = build_client_verifier(tls)?;

    let mut config = RustlsServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(cert_chain, private_key)
        .map_err(|err| anyhow!("invalid TLS certificate/private key pair: {err}"))?;

    // HTTP/1.1 + HTTP/2 are enabled so reverse proxies can continue to negotiate either protocol.
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    Ok(config)
}

#[cfg(feature = "tls")]
fn load_cert_chain(tls: &TlsServerConfig) -> Result<Vec<CertificateDer<'static>>> {
    let cert_path = tls
        .certificate_path
        .as_ref()
        .ok_or_else(|| anyhow!("security.transport.tls.certificate_path must be set"))?;
    let data = fs::read(cert_path)
        .with_context(|| format!("failed to read TLS certificate chain at {cert_path}"))?;
    let mut reader = data.as_slice();
    let certs: Vec<CertificateDer<'static>> = certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to parse TLS certificate chain at {cert_path}"))?;

    if certs.is_empty() {
        anyhow::bail!(
            "no certificates were found in security.transport.tls.certificate_path ({cert_path})"
        );
    }

    Ok(certs)
}

#[cfg(feature = "tls")]
fn load_private_key(tls: &TlsServerConfig) -> Result<PrivateKeyDer<'static>> {
    let key_path = tls
        .private_key_path
        .as_ref()
        .ok_or_else(|| anyhow!("security.transport.tls.private_key_path must be set"))?;
    let key_bytes = fs::read(key_path)
        .with_context(|| format!("failed to read TLS private key at {key_path}"))?;

    let mut reader = key_bytes.as_slice();
    while let Some(item) = read_one(&mut reader)
        .with_context(|| format!("failed to parse PEM entry inside TLS private key ({key_path})"))?
    {
        let der: PrivateKeyDer<'static> = match item {
            Item::Pkcs8Key(key) => key.into(),
            Item::Pkcs1Key(key) => key.into(),
            Item::Sec1Key(key) => key.into(),
            _ => continue,
        };
        return Ok(der);
    }

    anyhow::bail!(
        "no supported private key (pkcs8/pkcs1/sec1) was found in security.transport.tls.private_key_path ({key_path})"
    );
}

#[cfg(feature = "tls")]
fn build_client_verifier(tls: &TlsServerConfig) -> Result<Arc<dyn ClientCertVerifier>> {
    if matches!(tls.client_auth, ClientAuthMode::None) {
        return Ok(WebPkiClientVerifier::no_client_auth());
    }

    let ca_path = tls.client_ca_cert_path.as_ref().ok_or_else(|| {
        anyhow!(
            "security.transport.tls.client_ca_cert_path must be set when client_auth is {:?}",
            tls.client_auth
        )
    })?;
    let ca_bytes = fs::read(ca_path)
        .with_context(|| format!("failed to read client CA bundle at {ca_path}"))?;
    let mut reader = ca_bytes.as_slice();
    let mut store = RootCertStore::empty();
    let mut loaded = 0usize;
    for cert in certs(&mut reader) {
        let cert = cert.with_context(|| {
            format!("failed to parse a certificate from {ca_path} for client auth")
        })?;
        store
            .add(cert)
            .map_err(|err| anyhow!("invalid client CA certificate in {ca_path}: {err}"))?;
        loaded += 1;
    }

    if loaded == 0 {
        anyhow::bail!(
            "no certificates were loaded from security.transport.tls.client_ca_cert_path ({ca_path})"
        );
    }

    let builder = WebPkiClientVerifier::builder(Arc::new(store));
    let builder = if matches!(tls.client_auth, ClientAuthMode::Optional) {
        builder.allow_unauthenticated()
    } else {
        builder
    };
    let verifier = builder
        .build()
        .map_err(|err| anyhow!("failed to initialize client certificate verifier: {err}"))?;

    Ok(verifier)
}
