use std::sync::Arc;

#[cfg(feature = "tls")]
use std::{fs, io};

#[cfg(feature = "tls")]
use anyhow::{anyhow, Context, Result};

#[cfg(feature = "tls")]
use axum::{middleware::AddExtension, Extension};

#[cfg(feature = "tls")]
use axum_server::{accept::Accept, tls_rustls::RustlsConfig};

#[cfg(feature = "tls")]
use futures_util::future::BoxFuture;

#[cfg(feature = "tls")]
use rustls::{
    crypto::CryptoProvider,
    server::{danger::ClientCertVerifier, WebPkiClientVerifier},
    RootCertStore, ServerConfig as RustlsServerConfig,
};

#[cfg(feature = "tls")]
use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};

#[cfg(feature = "tls")]
use crate::config::{ClientAuthMode, TlsServerConfig};

#[cfg(feature = "tls")]
use sha2::{Digest, Sha256};

#[cfg(feature = "tls")]
use tokio::net::TcpStream;

#[cfg(feature = "tls")]
use tokio_rustls::server::TlsStream;

#[cfg(feature = "tls")]
use tower::Layer;

/// Header names an embedding layer may map to a SHA-256 certificate
/// fingerprint only after it has authenticated and stripped client-supplied
/// forwarding headers. Every entry must carry the lowercase hex digest itself,
/// never an encoded certificate. The built-in listener deliberately does not
/// consume these headers.
pub const CLIENT_FINGERPRINT_HEADER_CANDIDATES: &[&str] = &[
    "x-signalfish-client-cert-sha256",
    "x-forwarded-client-cert-sha256",
];

/// Verified client certificate fingerprint metadata propagated through request extensions.
#[derive(Debug, Clone)]
pub struct ClientCertificateFingerprint {
    /// Lowercase hexadecimal SHA-256 digest of the authenticated leaf certificate DER.
    pub fingerprint: Arc<str>,
    /// Origin of the verified value. Kept for source compatibility with embedded listeners.
    /// The built-in listener always uses `rustls-peer-certificate`, never an HTTP header.
    pub source_header: &'static str,
}

/// TLS connection metadata installed by the built-in listener after the rustls handshake.
///
/// `None` is meaningful for optional mTLS: the TLS connection is valid, but the client did not
/// present a certificate. The WebSocket token-binding policy decides whether that is acceptable.
#[cfg(feature = "tls")]
#[derive(Debug, Clone, Default)]
pub struct VerifiedClientCertificate(pub Option<ClientCertificateFingerprint>);

/// Acceptor wrapper that derives request identity exclusively from rustls's authenticated peer.
#[cfg(feature = "tls")]
#[derive(Debug, Clone)]
pub struct VerifiedClientCertificateAcceptor<A> {
    inner: A,
}

#[cfg(feature = "tls")]
impl<A> VerifiedClientCertificateAcceptor<A> {
    pub fn new(inner: A) -> Self {
        Self { inner }
    }
}

#[cfg(feature = "tls")]
impl<A, S> Accept<TcpStream, S> for VerifiedClientCertificateAcceptor<A>
where
    A: Accept<TcpStream, S, Stream = TlsStream<TcpStream>> + Clone + Send + Sync + 'static,
    A::Future: Send + 'static,
    A::Service: Send + 'static,
    S: Send + 'static,
{
    type Stream = TlsStream<TcpStream>;
    type Service = AddExtension<A::Service, VerifiedClientCertificate>;
    type Future = BoxFuture<'static, io::Result<(Self::Stream, Self::Service)>>;

    fn accept(&self, stream: TcpStream, service: S) -> Self::Future {
        let inner = self.inner.clone();

        Box::pin(async move {
            let (stream, service) = inner.accept(stream, service).await?;
            let verified = stream
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|certificates| certificates.first())
                .and_then(client_certificate_fingerprint);
            let service = Extension(VerifiedClientCertificate(verified)).layer(service);
            Ok((stream, service))
        })
    }
}

#[cfg(feature = "tls")]
fn client_certificate_fingerprint(
    certificate: &CertificateDer<'_>,
) -> Option<ClientCertificateFingerprint> {
    let digest = Sha256::digest(certificate.as_ref());
    let mut encoded = String::with_capacity(digest.len().saturating_mul(2));
    for byte in digest {
        encoded.push(char::from_digit(u32::from(byte >> 4), 16)?);
        encoded.push(char::from_digit(u32::from(byte & 0x0f), 16)?);
    }

    Some(ClientCertificateFingerprint {
        fingerprint: Arc::from(encoded),
        source_header: "rustls-peer-certificate",
    })
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
    // Select our provider explicitly. Dev dependencies and downstream
    // embedders may enable another rustls provider in the unified graph; the
    // implicit builder panics when it cannot infer one global default.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = build_client_verifier(tls, Arc::clone(&provider))?;

    let mut config = RustlsServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("failed to configure safe default TLS protocol versions")?
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
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&data)
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

    PrivateKeyDer::from_pem_slice(&key_bytes).with_context(|| {
        format!("no supported private key (pkcs8/pkcs1/sec1) was found in security.transport.tls.private_key_path ({key_path})")
    })
}

#[cfg(feature = "tls")]
fn build_client_verifier(
    tls: &TlsServerConfig,
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ClientCertVerifier>> {
    if matches!(tls.client_auth, ClientAuthMode::None) {
        return Ok(WebPkiClientVerifier::no_client_auth());
    }

    let ca_path = tls.client_ca_cert_path.as_ref().ok_or_else(|| {
        anyhow!(
            "security.transport.tls.client_ca_cert_path must be set when client_auth is {}",
            tls.client_auth
        )
    })?;
    let ca_bytes = fs::read(ca_path)
        .with_context(|| format!("failed to read client CA bundle at {ca_path}"))?;
    let mut store = RootCertStore::empty();
    let mut loaded = 0usize;
    for cert in CertificateDer::pem_slice_iter(&ca_bytes) {
        let cert = cert.with_context(|| {
            format!("failed to parse a certificate from {ca_path} for client auth")
        })?;
        store
            .add(cert)
            .map_err(|err| anyhow!("invalid client CA certificate in {ca_path}: {err}"))?;
        loaded = loaded.saturating_add(1);
    }

    if loaded == 0 {
        anyhow::bail!(
            "no certificates were loaded from security.transport.tls.client_ca_cert_path ({ca_path})"
        );
    }

    let builder = WebPkiClientVerifier::builder_with_provider(Arc::new(store), provider);
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

#[cfg(all(test, feature = "tls"))]
mod tests {
    use super::client_certificate_fingerprint;
    use rustls_pki_types::CertificateDer;

    #[test]
    fn certificate_fingerprint_is_lowercase_sha256_of_exact_der() {
        let certificate = CertificateDer::from(b"authenticated-leaf".to_vec());
        let fingerprint = client_certificate_fingerprint(&certificate);

        assert_eq!(
            fingerprint
                .as_ref()
                .map(|fingerprint| fingerprint.fingerprint.as_ref()),
            Some("8dd83ea96bac3f14faf9bf8815f141245166897c602eadbe5142f848521d3217")
        );
        assert_eq!(
            fingerprint.map(|fingerprint| fingerprint.source_header),
            Some("rustls-peer-certificate")
        );
    }
}
