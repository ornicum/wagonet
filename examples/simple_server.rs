//! Simple TCP server example
//!
//! Run with: `cargo run --example simple_server`
//! Then run simple_client in another terminal
//
use tokio::net::TcpListener;
use wagonet::ServerTL;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind("127.0.0.1:8080").await?;
    println!("Server listening on 127.0.0.1:8080");

    loop {
        let (mut stream, addr) = listener.accept().await?;
        println!("Client connected: {}", addr);

        let (rd, wr) = stream.split();
        let mut server = ServerTL::new(rd, wr);

        loop {
            match server.read_command().await {
                Ok((cmd, size)) => {
                    println!("Received command: {}, data size: {}", cmd, size);

                    let data = match server.receive_data(size).await {
                        Ok(d) => d,
                        Err(e) => {
                            eprintln!("Receive error: {}", e);
                            break;
                        }
                    };
                    println!("Received data: {:?}", String::from_utf8_lossy(&data));

                    let response = format!("Echo: {}", String::from_utf8_lossy(&data));
                    if let Err(e) = server.send_data(0, Some(response.as_bytes())).await {
                        eprintln!("Send error: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("Client disconnected or error: {}", e);
                    break;
                }
            }
        }
    }
}
