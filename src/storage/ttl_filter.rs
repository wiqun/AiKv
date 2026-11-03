//! TTL 过期值与孤儿 Subkey 在 compaction 时丢弃. 纯决策, 不改 `expires` 计数.

use aidb::engine::compaction::{CompactionFilter, CompactionPointReader, FilterDecision};

use crate::storage::subkey::parse_strict_subkey;
use crate::storage::types::{StoredValue, ValueType};

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

/// TTL 过期的 key-value entry 及孤儿 subkey 在 compaction 时自动丢弃.
pub struct TtlExpireFilter;

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

        // 2. 严格校验 subkey 键编码规范 (长度与 tag 校验)
        if let Some((parent_user_key, expected_kind)) = parse_strict_subkey(user_key) {
            let parent_engine_key = if cluster_prefix.is_empty() {
                parent_user_key
            } else {
                [cluster_prefix, &parent_user_key].concat()
            };

            // 点查 parent header (支持单机与集群)
            return match reader.get(&parent_engine_key) {
                Ok(None) => {
                    // Header 彻底不存在, 确认为孤儿 subkey, 兜底回收
                    FilterDecision::Remove
                }
                Ok(Some(raw_header)) => {
                    let Ok(header_stored) = postcard::from_bytes::<StoredValue>(&raw_header) else {
                        // 无法反序列化 header, 保守保留
                        return FilterDecision::Keep;
                    };
                    if header_stored.is_expired() {
                        // Header 已过期, 孤儿 subkey 回收
                        return FilterDecision::Remove;
                    }
                    // 校验 Header 类型是否与 subkey tag 一致
                    match header_stored.value {
                        ValueType::CollectionHeader { kind, .. } if kind == expected_kind => {
                            // Header 存在、未过期且类型匹配, 严守反向红线必须保留
                            FilterDecision::Keep
                        }
                        _ => {
                            // Header 类型不匹配 (例如被同名重建为 String 或其他集合), 旧 subkey 回收
                            FilterDecision::Remove
                        }
                    }
                }
                Err(_) => {
                    // 点查失败时保守保留, 避免误删
                    FilterDecision::Keep
                }
            };
        }

        // 3. 常规 key: 走标准 TTL 检查
        self.filter(level, key, value)
    }
}
