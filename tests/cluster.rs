//! 集群路由、命令、集成、迁移与一致性测试统一入口
//! @component aikv-cluster
//!
//! 注: 需通过 `cargo-nextest` 运行 (每用例独立进程), 避免 `CLUSTER_STATE_MGR`
//! OnceLock 全局状态在不同测试组之间互相污染.
#![cfg(feature = "cluster")]
#![recursion_limit = "512"]

#[path = "modules/cluster/mod.rs"]
mod cluster;

#[path = "modules/cluster/routing.rs"]
mod routing;

#[path = "modules/cluster/creategroup.rs"]
mod creategroup;

#[path = "modules/cluster/integration.rs"]
mod integration;

#[path = "modules/cluster/info_golden.rs"]
mod info_golden;

#[path = "modules/cluster/lazy_expire.rs"]
mod lazy_expire;

#[path = "modules/cluster/migration_merge_read.rs"]
mod migration_merge_read;

#[path = "modules/cluster/redirect_metrics.rs"]
mod redirect_metrics;

#[path = "modules/cluster/scan.rs"]
mod scan;

#[path = "modules/cluster/atom_exec_json_batch_redirect.rs"]
mod atom_exec_json_batch_redirect;

mod cluster_commands_unit {
    use aikv::cluster::*;

    #[test]
    fn cluster_keyslot_returns_integer() {
        let result = cluster_keyslot(b"mykey");
        assert!(result.is_ok());
        let slot_str = result.unwrap();
        let slot: u16 = slot_str.parse().unwrap();
        assert!(slot < 16384);
    }

    #[test]
    fn cluster_myid_returns_error_when_uninitialized() {
        let result = cluster_myid();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("CLUSTERDOWN"));
    }

    #[test]
    fn parse_hex_node_id_valid() {
        let result = parse_hex_node_id("0000000000000000000000000000000000000001");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1u64);
    }

    #[test]
    fn parse_hex_node_id_max() {
        let result = parse_hex_node_id("000000000000000000000000ffffffffffffffff");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), u64::MAX);
    }

    #[test]
    fn parse_hex_node_id_invalid() {
        let result = parse_hex_node_id("not-a-hex-string");
        assert!(result.is_err());
    }

    #[test]
    fn parse_int_valid_u16() {
        assert_eq!(parse_int::<u16>(b"6379"), Some(6379));
    }

    #[test]
    fn parse_int_invalid_string() {
        assert_eq!(parse_int::<u16>(b"abc"), None);
    }

    #[test]
    fn parse_int_overflow_u8() {
        assert_eq!(parse_int::<u8>(b"256"), None);
    }

    #[test]
    fn parse_cluster_node_id_decimal_and_hex() {
        assert_eq!(parse_cluster_node_id("6").expect("decimal"), 6);
        assert_eq!(
            parse_cluster_node_id("0000000000000000000000000000000000000006").expect("hex"),
            6
        );
    }

    #[test]
    fn parse_forget_force_flag() {
        assert!(!parse_forget_force(None).expect("default"));
        assert!(parse_forget_force(Some(b"FORCE")).expect("force"));
        assert!(parse_forget_force(Some(b"force")).expect("force lower"));
    }

    #[test]
    fn parse_forget_force_rejects_unknown_option() {
        assert!(parse_forget_force(Some(b"NOW")).is_err());
    }

    #[tokio::test]
    async fn cluster_failover_uninitialized() {
        let result = cluster_failover("FORCE").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("CLUSTERDOWN"));
    }
}
