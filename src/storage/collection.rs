//! 大集合 (subkey 布局) 的阈值、版本号与写入描述.
//!
//! 版本号 = max(上次 + 1, 当前微秒 << 11). 低 11 位是同一微秒内的序号,
//! `version >> 11` 是生成时刻, compaction GC 用它判断宽限期.
//! 时钟回拨后新旧版本可能相等, 依赖 NTP; 见 docs/modules/03-storage.md.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::storage::subkey::{encode_zset_score, zset_score_sub, SubTag};
use crate::storage::types::{CollectionKind, StoredValue, ValueType};

/// Hash 的 field 数不超过该值时整体内联在主键值中, 否则走 subkey 布局.
pub const HASH_MAX_INLINE_FIELDS: usize = 64;
/// Set 的 member 数不超过该值时整体内联在主键值中, 否则走 subkey 布局.
pub const SET_MAX_INLINE_MEMBERS: usize = 64;
/// ZSet 的 member 数超过该值时转为子键布局 (内联上限).
pub const ZSET_MAX_INLINE_MEMBERS: usize = 64;
/// 子键化的 ZSet 经 ZREM 降到该值以下 (含) 时回落为内联. 介于两者之间保持子键, 避免 64/65 边界抖动.
pub const ZSET_DEMOTE_MEMBERS: usize = 32;

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

/// 对同一集合同一版本的一次提交. 每条 subkey 自带标签 (ZSet 一次提交同时含 `Z` 与 `R`).
pub struct CollectionWrite {
    pub header: HeaderWrite,
    pub version: u64,
    /// `(tag, sub, value)`.
    pub puts: Vec<(SubTag, Vec<u8>, Vec<u8>)>,
    /// `(tag, sub)`.
    pub deletes: Vec<(SubTag, Vec<u8>)>,
}

/// 脚本内对子键 ZSET 的成员级增量: 每个触碰过的成员记录磁盘状态与当前状态.
///
/// 增量相对磁盘版本 `version` 计算; 同一版本的增量可以在一个 burst 内累积, 提交时一次展开为 `CollectionWrite`.
#[derive(Debug, Clone, PartialEq)]
pub struct ZSetDelta {
    pub version: u64,
    /// 磁盘 header 的成员数 (不含本次增量).
    pub base_count: u64,
    /// 磁盘 header 的过期时间, 用于 TTL 记账与提交判断.
    pub base_expires_at: Option<u64>,
    /// 提交后 header 的过期时间 (`EXPIRE` 可改写).
    pub expires_at: Option<u64>,
    pub members: BTreeMap<Vec<u8>, MemberDelta>,
}

/// 成员在磁盘与当前视图中的分数; `None` 表示不存在.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemberDelta {
    pub on_disk: Option<f64>,
    pub current: Option<f64>,
}

impl ZSetDelta {
    pub fn new(version: u64, count: u64, expires_at: Option<u64>) -> Self {
        Self {
            version,
            base_count: count,
            base_expires_at: expires_at,
            expires_at,
            members: BTreeMap::new(),
        }
    }

    /// 当前视图的成员数.
    pub fn visible_count(&self) -> u64 {
        let delta: i64 = self
            .members
            .values()
            .map(|m| m.current.is_some() as i64 - m.on_disk.is_some() as i64)
            .sum();
        (self.base_count as i64 + delta).max(0) as u64
    }

    /// 磁盘 header 是否带 TTL.
    pub fn base_had_ttl(&self) -> bool {
        self.base_expires_at.is_some()
    }

    /// 成员与过期时间都与磁盘一致, 提交时无需写入.
    pub fn is_noop(&self) -> bool {
        self.expires_at == self.base_expires_at
            && self.members.values().all(|m| m.on_disk == m.current)
    }

    /// 视图成员数落在内联阈值内, 提交时应降级为内联整值.
    pub fn demotes(&self) -> bool {
        let count = self.visible_count();
        count > 0 && count <= ZSET_DEMOTE_MEMBERS as u64
    }

    /// 展开为一次子键提交 (`Z`/`R` 增删与 header 改写). 成员数为 0 时删除 header.
    pub fn into_collection_write(&self) -> CollectionWrite {
        let mut puts = Vec::new();
        let mut deletes = Vec::new();
        for (member, change) in &self.members {
            if change.on_disk == change.current {
                continue;
            }
            if let Some(old) = change.on_disk {
                deletes.push((SubTag::ZScore, zset_score_sub(old, member)));
                if change.current.is_none() {
                    deletes.push((SubTag::ZMember, member.clone()));
                }
            }
            if let Some(new) = change.current {
                puts.push((
                    SubTag::ZMember,
                    member.clone(),
                    encode_zset_score(new).to_vec(),
                ));
                puts.push((SubTag::ZScore, zset_score_sub(new, member), Vec::new()));
            }
        }
        let count = self.visible_count();
        let header = if count == 0 {
            HeaderWrite::Delete {
                had_ttl: self.base_had_ttl(),
            }
        } else {
            HeaderWrite::Put(StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::ZSet,
                    count,
                    version: self.version,
                },
                expires_at: self.expires_at,
            })
        };
        CollectionWrite {
            header,
            version: self.version,
            puts,
            deletes,
        }
    }
}

/// 版本号中的生成时刻 (微秒).
pub fn version_micros(version: u64) -> u64 {
    version >> SEQ_BITS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 增量展开: 新增/改分/删除分别产生正确的 Z/R 写入与 header 计数.
    #[test]
    fn zset_delta_expands_to_member_writes() {
        let mut delta = ZSetDelta::new(9, 3, None);
        delta.members.insert(
            b"a".to_vec(),
            MemberDelta {
                on_disk: None,
                current: Some(1.0),
            },
        );
        delta.members.insert(
            b"b".to_vec(),
            MemberDelta {
                on_disk: Some(2.0),
                current: Some(5.0),
            },
        );
        delta.members.insert(
            b"c".to_vec(),
            MemberDelta {
                on_disk: Some(3.0),
                current: None,
            },
        );
        delta.members.insert(
            b"d".to_vec(),
            MemberDelta {
                on_disk: Some(4.0),
                current: Some(4.0),
            },
        );
        assert_eq!(delta.visible_count(), 3);
        let write = delta.into_collection_write();
        assert_eq!(write.version, 9);
        assert_eq!(write.deletes.len(), 3, "b old R, c Z and R");
        assert_eq!(write.puts.len(), 4, "a Z/R, b Z/R");
        assert!(matches!(
            write.header,
            HeaderWrite::Put(StoredValue {
                value: ValueType::CollectionHeader {
                    count: 3,
                    version: 9,
                    ..
                },
                ..
            })
        ));
    }

    /// 视图计数降到 0 时展开为删除 header, 并保留磁盘 TTL 状态.
    #[test]
    fn zset_delta_empty_view_deletes_header() {
        let mut delta = ZSetDelta::new(4, 1, Some(99));
        delta.members.insert(
            b"only".to_vec(),
            MemberDelta {
                on_disk: Some(1.0),
                current: None,
            },
        );
        assert!(!delta.demotes());
        assert!(matches!(
            delta.into_collection_write().header,
            HeaderWrite::Delete { had_ttl: true }
        ));
    }

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
