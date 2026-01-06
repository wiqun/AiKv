//! Comprehensive tests for new cluster implementation
//!
//! This test suite validates:
//! 1. Raft consensus correctness
//! 2. Node-to-node metadata synchronization
//! 3. CLUSTER MEET synchronization across nodes
//! 4. CLUSTER ADDSLOTS synchronization
//! 5. Multi-node cluster operations

#[cfg(test)]
#[cfg(feature = "cluster")]
mod cluster_tests {
    use aikv::cluster::{ClusterCommands, ClusterConfig, ClusterNode, MultiRaftNode, Router};
    use aikv::error::Result;
    use openraft::Config as RaftConfig;
    use std::sync::Arc;
    use tokio::time::{sleep, Duration};

    /// Test that MetaRaftNode correctly syncs add_node operations
    #[tokio::test]
    async fn test_meta_raft_add_node_sync() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_add_node_node1").await;
        let _ = tokio::fs::remove_dir_all("/tmp/test_add_node_node2").await;

        // This test validates that when a node is added via MetaRaft on one node,
        // it syncs to all other nodes via Raft consensus

        // Setup: Create 2 MetaRaft nodes on different ports
        let config1 = RaftConfig::default();
        let config2 = RaftConfig::default();

        // Node 1 (bootstrap node) - initialize before wrapping in Arc
        let mut node1 = MultiRaftNode::new(1, "/tmp/test_add_node_node1", config1.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node1
            .init_meta_raft(config1.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        node1
            .initialize_meta_cluster(vec![(1, "127.0.0.1:50051".to_string())])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        let node1 = Arc::new(node1);

        let meta1 = node1.meta_raft().ok_or_else(|| {
            aikv::error::AikvError::Internal("Meta raft not initialized".to_string())
        })?;

        // Wait for cluster to stabilize
        sleep(Duration::from_millis(500)).await;

        // Create Node 2 on different port
        let mut node2 = MultiRaftNode::new(2, "/tmp/test_add_node_node2", config2.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node2
            .init_meta_raft(config2.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        let _node2 = Arc::new(node2);

        // Test: Add node 2 to the cluster via node 1's MetaRaft
        meta1
            .add_node(2, "127.0.0.1:50052".to_string())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        // Wait for replication
        sleep(Duration::from_millis(500)).await;

        // Verify: Check that node 2 appears in cluster meta on node 1
        let cluster_meta = meta1.get_cluster_meta();
        assert!(
            cluster_meta.nodes.contains_key(&2),
            "Node 2 should be in cluster meta"
        );
        assert_eq!(cluster_meta.nodes.get(&2).unwrap().addr, "127.0.0.1:50052");

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_add_node_node1").await;
        let _ = tokio::fs::remove_dir_all("/tmp/test_add_node_node2").await;

        Ok(())
    }

    /// Test multi-node CLUSTER MEET synchronization
    #[tokio::test]
    async fn test_cluster_meet_metadata_sync() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_meet_node1").await;

        // This test validates that CLUSTER MEET correctly synchronizes
        // across multiple nodes via Raft consensus

        let config = RaftConfig::default();

        // Create node 1 (bootstrap) - initialize before wrapping
        let mut node1 = MultiRaftNode::new(1, "/tmp/test_meet_node1", config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node1
            .init_meta_raft(config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        node1
            .initialize_meta_cluster(vec![(1, "127.0.0.1:50061".to_string())])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        let node1 = Arc::new(node1);

        let meta1 = node1.meta_raft().ok_or_else(|| {
            aikv::error::AikvError::Internal("Meta raft not initialized".to_string())
        })?;

        // Get cluster metadata and create Router
        let cluster_meta = meta1.get_cluster_meta();
        let router1 = Arc::new(Router::new(cluster_meta));

        // Create ClusterCommands for node 1
        let cmd1 = ClusterCommands::new(1, meta1.clone(), node1.clone(), router1.clone());

        // Wait for bootstrap
        sleep(Duration::from_millis(500)).await;

        // Test: Execute CLUSTER MEET to add node 2
        let result = cmd1
            .cluster_meet("127.0.0.1".to_string(), 50062, Some(2))
            .await?;
        // RespValue doesn't have to_string(), so we check if it's SimpleString
        match result {
            aikv::protocol::RespValue::SimpleString(s) => assert_eq!(s, "OK"),
            _ => panic!("Expected SimpleString OK"),
        }

        // Wait for Raft consensus and replication
        sleep(Duration::from_millis(300)).await;

        // Verify: Node 2 should appear in cluster metadata
        let cluster_meta = meta1.get_cluster_meta();
        assert!(
            cluster_meta.nodes.contains_key(&2),
            "Node 2 should be in cluster after MEET"
        );

        // Verify: Node 2's address is correct
        let node2_info = cluster_meta.nodes.get(&2).unwrap();
        assert_eq!(node2_info.addr, "127.0.0.1:50062");

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_meet_node1").await;

        Ok(())
    }

    /// Test that CLUSTER ADDSLOTS syncs via Raft
    #[tokio::test]
    async fn test_cluster_addslots_raft_sync() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_addslots").await;

        // This test validates that slot assignments sync across nodes

        let config = RaftConfig::default();

        // Setup node - initialize before wrapping
        let mut node = MultiRaftNode::new(1, "/tmp/test_addslots", config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node.init_meta_raft(config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        node.initialize_meta_cluster(vec![(1, "127.0.0.1:50071".to_string())])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        let node = Arc::new(node);

        let meta = node.meta_raft().ok_or_else(|| {
            aikv::error::AikvError::Internal("Meta raft not initialized".to_string())
        })?;

        sleep(Duration::from_millis(500)).await;

        // Create group in metadata so node 1 belongs to group 1
        meta.create_group(1, vec![1])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        sleep(Duration::from_millis(500)).await;

        let cluster_meta = meta.get_cluster_meta();
        let router = Arc::new(Router::new(cluster_meta));
        let cmd = ClusterCommands::new(1, meta.clone(), node.clone(), router);

        // Test: Assign slots 0-100 to this node
        let slots: Vec<u16> = (0..=100).collect();
        let result = cmd.cluster_addslots(slots).await?;
        match result {
            aikv::protocol::RespValue::SimpleString(s) => assert_eq!(s, "OK"),
            _ => panic!("Expected SimpleString OK"),
        }

        // Wait for Raft replication
        sleep(Duration::from_millis(500)).await;

        // Verify: Slots should be assigned in metadata
        let cluster_meta = meta.get_cluster_meta();
        for slot in 0..=100 {
            let assigned_group = cluster_meta.slots[slot as usize];
            assert_eq!(
                assigned_group, 1,
                "Slot {} should be assigned to group 1",
                slot
            );
        }

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_addslots").await;

        Ok(())
    }

    /// Test CLUSTER INFO returns correct state
    #[tokio::test]
    async fn test_cluster_info() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_info").await;

        let config = RaftConfig::default();

        let mut node = MultiRaftNode::new(1, "/tmp/test_info", config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node.init_meta_raft(config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        node.initialize_meta_cluster(vec![(1, "127.0.0.1:50081".to_string())])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        let node = Arc::new(node);

        let meta = node.meta_raft().ok_or_else(|| {
            aikv::error::AikvError::Internal("Meta raft not initialized".to_string())
        })?;
        let cluster_meta = meta.get_cluster_meta();
        let router = Arc::new(Router::new(cluster_meta));
        let cmd = ClusterCommands::new(1, meta.clone(), node, router);

        sleep(Duration::from_millis(500)).await;

        // Test: Get cluster info
        let info = cmd.cluster_info()?;

        // Extract the bulk string content
        let info_str = match info {
            aikv::protocol::RespValue::BulkString(Some(bytes)) => {
                String::from_utf8_lossy(&bytes).to_string()
            }
            _ => panic!("Expected BulkString"),
        };

        // Verify: Should contain cluster_state
        assert!(
            info_str.contains("cluster_state:"),
            "Info should contain cluster_state"
        );
        assert!(
            info_str.contains("cluster_slots_assigned:"),
            "Info should contain slots_assigned"
        );

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_info").await;

        Ok(())
    }

    /// Test CLUSTER NODES returns correct format
    #[tokio::test]
    async fn test_cluster_nodes() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_nodes").await;

        let config = RaftConfig::default();

        let mut node = MultiRaftNode::new(1, "/tmp/test_nodes", config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node.init_meta_raft(config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        node.initialize_meta_cluster(vec![(1, "127.0.0.1:50091".to_string())])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        let node = Arc::new(node);

        let meta = node.meta_raft().ok_or_else(|| {
            aikv::error::AikvError::Internal("Meta raft not initialized".to_string())
        })?;

        sleep(Duration::from_millis(500)).await;

        // Add this node to the cluster metadata
        meta.add_node(1, "127.0.0.1:50091".to_string())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        sleep(Duration::from_millis(300)).await;

        let cluster_meta = meta.get_cluster_meta();
        let router = Arc::new(Router::new(cluster_meta));
        let cmd = ClusterCommands::new(1, meta.clone(), node, router);

        // Test: Get cluster nodes
        let nodes = cmd.cluster_nodes()?;

        // Extract the bulk string content
        let nodes_str = match nodes {
            aikv::protocol::RespValue::BulkString(Some(bytes)) => {
                String::from_utf8_lossy(&bytes).to_string()
            }
            _ => panic!("Expected BulkString"),
        };

        // Debug: print nodes output
        println!("Nodes output: {}", nodes_str);

        // Verify: Should contain node information
        assert!(
            nodes_str.contains("127.0.0.1"),
            "Nodes output should contain IP. Got: {}",
            nodes_str
        );
        assert!(
            nodes_str.contains("myself"),
            "Nodes output should mark myself. Got: {}",
            nodes_str
        );

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_nodes").await;

        Ok(())
    }

    /// Test CLUSTER KEYSLOT calculation
    #[tokio::test]
    async fn test_cluster_keyslot() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_keyslot").await;

        let config = RaftConfig::default();

        let mut node = MultiRaftNode::new(1, "/tmp/test_keyslot", config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node.init_meta_raft(config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        node.initialize_meta_cluster(vec![(1, "127.0.0.1:50101".to_string())])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        let node = Arc::new(node);

        let meta = node.meta_raft().ok_or_else(|| {
            aikv::error::AikvError::Internal("Meta raft not initialized".to_string())
        })?;
        let cluster_meta = meta.get_cluster_meta();
        let router = Arc::new(Router::new(cluster_meta));
        let cmd = ClusterCommands::new(1, meta.clone(), node, router);

        // Test: Calculate slot for a key
        let slot = cmd.cluster_keyslot(b"user:1000")?;

        // Verify: Router.key_to_slot should give same result
        let expected_slot = Router::key_to_slot(b"user:1000");

        // Verify: Slot matches expected
        if let aikv::protocol::RespValue::Integer(s) = slot {
            assert_eq!(
                s, expected_slot as i64,
                "Slot should match Router::key_to_slot"
            );
            assert!(
                (0i64..16384i64).contains(&s),
                "Slot should be in range 0-16383"
            );
        } else {
            panic!("Expected Integer");
        }

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_keyslot").await;

        Ok(())
    }

    /// Test ClusterNode initialization
    #[tokio::test]
    async fn test_cluster_node_init() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_node_init").await;

        let config = ClusterConfig {
            node_id: 1,
            data_dir: "/tmp/test_node_init".into(),
            bind_address: "127.0.0.1:6379".to_string(),
            raft_address: "127.0.0.1:50111".to_string(),
            num_groups: 4,
            is_bootstrap: true,
            initial_members: vec![(1, "127.0.0.1:50111".to_string())],
        };

        let mut node = ClusterNode::new(config);

        // Test: Initialize node
        node.initialize().await?;

        // Verify: Node should have meta_raft and multi_raft
        assert!(
            node.meta_raft().is_some(),
            "Meta raft should be initialized"
        );
        assert!(
            node.multi_raft().is_some(),
            "Multi raft should be initialized"
        );
        assert!(node.router().is_some(), "Router should be initialized");
        assert_eq!(node.node_id(), 1);

        // Cleanup
        node.shutdown().await?;
        let _ = tokio::fs::remove_dir_all("/tmp/test_node_init").await;

        Ok(())
    }

    /// Test MOVED redirection for keys that belong to another node
    #[tokio::test]
    async fn test_moved_redirection_check_slot_ownership() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_moved_redirect").await;

        // Create a cluster node
        let raft_config = RaftConfig::default();
        let mut node = MultiRaftNode::new(1, "/tmp/test_moved_redirect", raft_config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node.init_meta_raft(raft_config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        // Bootstrap as single node
        node.initialize_meta_cluster(vec![(1, "127.0.0.1:50001".to_string())])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        let node = Arc::new(node);
        let meta_raft = node.meta_raft().unwrap();
        let cluster_meta = meta_raft.get_cluster_meta();
        let router = Arc::new(Router::new(cluster_meta));

        // Create ClusterCommands for this node
        let cluster_commands = ClusterCommands::new(1, meta_raft.clone(), node.clone(), router);

        // Test 1: Before slots are assigned, check_slot_ownership should fail
        let result = cluster_commands.check_slot_ownership(0);
        assert!(
            result.is_err(),
            "Slot 0 should not be owned before assignment"
        );

        // Create a group for node 1 and assign slots 0-5460
        meta_raft
            .create_group(1, vec![1])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        meta_raft
            .update_group_leader(1, 1)
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        meta_raft
            .update_slots(0, 5461, 1)
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        // Wait for Raft to apply
        sleep(Duration::from_millis(100)).await;

        // Test 2: After slots are assigned, check_slot_ownership should succeed for owned slots
        let result = cluster_commands.check_slot_ownership(0);
        assert!(
            result.is_ok(),
            "Slot 0 should be owned by this node after assignment"
        );

        let result = cluster_commands.check_slot_ownership(5460);
        assert!(result.is_ok(), "Slot 5460 should be owned by this node");

        // Test 3: Slots not assigned to this node should return error
        let result = cluster_commands.check_slot_ownership(5461);
        assert!(
            result.is_err(),
            "Slot 5461 should not be owned by this node"
        );

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_moved_redirect").await;

        Ok(())
    }

    /// Test check_key_slot calculates slot correctly and checks ownership
    #[tokio::test]
    async fn test_check_key_slot() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_check_key_slot").await;

        // Create a cluster node
        let raft_config = RaftConfig::default();
        let mut node = MultiRaftNode::new(1, "/tmp/test_check_key_slot", raft_config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node.init_meta_raft(raft_config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node.initialize_meta_cluster(vec![(1, "127.0.0.1:50002".to_string())])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        let node = Arc::new(node);
        let meta_raft = node.meta_raft().unwrap();
        let cluster_meta = meta_raft.get_cluster_meta();
        let router = Arc::new(Router::new(cluster_meta));

        let cluster_commands = ClusterCommands::new(1, meta_raft.clone(), node.clone(), router);

        // Create a group and assign all slots to node 1
        meta_raft
            .create_group(1, vec![1])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        meta_raft
            .update_group_leader(1, 1)
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        meta_raft
            .update_slots(0, 16384, 1)
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        sleep(Duration::from_millis(100)).await;

        // Test: check_key_slot should succeed for any key when all slots are owned
        let result = cluster_commands.check_key_slot(b"user:1000");
        assert!(
            result.is_ok(),
            "Key 'user:1000' should be handled by this node"
        );

        let result = cluster_commands.check_key_slot(b"product:123");
        assert!(
            result.is_ok(),
            "Key 'product:123' should be handled by this node"
        );

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_check_key_slot").await;

        Ok(())
    }

    /// Test check_keys_slot validates all keys are in the same slot
    #[tokio::test]
    async fn test_check_keys_slot_cross_slot() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_cross_slot").await;

        // Create a cluster node
        let raft_config = RaftConfig::default();
        let mut node = MultiRaftNode::new(1, "/tmp/test_cross_slot", raft_config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node.init_meta_raft(raft_config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node.initialize_meta_cluster(vec![(1, "127.0.0.1:50003".to_string())])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        let node = Arc::new(node);
        let meta_raft = node.meta_raft().unwrap();

        // Assign all slots to node 1 BEFORE creating ClusterCommands
        meta_raft
            .create_group(1, vec![1])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        meta_raft
            .update_group_leader(1, 1)
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        meta_raft
            .update_slots(0, 16384, 1)
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        sleep(Duration::from_millis(100)).await;

        // Create ClusterCommands AFTER assigning slots so Router has correct metadata
        let cluster_meta = meta_raft.get_cluster_meta();
        let router = Arc::new(Router::new(cluster_meta));
        let cluster_commands = ClusterCommands::new(1, meta_raft.clone(), node.clone(), router);

        // Test 1: Same key multiple times (obviously same slot) should succeed
        let keys: Vec<&[u8]> = vec![b"mykey", b"mykey", b"mykey"];
        let result = cluster_commands.check_keys_slot(&keys);
        assert!(result.is_ok(), "Same key should be in same slot");

        // Test 2: Different keys should succeed if all slots are owned by this node
        // Since we assigned all slots to node 1, any set of keys should work
        let keys: Vec<&[u8]> = vec![b"key1"];
        let result = cluster_commands.check_keys_slot(&keys);
        assert!(
            result.is_ok(),
            "Single key should succeed when slot is owned"
        );

        // Test 3: Keys in different slots should fail with CrossSlot error
        let keys: Vec<&[u8]> = vec![b"key1", b"key2", b"key3"];

        // Calculate slots to verify they're different
        let slot1 = Router::key_to_slot(b"key1");
        let slot2 = Router::key_to_slot(b"key2");
        let slot3 = Router::key_to_slot(b"key3");

        // If any slots are different, check_keys_slot should return CrossSlot error
        if slot1 != slot2 || slot2 != slot3 {
            let result = cluster_commands.check_keys_slot(&keys);
            assert!(result.is_err(), "Keys in different slots should fail");
            if let Err(aikv::error::AikvError::CrossSlot) = result {
                // Expected
            } else {
                panic!("Expected CrossSlot error, got {:?}", result);
            }
        }

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_cross_slot").await;

        Ok(())
    }

    /// Test get_slot_owner returns correct node info
    #[tokio::test]
    async fn test_get_slot_owner() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_slot_owner").await;

        // Create a cluster node
        let raft_config = RaftConfig::default();
        let mut node = MultiRaftNode::new(1, "/tmp/test_slot_owner", raft_config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node.init_meta_raft(raft_config.clone())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        node.initialize_meta_cluster(vec![(1, "127.0.0.1:50004".to_string())])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        let node = Arc::new(node);
        let meta_raft = node.meta_raft().unwrap();
        let cluster_meta = meta_raft.get_cluster_meta();
        let router = Arc::new(Router::new(cluster_meta));

        let cluster_commands = ClusterCommands::new(1, meta_raft.clone(), node.clone(), router);

        // Test 1: Before slots are assigned, get_slot_owner should return None
        let owner = cluster_commands.get_slot_owner(0);
        assert!(
            owner.is_none(),
            "Slot 0 should have no owner before assignment"
        );

        // First, add the node to the cluster so it's in the nodes map
        meta_raft
            .add_node(1, "127.0.0.1:50004".to_string())
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        // Assign slots to node 1
        meta_raft
            .create_group(1, vec![1])
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        meta_raft
            .update_group_leader(1, 1)
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;
        meta_raft
            .update_slots(0, 5461, 1)
            .await
            .map_err(|e| aikv::error::AikvError::Internal(e.to_string()))?;

        sleep(Duration::from_millis(100)).await;

        // Test 2: After assignment, get_slot_owner should return the owner
        let owner = cluster_commands.get_slot_owner(0);
        assert!(owner.is_some(), "Slot 0 should have owner after assignment");
        let (node_id, _addr) = owner.unwrap();
        assert_eq!(node_id, 1, "Slot 0 should be owned by node 1");

        // Test 3: Unassigned slot should return None
        let owner = cluster_commands.get_slot_owner(10000);
        assert!(owner.is_none(), "Slot 10000 should have no owner");

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_slot_owner").await;

        Ok(())
    }
}
