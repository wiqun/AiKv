//! @component aikv-cluster
//! CLUSTER 命令单元测试.
#[path = "modules/cluster/mod.rs"]
#[cfg(feature = "cluster")]
mod cluster;

#[cfg(feature = "cluster")]
mod tests {
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
