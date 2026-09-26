//! TLS server example
//!
//! Run with: `cargo run --example tls_server`
//! Generates a self-signed certificate for testing.
//! Then run tls_client in another terminal.

use native_tls::{Identity, TlsAcceptor as NativeTlsAcceptor};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use tokio::net::TcpListener;
use tokio_native_tls::TlsAcceptor;
use wagonet::ServerTLS;

fn create_tls_acceptor() -> TlsAcceptor {
    let mut params = CertificateParams::default();
    params.not_before = rcgen::date_time_ymd(2024, 1, 1);
    params.not_after = rcgen::date_time_ymd(2034, 1, 1);

    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "localhost");
    params.distinguished_name = dn;
    params.subject_alt_names = vec![rcgen::SanType::DnsName("localhost".try_into().unwrap())];

    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let cert = params.self_signed(&key_pair).unwrap();

    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    let identity = Identity::from_pkcs8(cert_pem.as_bytes(), key_pem.as_bytes()).unwrap();
    let acceptor = NativeTlsAcceptor::builder(identity).build().unwrap();

    tokio_native_tls::TlsAcceptor::from(acceptor)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let tls_acceptor = create_tls_acceptor();
    let listener = TcpListener::bind("127.0.0.1:8443").await?;
    println!("TLS Server listening on 127.0.0.1:8443");
    println!(
        "Certificate: self-signed (client must use accept_invalid_certs=true or trust the cert)"
    );

    loop {
        let (stream, addr) = listener.accept().await?;
        println!("Client connected: {}", addr);

        let tls_acceptor = tls_acceptor.clone();
        tokio::spawn(async move {
            // Perform TLS handshake
            let tls_stream = match tls_acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("TLS handshake failed: {}", e);
                    return;
                }
            };
            println!("TLS handshake completed for: {}", addr);

            let mut server = ServerTLS::new(tls_stream);
            while let Some((_cmd, size)) = server.read_command().await.unwrap() {
                let data = match server.receive_data(size).await {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("Receive error: {}", e);
                        break;
                    }
                };
                println!("Received: {:?}", String::from_utf8_lossy(&data));

                let response = format!("TLS Echo: {}", String::from_utf8_lossy(&data));
                if let Err(e) = server.send_data(0, Some(response.as_bytes())).await {
                    eprintln!("Send error: {}", e);
                    break;
                }
            }
        });
    }
}