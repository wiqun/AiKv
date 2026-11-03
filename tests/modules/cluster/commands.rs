//! @component aikv-cluster

// CLUSTER 命令格式测试 (L1).
// 测试在不初始化集群状态时的错误响应.
#![cfg(feature = "cluster")]

use aikv::cluster::*;

#[test]
fn cluster_info_uninitialized() {
    let err = cluster_info().unwrap_err();
    assert!(
        err.contains("CLUSTERDOWN"),
        "expected CLUSTERDOWN, got: {err}"
    );
}

#[test]
fn cluster_nodes_uninitialized() {
    // Also verifies data_port_offset is safely defaulted (map_or) when CLUSTER_STATE_MGR is uninitialized.
    let err = cluster_nodes().unwrap_err();
    assert!(
        err.contains("CLUSTERDOWN"),
        "expected CLUSTERDOWN, got: {err}"
    );
}

#[test]
fn cluster_slots_uninitialized() {
    let err = cluster_slots().unwrap_err();
    assert!(
        err.contains("CLUSTERDOWN"),
        "expected CLUSTERDOWN, got: {err}"
    );
}

#[test]
fn cluster_shards_uninitialized() {
    let err = cluster_shards().unwrap_err();
    assert!(
        err.contains("CLUSTERDOWN"),
        "expected CLUSTERDOWN, got: {err}"
    );
}

#[test]
fn cluster_myshardid_uninitialized() {
    let err = cluster_myshardid().unwrap_err();
    assert!(
        err.contains("CLUSTERDOWN"),
        "expected CLUSTERDOWN, got: {err}"
    );
}

#[tokio::test]
async fn cluster_count_keys_in_slot_uninitialized() {
    let err = cluster_count_keys_in_slot(0).await.unwrap_err();
    assert!(
        err.contains("CLUSTERDOWN"),
        "expected CLUSTERDOWN, got: {err}"
    );
}

#[tokio::test]
async fn cluster_get_keys_in_slot_uninitialized() {
    let err = cluster_get_keys_in_slot(0, 10).await.unwrap_err();
    assert!(
        err.contains("CLUSTERDOWN"),
        "expected CLUSTERDOWN, got: {err}"
    );
}

#[test]
fn cluster_replicas_uninitialized() {
    let err = cluster_replicas(1).unwrap_err();
    assert!(
        err.contains("CLUSTERDOWN"),
        "expected CLUSTERDOWN, got: {err}"
    );
}

#[test]
fn cluster_keyslot_format() {
    let result = cluster_keyslot(b"mykey");
    assert!(result.is_ok());
    let slot: u16 = result.unwrap().parse().expect("slot must be a u16");
    assert!(slot < 16384, "slot {slot} out of range");
}

#[test]
fn cluster_keyslot_deterministic() {
    let a = cluster_keyslot(b"test").unwrap();
    let b = cluster_keyslot(b"test").unwrap();
    assert_eq!(a, b, "KEYSLOT must be deterministic");
}

#[test]
fn cluster_keyslot_empty_key() {
    let result = cluster_keyslot(b"");
    assert!(result.is_ok());
    let slot: u16 = result.unwrap().parse().unwrap();
    assert!(slot < 16384);
}

#[test]
fn cluster_keyslot_hash_tag() {
    let with_tag = cluster_keyslot(b"{user}.name").unwrap();
    let just_tag = cluster_keyslot(b"user").unwrap();
    assert_eq!(
        with_tag, just_tag,
        "hash tag {{user}} should use same slot as 'user'"
    );
}

#[test]
fn parse_int_edge_cases() {
    assert_eq!(parse_int::<u16>(b"0"), Some(0));
    assert_eq!(parse_int::<u16>(b"65535"), Some(u16::MAX));
    assert_eq!(parse_int::<u16>(b"65536"), None); // overflow
    assert_eq!(parse_int::<u8>(b"-1"), None);
    assert_eq!(parse_int::<u32>(b""), None);
    assert_eq!(parse_int::<u64>(b"18446744073709551615"), Some(u64::MAX));
}

#[test]
fn parse_hex_node_id_padding() {
    // Minimal: "01" = 1
    let r = parse_hex_node_id("01");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), 1);

    // Leading zeros are fine
    let r = parse_hex_node_id("00000000000000000000000000000001");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), 1);

    // Too long (u64::from_str_radix rejects >16 hex digits when value exceeds u64::MAX)
    let r = parse_hex_node_id("10000000000000000000000000000000000000001");
    assert!(r.is_err());
}
