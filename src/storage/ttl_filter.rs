//! TTL 过期值在 compaction 时丢弃. 纯决策, 不改 `expires` 计数.

use aidb::engine::compaction::{CompactionFilter, FilterDecision};

use crate::storage::types::StoredValue;

/// TTL 过期的 key-value entry 在 compaction 时自动丢弃.
pub struct TtlExpireFilter;

impl CompactionFilter for TtlExpireFilter {
    fn filter(&self, _level: usize, _key: &[u8], value: &[u8]) -> FilterDecision {
        let Ok(stored) = postcard::from_bytes::<StoredValue>(value) else {
            return FilterDecision::Keep;
        };
        if stored.is_expired() {
            FilterDecision::Remove
        } else {
            FilterDecision::Keep
        }
    }
}
