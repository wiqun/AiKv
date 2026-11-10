//! 验证全局共享 InFlightContext 跨模块可见性与生产注入一致性.

use std::sync::Arc;

use aikv::command::shared::in_flight_context::InFlightContext;
use aikv::command::CommandRouter;
use aikv::storage::memory::MemoryEngine;
use aikv::storage::types::WriteOp;
use aikv::storage::zset_subkey;
use aikv::storage::KvStorage;

#[tokio::test]
async fn test_shared_in_flight_context_visibility() {
    let ctx = InFlightContext::new();

    // 1. 写入普通 String 并在同/异句柄中可见
    let (seq1, encoded1) = ctx.in_flight.insert_batch(
        0,
        &[(
            b"str_key".to_vec(),
            WriteOp::Put(Arc::new(b"value_1".to_vec())),
        )],
    );
    let stored = ctx.in_flight.get(0, b"str_key");
    assert!(stored.is_some());
    let val = stored.unwrap().unwrap();
    assert_eq!(val.as_string().unwrap(), b"value_1");

    // 2. 写入 Raw Subkey (如 ZSet member)
    let member_op = zset_subkey::member_change_ops(0, b"z_key", b"m1", None, Some(10.5));
    let (seq2, encoded2) = ctx.in_flight.insert_batch(0, &member_op);

    // 验证 get 对 Raw 返回 None，而 get_raw 返回具体字节
    let enc_member_key = &encoded2[0];
    assert!(ctx.in_flight.get(0, b"z_key").is_none());
    let raw = ctx.in_flight.get_raw(enc_member_key);
    assert!(raw.is_some());
    assert!(raw.unwrap().is_some());

    // 3. Tombstone 语义: 写入 Delete -> get 返回 Some(None)
    let (seq3, encoded3) = ctx
        .in_flight
        .insert_batch(0, &[(b"del_key".to_vec(), WriteOp::Delete)]);
    let del_res = ctx.in_flight.get(0, b"del_key");
    assert_eq!(del_res, Some(None));

    // 清理
    ctx.in_flight.remove_encoded(&encoded1, seq1);
    ctx.in_flight.remove_encoded(&encoded2, seq2);
    ctx.in_flight.remove_encoded(&encoded3, seq3);
    assert!(ctx.in_flight.is_empty());
}

#[tokio::test]
async fn test_command_router_shares_same_in_flight_context() {
    let storage: Arc<dyn KvStorage> = MemoryEngine::new(1);
    let router = CommandRouter::new(storage);

    let ctx = router.in_flight_context();
    // 向 router 共享 context 写入
    let (seq, encoded) = ctx.in_flight.insert_batch(
        0,
        &[(
            b"shared_k".to_vec(),
            WriteOp::Put(Arc::new(b"shared_v".to_vec())),
        )],
    );

    // 验证 router 的 context 能读到
    let stored = router.in_flight_context().in_flight.get(0, b"shared_k");
    assert!(stored.is_some());
    assert_eq!(stored.unwrap().unwrap().as_string().unwrap(), b"shared_v");

    ctx.in_flight.remove_encoded(&encoded, seq);
    assert!(ctx.in_flight.is_empty());
}
