use aikv::Server;
use tracing_subscriber;

#[tokio::main]
async fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    // Parse command line arguments
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:6379".to_string());

    // Create and run server
    let server = Server::new(addr);

    if let Err(e) = server.run().await {
        eprintln!("Server error: {}", e);
        std::process::exit(1);
    }
}
