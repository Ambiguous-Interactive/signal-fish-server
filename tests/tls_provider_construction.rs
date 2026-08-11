#![cfg(feature = "tls")]

use signal_fish_server::config::{ClientAuthMode, TlsServerConfig};
use signal_fish_server::security::build_rustls_config;

#[test]
fn m_tls_config_uses_an_explicit_crypto_provider() {
    let workdir = tempfile::tempdir().expect("create TLS fixture directory");
    let cert_path = workdir.path().join("cert.pem");
    let key_path = workdir.path().join("key.pem");
    std::fs::write(&cert_path, include_bytes!("fixtures/tls/cert.pem"))
        .expect("write certificate fixture");
    std::fs::write(&key_path, include_bytes!("fixtures/tls/key.pem"))
        .expect("write private-key fixture");
    let tls = TlsServerConfig {
        enabled: true,
        certificate_path: Some(cert_path.to_string_lossy().into_owned()),
        private_key_path: Some(key_path.to_string_lossy().into_owned()),
        client_ca_cert_path: Some(cert_path.to_string_lossy().into_owned()),
        client_auth: ClientAuthMode::Require,
    };

    build_rustls_config(&tls)
        .expect("explicit ring selection must work when the graph enables multiple providers");
}
