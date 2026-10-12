//! aikv 的 compaction filter: 删除过期主键, 回收孤儿 subkey.
//!
//! 单机模式下物理 key 就是 aikv key; 集群模式下数据面 key 为
//! `\x01sm/{gid 8B}/{aikv key}`, 其它前缀 (Raft 日志、迁移 oplog) 一律保留.
//! subkey 只在 header 存在、未过期且版本相同时保留; 版本生成后
//! `grace_us` 内的 subkey 一律保留, 给暂存写留出切换 header 的时间.

use std::cell::RefCell;

use aidb::engine::compaction::{CompactionFilter, CompactionPointReader, FilterDecision};
use aidb::engine::memtable::extract_user_key;

use crate::storage::collection::{now_micros, version_micros, SUBKEY_GC_GRACE_US};
use crate::storage::subkey::decode_versioned_subkey;
use crate::storage::types::{StoredValue, ValueType};
use crate::storage::AiDbEngine;

const SM_PREFIX: &[u8] = b"\x01sm/";
const SM_HEADER_LEN: usize = SM_PREFIX.len() + 8 + 1;

#[derive(Clone, Copy)]
enum Mode {
    SingleNode,
    Cluster,
}

pub struct AikvCompactionFilter {
    mode: Mode,
    grace_us: u64,
}

/// 同一次 compaction 中对某个 header 的点查结果, 用于跨 subkey 复用.
#[derive(Clone, Copy)]
enum Seen {
    Missing,
    Live(u64),
    Expired(u64),
    /// 不是集合 header (类型变了); 任何版本的 subkey 都不可见.
    Other,
}

thread_local! {
    static LAST_HEADER: RefCell<Option<(Vec<u8>, Seen)>> = const { RefCell::new(None) };
}

impl AikvCompactionFilter {
    pub fn single_node() -> Self {
        Self {
            mode: Mode::SingleNode,
            grace_us: SUBKEY_GC_GRACE_US,
        }
    }

    pub fn cluster() -> Self {
        Self {
            mode: Mode::Cluster,
            grace_us: SUBKEY_GC_GRACE_US,
        }
    }

    pub fn with_grace_us(mut self, grace_us: u64) -> Self {
        self.grace_us = grace_us;
        self
    }

    /// 拆成 (读 header 用的物理前缀, aikv key). 非数据面 key 返回 None.
    fn split<'a>(&self, user_key: &'a [u8]) -> Option<(&'a [u8], &'a [u8])> {
        match self.mode {
            Mode::SingleNode => Some((&[], user_key)),
            Mode::Cluster => {
                if !user_key.starts_with(SM_PREFIX) || user_key.len() < SM_HEADER_LEN {
                    return None;
                }
                Some(user_key.split_at(SM_HEADER_LEN))
            }
        }
    }

    fn expired_main_key(value: &[u8]) -> FilterDecision {
        match postcard::from_bytes::<StoredValue>(value) {
            Ok(stored) if stored.is_expired() => FilterDecision::Remove,
            _ => FilterDecision::Keep,
        }
    }

    fn observe(reader: &dyn CompactionPointReader, header_key: &[u8]) -> Option<Seen> {
        let raw = match reader.get(header_key) {
            Ok(Some(raw)) => raw,
            Ok(None) => return Some(Seen::Missing),
            Err(_) => return None,
        };
        let stored = postcard::from_bytes::<StoredValue>(&raw).ok()?;
        Some(match stored.value {
            ValueType::CollectionHeader { version, .. } if stored.is_expired() => {
                Seen::Expired(version)
            }
            ValueType::CollectionHeader { version, .. } => Seen::Live(version),
            _ => Seen::Other,
        })
    }

    /// `None` 表示缓存不足以下结论, 需要重新点查.
    fn decide_cached(seen: Seen, version: u64) -> Option<FilterDecision> {
        match seen {
            Seen::Live(v) if version == v => Some(FilterDecision::Keep),
            Seen::Live(v) | Seen::Expired(v) if version < v => Some(FilterDecision::Remove),
            Seen::Expired(v) if version == v => Some(FilterDecision::Remove),
            _ => None,
        }
    }

    fn decide_fresh(seen: Seen, version: u64) -> FilterDecision {
        match seen {
            Seen::Live(v) if v == version => FilterDecision::Keep,
            _ => FilterDecision::Remove,
        }
    }
}

impl CompactionFilter for AikvCompactionFilter {
    fn filter(&self, _level: usize, key: &[u8], value: &[u8]) -> FilterDecision {
        let Some((_, aikv_key)) = self.split(extract_user_key(key)) else {
            return FilterDecision::Keep;
        };
        if decode_versioned_subkey(aikv_key).is_some() {
            return FilterDecision::Keep;
        }
        Self::expired_main_key(value)
    }

    fn filter_with_reader(
        &self,
        _level: usize,
        key: &[u8],
        value: &[u8],
        reader: &dyn CompactionPointReader,
    ) -> FilterDecision {
        let Some((space, aikv_key)) = self.split(extract_user_key(key)) else {
            return FilterDecision::Keep;
        };
        let Some(sub) = decode_versioned_subkey(aikv_key) else {
            return Self::expired_main_key(value);
        };
        if now_micros().saturating_sub(version_micros(sub.version)) < self.grace_us {
            return FilterDecision::Keep;
        }
        let mut header_key = space.to_vec();
        header_key.extend_from_slice(&AiDbEngine::encode_key(sub.db, sub.user_key));

        let cached = LAST_HEADER.with(|cell| {
            cell.borrow()
                .as_ref()
                .filter(|(k, _)| *k == header_key)
                .and_then(|(_, seen)| Self::decide_cached(*seen, sub.version))
        });
        if let Some(decision) = cached {
            return decision;
        }
        let Some(seen) = Self::observe(reader, &header_key) else {
            return FilterDecision::Keep;
        };
        LAST_HEADER.with(|cell| *cell.borrow_mut() = Some((header_key, seen)));
        Self::decide_fresh(seen, sub.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缓存结论只能删除确定过时的 subkey; 更新版本或无法判断时必须保留或重查.
    #[test]
    fn cached_decisions_never_drop_a_newer_version() {
        assert!(AikvCompactionFilter::decide_cached(Seen::Missing, 5).is_none());
        assert!(AikvCompactionFilter::decide_cached(Seen::Live(5), 6).is_none());
        assert!(AikvCompactionFilter::decide_cached(Seen::Expired(5), 6).is_none());
        assert_eq!(
            AikvCompactionFilter::decide_cached(Seen::Live(5), 5),
            Some(FilterDecision::Keep)
        );
        assert_eq!(
            AikvCompactionFilter::decide_cached(Seen::Live(5), 4),
            Some(FilterDecision::Remove)
        );
        assert_eq!(
            AikvCompactionFilter::decide_cached(Seen::Expired(5), 5),
            Some(FilterDecision::Remove)
        );
    }

    /// 集群模式只处理 `\x01sm/` 数据面 key, Raft 日志等其它前缀一律保留.
    #[test]
    fn cluster_mode_keeps_non_data_keys() {
        let filter = AikvCompactionFilter::cluster();
        assert!(filter.split(b"\x00raft/12345678/log/x").is_none());
        let mut sm = b"\x01sm/".to_vec();
        sm.extend_from_slice(&7u64.to_be_bytes());
        sm.push(b'/');
        sm.extend_from_slice(b"0:k");
        let (space, key) = filter.split(&sm).unwrap();
        assert_eq!(key, b"0:k");
        assert!(space.starts_with(b"\x01sm/"));
    }
}
