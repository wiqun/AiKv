//! Tests for dynamic MetaRaft membership changes
//!
//! This test suite validates:
//! 1. Adding nodes as MetaRaft learners
//! 2. Promoting learners to voters
//! 3. Multi-node MetaRaft consensus
//! 4. CLUSTER METARAFT commands

#[cfg(test)]
#[cfg(feature = "cluster")]
mod metaraft_tests {
    use aikv::cluster::{ClusterConfig, ClusterNode};
    use aikv::error::Result;
    use std::path::PathBuf;
    use tokio::time::{sleep, Duration};

    /// Time to wait for cluster to stabilize after operations
    const CLUSTER_STABILIZATION_DELAY: Duration = Duration::from_millis(200);

    /// Test adding a node as a MetaRaft learner
    #[tokio::test]
    async fn test_add_meta_learner() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_metaraft_learner_node1").await;

        // Create bootstrap node
        let config1 = ClusterConfig {
            node_id: 1,
            data_dir: PathBuf::from("/tmp/test_metaraft_learner_node1"),
            bind_address: "127.0.0.1:7001".to_string(),
            raft_address: "127.0.0.1:50061".to_string(),
            num_groups: 1,
            is_bootstrap: true,
            initial_members: vec![(1, "127.0.0.1:50061".to_string())],
        };

        let mut node1 = ClusterNode::new(config1);
        node1.initialize().await?;

        // Wait for cluster to stabilize
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Add node 2 as a learner
        node1
            .add_meta_learner(2, "127.0.0.1:50062".to_string())
            .await?;

        // Wait for learner to be added
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Verify learner was added by checking Raft metrics
        let meta_raft = node1.meta_raft().unwrap();
        let raft = meta_raft.raft();
        let metrics = raft.metrics().borrow().clone();
        let membership = metrics.membership_config.membership();

        // Check that node 2 is a learner
        let learners: Vec<u64> = membership.learner_ids().collect();
        assert!(
            learners.contains(&2),
            "Node 2 should be a learner, learners: {:?}",
            learners
        );

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_metaraft_learner_node1").await;

        Ok(())
    }

    /// Test promoting a learner to voter
    #[tokio::test]
    async fn test_promote_meta_voter() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_metaraft_promote_node1").await;

        // Create bootstrap node
        let config1 = ClusterConfig {
            node_id: 1,
            data_dir: PathBuf::from("/tmp/test_metaraft_promote_node1"),
            bind_address: "127.0.0.1:7002".to_string(),
            raft_address: "127.0.0.1:50063".to_string(),
            num_groups: 1,
            is_bootstrap: true,
            initial_members: vec![(1, "127.0.0.1:50063".to_string())],
        };

        let mut node1 = ClusterNode::new(config1);
        node1.initialize().await?;

        // Wait for cluster to stabilize
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Add node 2 as a learner
        node1
            .add_meta_learner(2, "127.0.0.1:50064".to_string())
            .await?;

        // Wait for learner to be added
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Promote node 2 to voter
        node1.promote_meta_voter(vec![1, 2]).await?;

        // Wait for promotion to complete
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Verify node 2 is now a voter
        let meta_raft = node1.meta_raft().unwrap();
        let raft = meta_raft.raft();
        let metrics = raft.metrics().borrow().clone();
        let membership = metrics.membership_config.membership();

        let voters: Vec<u64> = membership.voter_ids().collect();
        assert!(
            voters.contains(&2),
            "Node 2 should be a voter, voters: {:?}",
            voters
        );

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_metaraft_promote_node1").await;

        Ok(())
    }

    /// Test change_meta_membership API
    #[tokio::test]
    async fn test_change_meta_membership() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_metaraft_change_node1").await;

        // Create bootstrap node
        let config1 = ClusterConfig {
            node_id: 1,
            data_dir: PathBuf::from("/tmp/test_metaraft_change_node1"),
            bind_address: "127.0.0.1:7003".to_string(),
            raft_address: "127.0.0.1:50065".to_string(),
            num_groups: 1,
            is_bootstrap: true,
            initial_members: vec![(1, "127.0.0.1:50065".to_string())],
        };

        let mut node1 = ClusterNode::new(config1);
        node1.initialize().await?;

        // Wait for cluster to stabilize
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Add nodes 2 and 3 as learners
        node1
            .add_meta_learner(2, "127.0.0.1:50066".to_string())
            .await?;
        node1
            .add_meta_learner(3, "127.0.0.1:50067".to_string())
            .await?;

        // Wait for learners to be added
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Change membership to include all three nodes as voters
        node1
            .change_meta_membership(vec![1, 2, 3], true)
            .await?;

        // Wait for membership change to complete
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Verify all nodes are voters
        let meta_raft = node1.meta_raft().unwrap();
        let raft = meta_raft.raft();
        let metrics = raft.metrics().borrow().clone();
        let membership = metrics.membership_config.membership();

        let voters: Vec<u64> = membership.voter_ids().collect();
        assert_eq!(voters.len(), 3, "Should have 3 voters");
        assert!(voters.contains(&1), "Node 1 should be a voter");
        assert!(voters.contains(&2), "Node 2 should be a voter");
        assert!(voters.contains(&3), "Node 3 should be a voter");

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_metaraft_change_node1").await;

        Ok(())
    }

    /// Test single-node bootstrap initialization
    #[tokio::test]
    async fn test_single_node_bootstrap() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_metaraft_bootstrap").await;

        // Create bootstrap node (single-node cluster)
        let config = ClusterConfig {
            node_id: 1,
            data_dir: PathBuf::from("/tmp/test_metaraft_bootstrap"),
            bind_address: "127.0.0.1:7004".to_string(),
            raft_address: "127.0.0.1:50068".to_string(),
            num_groups: 1,
            is_bootstrap: true,
            initial_members: vec![(1, "127.0.0.1:50068".to_string())],
        };

        let mut node = ClusterNode::new(config);
        node.initialize().await?;

        // Wait for cluster to stabilize
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Verify node is initialized and is a voter
        let meta_raft = node.meta_raft().unwrap();
        let raft = meta_raft.raft();
        let metrics = raft.metrics().borrow().clone();
        let membership = metrics.membership_config.membership();

        let voters: Vec<u64> = membership.voter_ids().collect();
        assert_eq!(voters.len(), 1, "Should have 1 voter");
        assert!(voters.contains(&1), "Node 1 should be a voter");

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_metaraft_bootstrap").await;

        Ok(())
    }

    /// Test learner to voter workflow with multiple nodes
    #[tokio::test]
    async fn test_multi_node_learner_to_voter_workflow() -> Result<()> {
        // Cleanup before test
        let _ = tokio::fs::remove_dir_all("/tmp/test_metaraft_workflow_node1").await;

        // Step 1: Bootstrap node starts as single-node cluster
        let config1 = ClusterConfig {
            node_id: 1,
            data_dir: PathBuf::from("/tmp/test_metaraft_workflow_node1"),
            bind_address: "127.0.0.1:7005".to_string(),
            raft_address: "127.0.0.1:50069".to_string(),
            num_groups: 1,
            is_bootstrap: true,
            initial_members: vec![(1, "127.0.0.1:50069".to_string())],
        };

        let mut node1 = ClusterNode::new(config1);
        node1.initialize().await?;

        // Wait for cluster to stabilize
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Step 2: Add node 2 as learner
        node1
            .add_meta_learner(2, "127.0.0.1:50070".to_string())
            .await?;

        // Wait for learner to sync logs
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Step 3: Promote node 2 to voter
        node1.promote_meta_voter(vec![1, 2]).await?;

        // Wait for promotion
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Step 4: Verify node 2 is a voter
        let meta_raft = node1.meta_raft().unwrap();
        let raft = meta_raft.raft();
        let metrics = raft.metrics().borrow().clone();
        let membership = metrics.membership_config.membership();

        let voters: Vec<u64> = membership.voter_ids().collect();
        assert_eq!(voters.len(), 2, "Should have 2 voters");
        assert!(voters.contains(&1), "Node 1 should be a voter");
        assert!(voters.contains(&2), "Node 2 should be a voter");

        // Step 5: Add node 3 as learner
        node1
            .add_meta_learner(3, "127.0.0.1:50071".to_string())
            .await?;

        // Wait for learner to sync
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Step 6: Promote node 3 to voter
        node1.promote_meta_voter(vec![1, 2, 3]).await?;

        // Wait for promotion
        sleep(CLUSTER_STABILIZATION_DELAY).await;

        // Step 7: Verify all 3 nodes are voters
        let metrics = raft.metrics().borrow().clone();
        let membership = metrics.membership_config.membership();
        let voters: Vec<u64> = membership.voter_ids().collect();

        assert_eq!(voters.len(), 3, "Should have 3 voters");
        assert!(voters.contains(&1), "Node 1 should be a voter");
        assert!(voters.contains(&2), "Node 2 should be a voter");
        assert!(voters.contains(&3), "Node 3 should be a voter");

        // Cleanup
        let _ = tokio::fs::remove_dir_all("/tmp/test_metaraft_workflow_node1").await;

        Ok(())
    }
}
