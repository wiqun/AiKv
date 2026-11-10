//! String 算子热 Key 读改写流水线 (SETRANGE / INCR / BITFIELD / APPEND / SETBIT).
//! 在持有 KeyLock 期间仅完成内存读改写，写入在途缓冲并登记后立即放锁；
//! 在锁外调用 enqueue_batch 并在拿到句柄后立刻 publication.succeed() 放行后继，
//! 消除 1/RTT 串行网络等待，使 GroupSetBatcher 能够批量聚合并行提议.

use std::sync::Arc;

use crate::command::shared::{commit_ops_in_lock, InFlightContext};
use crate::error::Result;
use crate::storage::types::{StoredValue, WriteOp};
use crate::storage::KvStorage;

/// 在持有 KeyLock 的临界区内读取最新 String 字节.
/// 严格遵守 Tombstone 语义: 只有返回 None 才穿透读底库;
/// 若在途缓冲为 Some(None) 表示键已被在途删除，严禁读底库复活旧值.
pub(crate) async fn read_string_in_lock(
    storage: &dyn KvStorage,
    in_flight_ctx: &InFlightContext,
    db: usize,
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    if let Some(opt_stored) = in_flight_ctx.in_flight.get(db, key) {
        match opt_stored {
            Some(stored) => return Ok(Some(stored.as_string()?.to_vec())),
            None => return Ok(None),
        }
    }
    storage.get(db, key).await
}

/// 在持有 KeyLock 的临界区内读取最新 StoredValue (感知在途与过期元数据).
pub(crate) async fn read_stored_in_lock(
    storage: &dyn KvStorage,
    in_flight_ctx: &InFlightContext,
    db: usize,
    key: &[u8],
) -> Result<Option<StoredValue>> {
    if let Some(opt_stored) = in_flight_ctx.in_flight.get(db, key) {
        return Ok(opt_stored);
    }
    storage.get_typed(db, key).await
}

/// 在持有 KeyLock 的临界区内判断键是否存在 (优先检查在途缓冲与 Tombstone).
pub(crate) async fn exists_in_lock(
    storage: &dyn KvStorage,
    in_flight_ctx: &InFlightContext,
    db: usize,
    key: &[u8],
) -> Result<bool> {
    match in_flight_ctx.in_flight.get(db, key) {
        Some(Some(_)) => Ok(true),
        Some(None) => Ok(false),
        None => storage.exists(db, key).await,
    }
}

/// 在锁内登记在途缓冲并发布入队序列，随后立即释放 KeyLock，
/// 在锁外入队底层存储并立刻放行后续等待请求.
pub(crate) async fn commit_string_in_lock<G>(
    storage: &dyn KvStorage,
    in_flight_ctx: &InFlightContext,
    db: usize,
    key: &[u8],
    new_value: Vec<u8>,
    guard: G,
) -> Result<()> {
    let ops = vec![(key.to_vec(), WriteOp::Put(Arc::new(new_value)))];
    commit_ops_in_lock(storage, in_flight_ctx, db, key, ops, guard).await
}
