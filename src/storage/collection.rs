//! 大集合 (subkey 布局) 的阈值、版本号与写入描述.
//!
//! 版本号 = max(上次 + 1, 当前微秒 << 11). 低 11 位是同一微秒内的序号,
//! `version >> 11` 是生成时刻, compaction GC 用它判断宽限期.
//! 时钟回拨后新旧版本可能相等, 依赖 NTP; 见 docs/modules/03-storage.md.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::storage::types::{CollectionKind, StoredValue};

/// Hash 的 field 数不超过该值时整体内联在主键值中, 否则走 subkey 布局.
pub const HASH_MAX_INLINE_FIELDS: usize = 64;
/// Set 的 member 数不超过该值时整体内联在主键值中, 否则走 subkey 布局.
pub const SET_MAX_INLINE_MEMBERS: usize = 64;

/// 暂存写到切换 header 之间, subkey 看起来是垃圾; 这段时间内 GC 不碰它.
pub const SUBKEY_GC_GRACE_US: u64 = 3_600_000_000;

const SEQ_BITS: u32 = 11;

static LAST_VERSION: AtomicU64 = AtomicU64::new(0);

/// 当前 Unix 时间 (微秒).
pub fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

/// 生成严格递增的集合版本号 (进程内单调).
pub fn next_collection_version() -> u64 {
    let floor = now_micros() << SEQ_BITS;
    let mut last = LAST_VERSION.load(Ordering::Relaxed);
    loop {
        let next = floor.max(last + 1);
        match LAST_VERSION.compare_exchange_weak(last, next, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(seen) => last = seen,
        }
    }
}

/// header 的写法.
pub enum HeaderWrite {
    /// 改写已存在的 header; TTL 计数净变化为 0 (同 `rewrite_stored`).
    Put(StoredValue),
    Delete {
        had_ttl: bool,
    },
}

/// 对同一集合同一版本的一次提交.
pub struct CollectionWrite {
    pub header: HeaderWrite,
    pub version: u64,
    pub kind: CollectionKind,
    pub puts: Vec<(Vec<u8>, Vec<u8>)>,
    pub deletes: Vec<Vec<u8>>,
}

/// 版本号中的生成时刻 (微秒).
pub fn version_micros(version: u64) -> u64 {
    version >> SEQ_BITS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 版本号严格递增, 且其时间部分落在调用前后的时间窗口内.
    #[test]
    fn versions_are_strictly_increasing_and_carry_time() {
        let before = now_micros();
        let a = next_collection_version();
        let b = next_collection_version();
        assert!(b > a);
        assert!(version_micros(a) >= before);
        assert!(version_micros(b) <= now_micros());
    }
}
