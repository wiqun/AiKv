//! TTL 过期值与孤儿 Subkey 在 compaction 时丢弃. 纯决策, 不改 `expires` 计数.

use aidb::engine::compaction::{CompactionFilter, CompactionPointReader, FilterDecision};
use parking_lot::Mutex;

use crate::storage::subkey::parse_strict_subkey;
use crate::storage::types::{now_ms, CollectionKind, StoredValue, ValueType};

/// aidb 状态机 key 前缀格式: `\x01sm/{gid:8B}/{user_key}`
/// 与 `aidb::cluster::storage::keys::sm_key` 对齐 (4 字节前缀 + 8 字节 gid + 1 字节 '/' = 13 字节)
pub const CLUSTER_SM_PREFIX_HEADER: &[u8] = b"\x01sm/";
pub const CLUSTER_SM_PREFIX_LEN: usize = 13;

/// 剥离集群状态机前缀, 返回 `(cluster_prefix, user_key)` 切片.
/// 非集群 key 返回 `(&[], engine_key)`.
pub fn split_cluster_sm_prefix(engine_key: &[u8]) -> (&[u8], &[u8]) {
    if engine_key.starts_with(CLUSTER_SM_PREFIX_HEADER)
        && engine_key.len() >= CLUSTER_SM_PREFIX_LEN
        && engine_key[CLUSTER_SM_PREFIX_LEN - 1] == b'/'
    {
        (
            &engine_key[..CLUSTER_SM_PREFIX_LEN],
            &engine_key[CLUSTER_SM_PREFIX_LEN..],
        )
    } else {
        (&[][..], engine_key)
    }
}

#[derive(Clone)]
enum CachedParentState {
    Missing,
    Corrupted,
    Header {
        kind: CollectionKind,
        version: u64,
        expires_at: Option<u64>,
    },
    NotCollection,
}

struct CachedParent {
    reader_ptr: usize,
    parent_engine_key: Vec<u8>,
    state: CachedParentState,
}

/// TTL 过期的 key-value entry 及孤儿 subkey 在 compaction 时自动丢弃.
pub struct TtlExpireFilter {
    last_parent: Mutex<Option<CachedParent>>,
}

impl Default for TtlExpireFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl TtlExpireFilter {
    pub fn new() -> Self {
        Self {
            last_parent: Mutex::new(None),
        }
    }
}

impl CompactionFilter for TtlExpireFilter {
    fn filter(&self, _level: usize, key: &[u8], value: &[u8]) -> FilterDecision {
        let engine_key = if key.len() >= 8 {
            &key[..key.len() - 8]
        } else {
            key
        };
        let (_, user_key) = split_cluster_sm_prefix(engine_key);

        // 1. 若 value 能够反序列化为非 CollectionHeader 的 StoredValue, 确认为合法独立用户 key
        if let Ok(stored) = postcard::from_bytes::<StoredValue>(value) {
            if !matches!(stored.value, ValueType::CollectionHeader { .. }) {
                return if stored.is_expired() {
                    FilterDecision::Remove
                } else {
                    FilterDecision::Keep
                };
            }
        }

        // 2. 若结构符合 subkey 规范但处于无 reader 路径, 保守保留
        if parse_strict_subkey(user_key).is_some() {
            return FilterDecision::Keep;
        }

        let Ok(stored) = postcard::from_bytes::<StoredValue>(value) else {
            return FilterDecision::Keep;
        };
        if stored.is_expired() {
            FilterDecision::Remove
        } else {
            FilterDecision::Keep
        }
    }

    fn filter_with_reader(
        &self,
        level: usize,
        key: &[u8],
        value: &[u8],
        reader: &dyn CompactionPointReader,
    ) -> FilterDecision {
        let engine_key = if key.len() >= 8 {
            &key[..key.len() - 8]
        } else {
            key
        };

        let (cluster_prefix, user_key) = split_cluster_sm_prefix(engine_key);

        // 1. 若 value 为包装好的普通数据 StoredValue (非 CollectionHeader), 确认为用户普通 key, 绝不误删
        if let Ok(stored) = postcard::from_bytes::<StoredValue>(value) {
            if !matches!(stored.value, ValueType::CollectionHeader { .. }) {
                return if stored.is_expired() {
                    FilterDecision::Remove
                } else {
                    FilterDecision::Keep
                };
            }
        }

        // 2. 严格校验 subkey 键编码规范 (长度, tag 与 version 校验)
        if let Some((parent_user_key, expected_kind, subkey_version)) =
            parse_strict_subkey(user_key)
        {
            let parent_engine_key = if cluster_prefix.is_empty() {
                parent_user_key
            } else {
                [cluster_prefix, &parent_user_key].concat()
            };
            let reader_ptr = (reader as *const dyn CompactionPointReader) as *const () as usize;

            let cached = {
                let guard = self.last_parent.lock();
                guard
                    .as_ref()
                    .filter(|c| {
                        c.reader_ptr == reader_ptr && c.parent_engine_key == parent_engine_key
                    })
                    .map(|c| c.state.clone())
            };

            let state = match cached {
                Some(s) => s,
                None => {
                    let fetched = match reader.get(&parent_engine_key) {
                        Ok(None) => CachedParentState::Missing,
                        Ok(Some(raw_header)) => {
                            if let Ok(header_stored) =
                                postcard::from_bytes::<StoredValue>(&raw_header)
                            {
                                match header_stored.value {
                                    ValueType::CollectionHeader { kind, version, .. } => {
                                        CachedParentState::Header {
                                            kind,
                                            version,
                                            expires_at: header_stored.expires_at,
                                        }
                                    }
                                    _ => CachedParentState::NotCollection,
                                }
                            } else {
                                CachedParentState::Corrupted
                            }
                        }
                        Err(_) => {
                            // 点查失败时不写缓存, 保守保留
                            return FilterDecision::Keep;
                        }
                    };
                    *self.last_parent.lock() = Some(CachedParent {
                        reader_ptr,
                        parent_engine_key,
                        state: fetched.clone(),
                    });
                    fetched
                }
            };

            return match state {
                CachedParentState::Missing | CachedParentState::NotCollection => {
                    FilterDecision::Remove
                }
                CachedParentState::Corrupted => FilterDecision::Keep,
                CachedParentState::Header {
                    kind,
                    version,
                    expires_at,
                } => {
                    if expires_at.is_some_and(|exp| now_ms() >= exp)
                        || kind != expected_kind
                        || version != subkey_version
                    {
                        FilterDecision::Remove
                    } else {
                        FilterDecision::Keep
                    }
                }
            };
        }

        // 3. 常规 key: 走标准 TTL 检查
        self.filter(level, key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::subkey::encode_hash_field_key;
    use crate::storage::types::CollectionKind;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct MockReader {
        data: Mutex<std::collections::HashMap<Vec<u8>, Vec<u8>>>,
        gets: AtomicUsize,
    }

    impl MockReader {
        fn new() -> Self {
            Self {
                data: Mutex::new(std::collections::HashMap::new()),
                gets: AtomicUsize::new(0),
            }
        }
        fn insert(&self, k: Vec<u8>, v: Vec<u8>) {
            self.data.lock().unwrap().insert(k, v);
        }
        fn get_count(&self) -> usize {
            self.gets.load(Ordering::Relaxed)
        }
    }

    impl CompactionPointReader for MockReader {
        fn get(&self, user_key: &[u8]) -> aidb::Result<Option<Vec<u8>>> {
            self.gets.fetch_add(1, Ordering::Relaxed);
            Ok(self.data.lock().unwrap().get(user_key).cloned())
        }
    }

    fn make_internal_key(engine_key: &[u8]) -> Vec<u8> {
        let mut k = engine_key.to_vec();
        k.extend_from_slice(&[0u8; 8]);
        k
    }

    #[test]
    fn ttl_filter_caches_parent_lookup_and_removes_stale_version() {
        let reader = MockReader::new();
        let parent_key = b"db0:myhash".to_vec();
        let header = StoredValue {
            value: ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                count: 10,
                version: 2,
                head: 0,
                tail: 0,
            },
            expires_at: None,
        };
        reader.insert(parent_key.clone(), postcard::to_allocvec(&header).unwrap());

        let filter = TtlExpireFilter::new();
        for i in 0..10u8 {
            let sub_keep = make_internal_key(&encode_hash_field_key(&parent_key, 2, &[i]));
            assert_eq!(
                filter.filter_with_reader(1, &sub_keep, b"v", &reader),
                FilterDecision::Keep
            );
            let sub_stale = make_internal_key(&encode_hash_field_key(&parent_key, 1, &[i]));
            assert_eq!(
                filter.filter_with_reader(1, &sub_stale, b"v", &reader),
                FilterDecision::Remove
            );
        }
        assert_eq!(reader.get_count(), 1);
    }
}
