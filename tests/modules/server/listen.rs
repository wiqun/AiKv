//! @component aikv-server

//! Step 3: listener + accept

use super::helpers::{connect, start_server};

#[tokio::test]
async fn test_server_listen_and_accept() {
    let (addr, _handle) = start_server().await;
    let _stream = connect(addr).await;
}
