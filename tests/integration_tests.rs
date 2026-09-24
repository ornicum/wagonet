#[cfg(test)]
mod integration_tests {
    use native_tls::{Identity, TlsAcceptor as NativeTlsAcceptor};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::Notify;
    use tokio_native_tls::TlsAcceptor;

    use wagonet::client_tl::ClientTL;
    use wagonet::client_tls::ClientTLS;
    use wagonet::protocol_structs::ResponseStatus;
    use wagonet::server_tl::ServerTL;
    use wagonet::server_tls::ServerTLS;

    fn create_test_tls_acceptor() -> TlsAcceptor {
        use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};

        let mut params = CertificateParams::default();
        params.not_before = rcgen::date_time_ymd(2026, 1, 1);
        params.not_after = rcgen::date_time_ymd(2036, 1, 1);

        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "example.org");
        params.distinguished_name = dn;
        params.subject_alt_names = vec![SanType::DnsName("example.org".try_into().unwrap())];

        let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let cert = params.self_signed(&key_pair).unwrap();

        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();

        let identity = Identity::from_pkcs8(cert_pem.as_bytes(), key_pem.as_bytes()).unwrap();
        let acceptor = NativeTlsAcceptor::builder(identity).build().unwrap();

        TlsAcceptor::from(acceptor)
    }

    #[tokio::test]
    async fn test_tcp_transport_success() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let server_ready = Arc::new(Notify::new());
        let server_ready_clone = server_ready.clone();

        let server_handle = tokio::spawn(async move {
            server_ready_clone.notify_one();
            let (mut stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.split();
            let mut server = ServerTL::new(reader, writer);
            eprintln!("[server] waiting for command");
            let (command, data_size) = server.read_command().await.unwrap();
            eprintln!("[server] got command={}, data_size={}", command, data_size);
            assert_eq!(command, 42);
            assert_eq!(data_size, 12);

            eprintln!("[server] waiting for data");
            let received_data = server.receive_data(data_size).await.unwrap();
            eprintln!("[server] got data: {:?}", received_data);
            assert_eq!(received_data, b"hello server");

            let response_body = b"hello client";
            eprintln!("[server] sending response");
            server
                .send_data(ResponseStatus::Ok.into(), Some(response_body))
                .await
                .unwrap();
            eprintln!("[server] response sent");
        });

        server_ready.notified().await;

        let mut client = ClientTL::new(addr);
        let request_payload = b"hello server";

        eprintln!("[client] sending request");
        let response = client.handle_message(42, request_payload).await.unwrap();
        eprintln!("[client] got response: {:?}", response);
        assert_eq!(response, b"hello client");

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_tcp_transport_buffer_overflow() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let server_handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.split();
            let mut server = ServerTL::new(reader, writer);
            server.set_max_buffer_size(5);

            let res = server.read_command().await;
            assert!(res.is_err());
        });

        let mut client = ClientTL::new(addr);
        let large_payload = b"this payload is too long for server";

        let response_res = client.handle_message(10, large_payload).await;

        assert!(response_res.is_err());

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_tcp_transport_no_answer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server_handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.split();
            let mut server = ServerTL::new(reader, writer);
            let (_, data_size) = server.read_command().await.unwrap();
            let received_data = server.receive_data(data_size).await.unwrap();
            assert_eq!(received_data, b"fire and forget");

            server
                .send_data(ResponseStatus::Ok.into(), None)
                .await
                .unwrap();
        });

        let mut client = ClientTL::new(addr);
        let payload = b"fire and forget";

        let response = client
            .handle_message_with_no_answer(99, payload)
            .await
            .unwrap();

        assert!(response.is_empty());

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_tls_transport_success() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let acceptor = create_test_tls_acceptor();

        let server_handle = tokio::spawn({
            let acceptor = acceptor.clone();
            async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let tls_stream = acceptor.accept(&mut stream).await.unwrap();
                let mut server = ServerTLS::new(tls_stream);

                let (command, data_size) = server.read_command().await.unwrap();
                assert_eq!(command, 77);

                let received_data = server.receive_data(data_size).await.unwrap();
                assert_eq!(received_data, b"secure hello");

                server
                    .send_data(ResponseStatus::Ok.into(), Some(b"secure reply"))
                    .await
                    .unwrap();
            }
        });

        let mut client = ClientTLS::new(addr);
        client.set_accept_invalid_certs(true);
        client.set_domain("example.org".to_string());

        let response = client.handle_message(77, b"secure hello").await.unwrap();
        assert_eq!(response, b"secure reply");

        server_handle.await.unwrap();
    }
}
