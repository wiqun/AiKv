//! Example demonstrating a 3-node AiKv cluster startup
//!
//! This example shows how to create and bootstrap a Multi-Raft cluster
//! with 3 nodes using AiDb v0.4.0's cluster capabilities.
//!
//! # Running the Example
//!
//! Run in separate terminals:
//! ```bash
//! # Terminal 1 (bootstrap node)
//! cargo run --example cluster_startup -- --node-id 1 --bootstrap
//!
//! # Terminal 2
//! cargo run --example cluster_startup -- --node-id 2 --join 127.0.0.1:16379
//!
//! # Terminal 3
//! cargo run --example cluster_startup -- --node-id 3 --join 127.0.0.1:16379
//! ```

use aikv::cluster::{ClusterConfig, ClusterNode, SlotRouter};
use std::env;
use std::path::PathBuf;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .with_env_filter("info")
        .init();

    // Parse command line arguments
    let args: Vec<String> = env::args().collect();
    let node_id = parse_node_id(&args).unwrap_or(1);
    let bootstrap = args.contains(&"--bootstrap".to_string());
    let join_addr = parse_join_addr(&args);

    info!("Starting AiKv cluster node {}", node_id);

    // Create data directory
    let data_dir = PathBuf::from(format!("/tmp/aikv-cluster/node{}", node_id));

    // Configure the cluster node
    let port = 6378 + node_id as u16;
    let cluster_port = 16378 + node_id as u16;
    let bind_addr = format!("127.0.0.1:{}", port);

    let config = ClusterConfig::new(node_id, &bind_addr, &data_dir).with_members(vec![
        (1, "127.0.0.1:16379".to_string()),
        (2, "127.0.0.1:16380".to_string()),
        (3, "127.0.0.1:16381".to_string()),
    ]);

    info!(
        "Node {} configured: bind={}, cluster_port={}",
        node_id, bind_addr, cluster_port
    );

    // Create the cluster node
    let node = ClusterNode::new(config).await?;

    // Bootstrap or join the cluster
    if bootstrap {
        info!("Bootstrapping new cluster from node {}", node_id);
        node.bootstrap().await?;
        info!("Cluster bootstrapped successfully!");
    } else if let Some(addr) = join_addr {
        info!("Joining existing cluster via {}", addr);
        match node.join(&addr).await {
            Ok(_) => info!("Successfully joined cluster"),
            Err(e) => {
                info!("Join not yet implemented: {}", e);
                info!(
                    "In production, this would connect to {} and sync state",
                    addr
                );
            }
        }
    } else {
        info!("No --bootstrap or --join specified");
        info!("Use --bootstrap on first node or --join <addr> on others");
    }

    // Display cluster state
    let state = node.state().await;
    info!(
        "Cluster state: role={}, cluster_ok={}",
        state.role, state.cluster_ok
    );

    // Demonstrate slot routing
    demonstrate_slot_routing();

    // Keep running (in a real server, we'd handle connections here)
    info!("Node {} running. Press Ctrl+C to stop.", node_id);

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    info!("Shutting down node {}", node_id);

    Ok(())
}

fn parse_node_id(args: &[String]) -> Option<u64> {
    for (i, arg) in args.iter().enumerate() {
        if arg == "--node-id" && i + 1 < args.len() {
            return args[i + 1].parse().ok();
        }
    }
    None
}

fn parse_join_addr(args: &[String]) -> Option<String> {
    for (i, arg) in args.iter().enumerate() {
        if arg == "--join" && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }
    }
    None
}

fn demonstrate_slot_routing() {
    let router = SlotRouter::new();

    info!("=== Slot Routing Demonstration ===");

    // Show slot calculations for various keys
    let keys = [
        "foo", "bar", "hello", "user:1", "user:2", "{user}:1", "{user}:2",
    ];

    for key in keys {
        let slot = router.slot_for_key(key);
        info!("Key '{}' -> Slot {}", key, slot);
    }

    // Demonstrate hash tags
    info!("\n=== Hash Tag Example ===");
    info!("Keys with same hash tag go to same slot:");

    let tagged_keys = [
        (
            "order:{customer1}:items",
            router.slot_for_key("order:{customer1}:items"),
        ),
        (
            "order:{customer1}:total",
            router.slot_for_key("order:{customer1}:total"),
        ),
        ("{customer1}", router.slot_for_key("{customer1}")),
    ];

    for (key, slot) in tagged_keys {
        info!("Key '{}' -> Slot {}", key, slot);
    }

    info!("\n=== Slot Distribution ===");
    info!("Total slots: 16384");
    info!("Slots per group (3 groups): ~5461");
    info!("Group 1: slots [0, 5461)");
    info!("Group 2: slots [5461, 10923)");
    info!("Group 3: slots [10923, 16384)");
}
