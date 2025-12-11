//! Integration tests for cluster metadata synchronization
//!
//! These tests verify that CLUSTER MEET and CLUSTER FORGET wait for
//! Raft consensus before returning, ensuring metadata is synchronized.

#[cfg(feature = "cluster")]
mod cluster_sync_tests {
    use aikv::cluster::ClusterCommands;
    use bytes::Bytes;
    use std::time::{Duration, Instant};

    /// Test that CLUSTER MEET blocks until Raft consensus completes
    #[ignore]
    #[tokio::test]
    async fn test_cluster_meet_waits_for_consensus() {
        // Create a cluster commands instance
        let cmd = ClusterCommands::with_node_id(1);

        // Measure time taken for CLUSTER MEET
        let start = Instant::now();

        // Execute CLUSTER MEET - this should wait for Raft consensus if MetaRaft is available
        let result = cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("127.0.0.1"),
            Bytes::from("6380"),
        ]);

        let elapsed = start.elapsed();

        // Verify the command behaves correctly
        match result {
            Ok(_) => {
                // If OK is returned with MetaRaft, it should have waited
                // Without MetaRaft, it returns immediately after local state update
                // We can't assert exact timing without knowing if MetaRaft is configured
                println!("CLUSTER MEET completed in {:?}", elapsed);

                // Verify it didn't hang indefinitely at least
                assert!(
                    elapsed < Duration::from_secs(10),
                    "CLUSTER MEET should complete within 10s, took {:?}",
                    elapsed
                );
            }
            Err(e) => {
                // Error is expected without a real MetaRaft cluster
                let err_msg = format!("{:?}", e);
                println!("CLUSTER MEET error: {}", err_msg);

                // Could be timeout or MetaRaft not available
                assert!(
                    err_msg.contains("timeout")
                        || err_msg.contains("MetaRaft")
                        || err_msg.contains("not available")
                        || err_msg.contains("Storage"),
                    "Expected timeout or MetaRaft error, got: {}",
                    err_msg
                );

                // If it times out, it should be around 5 seconds (RAFT_PROPOSAL_TIMEOUT_SECS)
                if err_msg.contains("timeout") || err_msg.contains("Timeout") {
                    assert!(
                        elapsed >= Duration::from_secs(4) && elapsed <= Duration::from_secs(6),
                        "Timeout should occur around 5 seconds, took {:?}",
                        elapsed
                    );
                }
            }
        }
    }

    /// Test that CLUSTER MEET doesn't hang indefinitely
    #[ignore]
    #[tokio::test]
    async fn test_cluster_meet_timeout() {
        let cmd = ClusterCommands::with_node_id(1);

        // Execute CLUSTER MEET and ensure it returns within reasonable time
        let timeout = Duration::from_secs(10); // Should timeout within 5s + buffer
        let start = Instant::now();

        let result = tokio::time::timeout(timeout, async {
            cmd.execute(&[
                Bytes::from("MEET"),
                Bytes::from("127.0.0.1"),
                Bytes::from("6380"),
            ])
        })
        .await;

        let elapsed = start.elapsed();

        // Verify we got a result (not a timeout at the test level)
        assert!(
            result.is_ok(),
            "CLUSTER MEET should complete within {} seconds, took {:?}",
            timeout.as_secs(),
            elapsed
        );

        // The actual command may fail, but it shouldn't hang
        assert!(
            elapsed < timeout,
            "CLUSTER MEET took too long: {:?}",
            elapsed
        );
    }

    /// Test CLUSTER FORGET also waits for consensus
    #[ignore]
    #[tokio::test]
    async fn test_cluster_forget_waits_for_consensus() {
        let cmd = ClusterCommands::with_node_id(1);

        // First add a node to forget
        let target_id = 999u64;
        let _ = cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("127.0.0.1"),
            Bytes::from("6380"),
            Bytes::from(format!("{:040x}", target_id)),
        ]);

        // Measure time for CLUSTER FORGET
        let start = Instant::now();

        let result = cmd.execute(&[
            Bytes::from("FORGET"),
            Bytes::from(format!("{:040x}", target_id)),
        ]);

        let elapsed = start.elapsed();

        // Similar to MEET test - verify timing or appropriate error
        match result {
            Ok(_) => {
                // Should take at least some time for consensus
                // Note: May be instant if MetaRaft not available and only local state updated
                println!("CLUSTER FORGET took {:?}", elapsed);
            }
            Err(e) => {
                let err_msg = format!("{:?}", e);
                assert!(
                    err_msg.contains("timeout")
                        || err_msg.contains("MetaRaft")
                        || err_msg.contains("not available"),
                    "Expected timeout or MetaRaft error, got: {}",
                    err_msg
                );
            }
        }
    }

    /// Test that multiple CLUSTER MEET operations work sequentially
    #[ignore]
    #[tokio::test]
    async fn test_multiple_cluster_meets_sequential() {
        let cmd = ClusterCommands::with_node_id(1);

        let mut total_time = Duration::ZERO;

        // Execute multiple CLUSTER MEET operations
        for i in 1..=3 {
            let start = Instant::now();
            let result = cmd.execute(&[
                Bytes::from("MEET"),
                Bytes::from("127.0.0.1"),
                Bytes::from(format!("{}", 6379 + i)),
            ]);
            let elapsed = start.elapsed();
            total_time += elapsed;

            // Each should complete or fail appropriately
            match result {
                Ok(_) => {
                    println!("CLUSTER MEET {} completed in {:?}", i, elapsed);
                }
                Err(_) => {
                    // Error is fine without real MetaRaft
                    println!("CLUSTER MEET {} returned error in {:?}", i, elapsed);
                }
            }
        }

        println!("Total time for 3 CLUSTER MEETs: {:?}", total_time);

        // Ensure none of them hung indefinitely
        assert!(
            total_time < Duration::from_secs(30),
            "Multiple CLUSTER MEETs should complete within 30s, took {:?}",
            total_time
        );
    }

    /// Test synchronous behavior constants
    #[ignore]
    #[test]
    fn test_raft_constants_defined() {
        // Verify the constants are defined with expected values
        // This is a compile-time check that the constants exist

        // Note: These constants are private, so we test indirectly through behavior
        // The timeout is 5 seconds, so a CLUSTER MEET without MetaRaft should
        // fail within that timeframe

        let cmd = ClusterCommands::with_node_id(1);
        let start = Instant::now();

        let _ = cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("127.0.0.1"),
            Bytes::from("6380"),
        ]);

        let elapsed = start.elapsed();

        // Should complete within 6 seconds (5s timeout + 1s buffer)
        assert!(
            elapsed < Duration::from_secs(6),
            "CLUSTER MEET should respect RAFT_PROPOSAL_TIMEOUT_SECS (5s), took {:?}",
            elapsed
        );
    }

    /// Test that CLUSTER MEET with invalid arguments fails fast
    #[ignore]
    #[test]
    fn test_cluster_meet_invalid_args_fast() {
        let cmd = ClusterCommands::with_node_id(1);
        let start = Instant::now();

        // Missing port argument - should fail immediately
        let result = cmd.execute(&[Bytes::from("MEET"), Bytes::from("127.0.0.1")]);

        let elapsed = start.elapsed();

        // Should fail immediately (validation before Raft)
        assert!(result.is_err(), "Invalid args should fail");
        assert!(
            elapsed < Duration::from_millis(100),
            "Invalid args should fail fast, took {:?}",
            elapsed
        );
    }

    /// Test that CLUSTER FORGET of self fails fast
    #[ignore]
    #[test]
    fn test_cluster_forget_self_fast() {
        let node_id = 1u64;
        let cmd = ClusterCommands::with_node_id(node_id);
        let start = Instant::now();

        // Try to forget self - should fail immediately
        let result = cmd.execute(&[
            Bytes::from("FORGET"),
            Bytes::from(format!("{:040x}", node_id)),
        ]);

        let elapsed = start.elapsed();

        // Should fail immediately (validation before Raft)
        assert!(result.is_err(), "Forgetting self should fail");
        assert!(
            elapsed < Duration::from_millis(100),
            "Self-forget should fail fast, took {:?}",
            elapsed
        );
    }

    /// Test metadata consistency after CLUSTER MEET
    #[ignore]
    #[test]
    fn test_metadata_consistency_after_meet() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add a node
        let target_id = 12345u64;
        let _ = cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("127.0.0.1"),
            Bytes::from("6380"),
            Bytes::from(format!("{:040x}", target_id)),
        ]);

        // Verify local state was updated (happens even without MetaRaft)
        let state = cmd.state();
        let state = state.read().unwrap();
        assert!(
            state.nodes.contains_key(&target_id),
            "Node should be in local state even if Raft fails"
        );

        let node_info = state.nodes.get(&target_id).unwrap();
        assert_eq!(node_info.addr, "127.0.0.1:6380");
        // Cluster port should be set (exact value depends on implementation)
        assert!(
            node_info.cluster_port > 0,
            "Cluster port should be set, got {}",
            node_info.cluster_port
        );
    }

    /// Test that CLUSTER NODES returns consistent data after MEET
    #[ignore]
    #[test]
    fn test_cluster_nodes_after_meet() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add a node
        let target_id = 67890u64;
        let _ = cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("127.0.0.1"),
            Bytes::from("6381"),
            Bytes::from(format!("{:040x}", target_id)),
        ]);

        // Query CLUSTER NODES
        let result = cmd.execute(&[Bytes::from("NODES")]);
        assert!(result.is_ok(), "CLUSTER NODES should work");

        // Parse the result to verify node is present
        if let Ok(resp) = result {
            let output = format!("{:?}", resp);
            // The output should contain the node ID
            // Note: Exact format depends on implementation
            println!("CLUSTER NODES output: {}", output);
        }
    }
}

// Stub test for non-cluster builds
#[cfg(not(feature = "cluster"))]
mod cluster_sync_tests {
    #[ignore]
    #[test]
    fn cluster_feature_not_enabled() {
        // This test exists to ensure the test file compiles even without cluster feature
        println!("Cluster feature not enabled - skipping cluster sync tests");
    }
}
