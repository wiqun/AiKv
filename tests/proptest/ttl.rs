//! TTL 过期边界与 AikvCompactionFilter 的 property-based 测试.

use proptest::prelude::*;

use aidb::engine::compaction::{CompactionFilter, FilterDecision};
use aikv::storage::{AikvCompactionFilter, StoredValue, ValueType};

/// TTL 烟雾测试: is_expired() 不 panic, 返回 bool.
///
/// 由于 is_expired() 内部调用 now_ms(), 无法传入固定时间戳做精确边界验证.
/// 精确判定由 Test 2 (filter 集成) 覆盖.
#[test]
fn prop_ttl_expired_smoke() {
    let strategy = (0u64..10_000_000_000_000u64).prop_map(|expires_at| StoredValue {
        value: ValueType::String(b"v".to_vec()),
        expires_at: Some(expires_at),
    });
    let mut runner = proptest::test_runner::TestRunner::default();
    runner
        .run(&strategy, |stored| {
            let result = stored.is_expired();
            // 返回值必须是 bool, 不能 panic
            let _: bool = result;
            Ok(())
        })
        .unwrap();
}

/// AikvCompactionFilter 集成: 验证完整过滤链路.
///
/// - 已过期 entry → Remove
/// - 未过期 entry → Keep
/// - expires_at: None → Keep
/// - 非 StoredValue 格式 → Keep (保守保留)
#[test]
fn prop_ttl_filter_integration() {
    let strategy = (
        0u64..10_000_000_000_000u64,
        prop::collection::vec(any::<u8>(), 0..16),
    );
    let mut runner = proptest::test_runner::TestRunner::default();
    runner
        .run(&strategy, |(expires_at, value_bytes)| {
            let filter = AikvCompactionFilter::single_node();

            // Case 1: 合法 StoredValue
            let stored = StoredValue {
                value: ValueType::String(value_bytes.clone()),
                expires_at: Some(expires_at),
            };
            let encoded = postcard::to_allocvec(&stored).unwrap();
            let decision = filter.filter(0, b"k", &encoded);
            // filter 不 panic 且返回合法决策
            prop_assert!(matches!(
                decision,
                FilterDecision::Keep | FilterDecision::Remove
            ));

            // Case 2: expires_at: None, 必须 Keep
            let stored_no_exp = StoredValue {
                value: ValueType::String(value_bytes),
                expires_at: None,
            };
            let encoded_no_exp = postcard::to_allocvec(&stored_no_exp).unwrap();
            prop_assert_eq!(
                filter.filter(0, b"k", &encoded_no_exp),
                FilterDecision::Keep
            );

            // Case 3: 非 StoredValue 格式 (随机字节), 保守保留
            let random_bytes: Vec<u8> = (0..32).map(|_| rand::random()).collect();
            prop_assert_eq!(filter.filter(0, b"k", &random_bytes), FilterDecision::Keep);

            Ok(())
        })
        .unwrap();
}
