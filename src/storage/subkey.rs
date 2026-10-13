//! 大集合 (Hash/Set/ZSet) 的版本化 subkey 编码.
//!
//! ```text
//! header: {db}:{user_key}                                  值: postcard(StoredValue{CollectionHeader})
//! subkey: {db}\x01{len u32 BE}{user_key}{version u64 BE}{tag}{sub}
//!         tag = b'H' (Hash field, 值为 field 值)
//!             | b'S' (Set member, 值为空)
//!             | b'Z' (ZSet member, sub 为 member, 值为 score 的 f64 大端)
//!             | b'R' (ZSet 分数索引, sub 为 sortable(score) || member, 值为空)
//! ```
//!
//! `\x01 < b':'`, 所以 `{db}\x01` 与 `{db}:` 不共享前缀, 主键扫描看不到 subkey.
//! 只有版本与 header 相同的 subkey 可见; 其余由 compaction filter 回收.
//! 物理 key 不再依赖 user key 中不含分隔符, 任意字节的 key 均可使用.

use crate::error::{Error, Result};
use crate::storage::aidb::AiDbEngine;
use crate::storage::types::CollectionKind;

/// 版本化 subkey 键空间标记 `\x01`: `{db}\x01...` 与主键空间 `{db}:` 不共享前缀.
const SUBKEY_SPACE: u8 = 0x01;

/// subkey 的标签字节, 决定 subkey 属于集合中的哪一类数据.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubTag {
    /// Hash field → value.
    Hash,
    /// Set member (值为空).
    Set,
    /// ZSet member → score (值为 8 字节 f64 大端).
    ZMember,
    /// ZSet 分数索引 (sub 为 `sortable_score || member`, 值为空).
    ZScore,
}

impl SubTag {
    pub const fn byte(self) -> u8 {
        match self {
            SubTag::Hash => b'H',
            SubTag::Set => b'S',
            SubTag::ZMember => b'Z',
            SubTag::ZScore => b'R',
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'H' => Some(SubTag::Hash),
            b'S' => Some(SubTag::Set),
            b'Z' => Some(SubTag::ZMember),
            b'R' => Some(SubTag::ZScore),
            _ => None,
        }
    }

    pub fn kind(self) -> CollectionKind {
        match self {
            SubTag::Hash => CollectionKind::Hash,
            SubTag::Set => CollectionKind::Set,
            SubTag::ZMember | SubTag::ZScore => CollectionKind::ZSet,
        }
    }
}

/// 集合类型包含的全部 subkey 标签 (复制、展开时需遍历全部).
pub fn subkey_tags(kind: CollectionKind) -> &'static [SubTag] {
    match kind {
        CollectionKind::Hash => &[SubTag::Hash],
        CollectionKind::Set => &[SubTag::Set],
        CollectionKind::ZSet => &[SubTag::ZMember, SubTag::ZScore],
    }
}

// ---- 版本化 subkey 编解码 ----

/// subkey 的库前缀: `{db}\x01`.
pub fn subkey_db_prefix(db: usize) -> Vec<u8> {
    let mut prefix = db.to_string().into_bytes();
    prefix.push(SUBKEY_SPACE);
    prefix
}

/// 某个父 key 某个版本、某个标签的 subkey 扫描前缀.
///
/// 格式: `{db}\x01{user_key_len:u32 BE}{user_key}{version:u64 BE}{tag}`
pub fn subkey_version_prefix(db: usize, user_key: &[u8], version: u64, tag: SubTag) -> Vec<u8> {
    let mut prefix = subkey_db_prefix(db);
    prefix.reserve(4 + user_key.len() + 9);
    prefix.extend_from_slice(&(user_key.len() as u32).to_be_bytes());
    prefix.extend_from_slice(user_key);
    prefix.extend_from_slice(&version.to_be_bytes());
    prefix.push(tag.byte());
    prefix
}

/// 构造版本化 subkey 的完整物理 key.
pub fn encode_subkey(db: usize, user_key: &[u8], version: u64, tag: SubTag, sub: &[u8]) -> Vec<u8> {
    let mut key = subkey_version_prefix(db, user_key, version, tag);
    key.extend_from_slice(sub);
    key
}

/// ZSet 分数索引 (`R`) 的 sub: `sortable_score(score) || member`.
pub fn zset_score_sub(score: f64, member: &[u8]) -> Vec<u8> {
    let mut sub = Vec::with_capacity(8 + member.len());
    sub.extend_from_slice(&sortable_score(score));
    sub.extend_from_slice(member);
    sub
}

/// 分数的字节序可排序编码 (`R` 索引的前 8 字节).
///
/// `-0.0` 规范为 `+0.0`; 符号位为 1 时按位取反, 否则翻转符号位, 使字节序与数值序一致.
/// `±inf` 直接参与编码; NaN 由命令层在写入前拒绝.
pub fn sortable_score(score: f64) -> [u8; 8] {
    let score = if score == 0.0 { 0.0 } else { score };
    let bits = score.to_bits();
    let encoded = if bits >> 63 == 1 {
        !bits
    } else {
        bits ^ (1 << 63)
    };
    encoded.to_be_bytes()
}

/// 分数的逆编码 (`sortable_score` 的反函数).
pub fn unsortable_score(encoded: [u8; 8]) -> f64 {
    let e = u64::from_be_bytes(encoded);
    let bits = if e >> 63 == 1 { e ^ (1 << 63) } else { !e };
    f64::from_bits(bits)
}

/// 解析 `R` 索引的 sub: 返回 (score, member). 长度不足 8 字节时返回 None.
pub fn decode_zset_score_sub(sub: &[u8]) -> Option<(f64, &[u8])> {
    let raw: [u8; 8] = sub.get(..8)?.try_into().ok()?;
    Some((unsortable_score(raw), &sub[8..]))
}

/// 分数区间端点: `min`/`max` 为分数值 (无穷大直接参与比较), `*_inclusive` 为闭端点.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreSpan {
    pub min: f64,
    pub min_inclusive: bool,
    pub max: f64,
    pub max_inclusive: bool,
}

impl ScoreSpan {
    /// 全部分数 `[-inf, +inf]`.
    pub const FULL: ScoreSpan = ScoreSpan {
        min: f64::NEG_INFINITY,
        min_inclusive: true,
        max: f64::INFINITY,
        max_inclusive: true,
    };

    pub fn new(min: (f64, bool), max: (f64, bool)) -> Self {
        Self {
            min: min.0,
            min_inclusive: min.1,
            max: max.0,
            max_inclusive: max.1,
        }
    }

    pub fn contains(&self, score: f64) -> bool {
        let above = if self.min_inclusive {
            score >= self.min
        } else {
            score > self.min
        };
        let below = if self.max_inclusive {
            score <= self.max
        } else {
            score < self.max
        };
        above && below
    }
}

/// 某个 user key 某个版本的 `R` 索引中, 分数落在 `span` 内的物理 key 半开区间 `[start, end)`.
///
/// 空区间返回 None. 闭端点取 `sortable(score)` 的 8 字节块 (含该分数的全部成员),
/// 开端点取该块的后继, 因此 `(s` 与 `s` 的边界对同分数成员一致.
pub fn zset_score_range_keys(
    db: usize,
    user_key: &[u8],
    version: u64,
    span: &ScoreSpan,
) -> Option<(Vec<u8>, Vec<u8>)> {
    let prefix = subkey_version_prefix(db, user_key, version, SubTag::ZScore);
    let at = |score: f64| {
        let mut key = prefix.clone();
        key.extend_from_slice(&sortable_score(score));
        key
    };
    let successor = |key: Vec<u8>| {
        AiDbEngine::prefix_end(&key)
            .unwrap_or_else(|| AiDbEngine::prefix_end(&prefix).expect("tag byte is not 0xff"))
    };
    let start = if span.min_inclusive {
        at(span.min)
    } else {
        successor(at(span.min))
    };
    let end = if span.max_inclusive {
        successor(at(span.max))
    } else {
        at(span.max)
    };
    (start < end).then_some((start, end))
}

/// ZSet member → score 的值编码 (f64 大端).
pub fn encode_zset_score(score: f64) -> [u8; 8] {
    score.to_be_bytes()
}

/// 解码 `Z` subkey 的值; 长度不是 8 字节时返回 None.
pub fn decode_zset_score(bytes: &[u8]) -> Option<f64> {
    let raw: [u8; 8] = bytes.try_into().ok()?;
    Some(f64::from_be_bytes(raw))
}

/// 解码 `Z` subkey 的值, 损坏时返回存储错误.
pub fn decode_zset_score_or_err(bytes: &[u8]) -> Result<f64> {
    decode_zset_score(bytes).ok_or_else(|| Error::Storage("zset score subkey is corrupted".into()))
}

/// 版本化 subkey 解码结果; 借用原始物理 key.
#[derive(Debug, PartialEq, Eq)]
pub struct DecodedSubkey<'a> {
    pub db: usize,
    pub user_key: &'a [u8],
    pub version: u64,
    pub tag: SubTag,
    pub sub: &'a [u8],
}

/// 解码版本化 subkey. 主键 `{db}:{user_key}` 与非法输入返回 None.
pub fn decode_versioned_subkey(physical: &[u8]) -> Option<DecodedSubkey<'_>> {
    let sep = physical.iter().position(|&b| b == SUBKEY_SPACE)?;
    let digits = &physical[..sep];
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let db = std::str::from_utf8(digits).ok()?.parse().ok()?;
    let rest = &physical[sep + 1..];
    let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    let rest = &rest[4..];
    let user_key = rest.get(..len)?;
    let rest = &rest[len..];
    let version = u64::from_be_bytes(rest.get(..8)?.try_into().ok()?);
    let tag = SubTag::from_byte(*rest.get(8)?)?;
    Some(DecodedSubkey {
        db,
        user_key,
        version,
        tag,
        sub: &rest[9..],
    })
}

/// 集群路由用的 user key: subkey 取父 key, 主键取 `{db}:` 之后的部分.
///
/// subkey 与父 key 必须落在同一 slot, 否则集群模式下会路由到错误的 group.
pub fn routing_user_key(physical: &[u8]) -> Option<Vec<u8>> {
    if let Some(decoded) = decode_versioned_subkey(physical) {
        return Some(decoded.user_key.to_vec());
    }
    AiDbEngine::decode_key(physical).map(|(_, key)| key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 版本化 subkey 编码后应能无损解出 db、父 key (含任意字节)、版本、标签与 sub.
    #[test]
    fn versioned_subkey_roundtrip_with_any_key_bytes() {
        let key = b"a\x01b:c";
        let encoded = encode_subkey(3, key, 42, SubTag::Hash, b"f\x00ield");
        let decoded = decode_versioned_subkey(&encoded).expect("decode");
        assert_eq!(decoded.db, 3);
        assert_eq!(decoded.user_key, key);
        assert_eq!(decoded.version, 42);
        assert_eq!(decoded.tag, SubTag::Hash);
        assert_eq!(decoded.sub, b"f\x00ield");
    }

    /// 版本化 subkey 不得与主键 `{db}:` 前缀相交, 也不得跨 db 前缀匹配.
    #[test]
    fn versioned_subkeys_never_share_the_main_key_prefix() {
        let sub = encode_subkey(0, b"k", 1, SubTag::Set, b"m");
        assert!(!sub.starts_with(b"0:"));
        assert!(sub.starts_with(&subkey_db_prefix(0)));
        assert!(!encode_subkey(10, b"k", 1, SubTag::Set, b"m").starts_with(&subkey_db_prefix(1)));
    }

    /// 同一父 key 的不同版本互不匹配; 父 key 为前缀关系 (`k` vs `kk`) 时也互不匹配.
    #[test]
    fn version_prefix_isolates_versions() {
        let p1 = subkey_version_prefix(0, b"k", 1, SubTag::Hash);
        let v2 = encode_subkey(0, b"k", 2, SubTag::Hash, b"f");
        assert!(!v2.starts_with(&p1));
        let other = encode_subkey(0, b"kk", 1, SubTag::Hash, b"f");
        assert!(!other.starts_with(&p1));
    }

    /// 主键 (无 `\x01` 或不含版本段) 不能被误判为版本化 subkey.
    #[test]
    fn main_keys_are_not_versioned_subkeys() {
        assert!(decode_versioned_subkey(b"0:a\x01b").is_none());
        assert!(decode_versioned_subkey(b"0:plain").is_none());
    }

    /// ZSet 的 Z/R 两类 subkey 都应解码为 ZSet 集合, 且标签可区分.
    #[test]
    fn zset_tags_decode_to_zset_kind() {
        let z = encode_subkey(0, b"z", 7, SubTag::ZMember, b"m");
        let r = encode_subkey(0, b"z", 7, SubTag::ZScore, &zset_score_sub(1.5, b"m"));
        let dz = decode_versioned_subkey(&z).unwrap();
        let dr = decode_versioned_subkey(&r).unwrap();
        assert_eq!(dz.tag.kind(), CollectionKind::ZSet);
        assert_eq!(dr.tag.kind(), CollectionKind::ZSet);
        assert_eq!(dz.tag, SubTag::ZMember);
        assert_eq!(dr.tag, SubTag::ZScore);
        assert_eq!(dr.sub, zset_score_sub(1.5, b"m").as_slice());
    }

    /// `sortable_score` 的字节序必须与数值序一致 (含负数、-0/+0、±inf).
    #[test]
    fn sortable_score_preserves_numeric_order() {
        let values = [
            f64::NEG_INFINITY,
            -1e300,
            -2.5,
            -1.0,
            -f64::MIN_POSITIVE,
            0.0,
            f64::MIN_POSITIVE,
            1.0,
            2.5,
            1e300,
            f64::INFINITY,
        ];
        for pair in values.windows(2) {
            assert!(
                sortable_score(pair[0]) < sortable_score(pair[1]),
                "{pair:?}"
            );
        }
        assert_eq!(sortable_score(-0.0), sortable_score(0.0));
    }

    /// `Z` 分数值编码往返无损; 长度错误的值解码失败.
    #[test]
    fn zset_score_value_roundtrip() {
        for score in [0.0, -0.0, 1.25, -3.5, f64::INFINITY] {
            let encoded = encode_zset_score(score);
            assert_eq!(decode_zset_score(&encoded), Some(score));
        }
        assert_eq!(decode_zset_score(b"short"), None);
    }

    /// `unsortable_score` 必须是 `sortable_score` 的逆: 往返无损 (含 ±inf、负数、-0 规范为 +0).
    #[test]
    fn sortable_score_roundtrips() {
        for score in [
            f64::NEG_INFINITY,
            -1e300,
            -2.5,
            -0.0,
            0.0,
            1.5,
            1e300,
            f64::INFINITY,
        ] {
            let back = unsortable_score(sortable_score(score));
            assert_eq!(back, if score == 0.0 { 0.0 } else { score });
        }
    }

    /// 分数区间的开闭端点与同分数成员: `(1` 排除 1 的全部成员, `1` 包含之.
    #[test]
    fn score_range_keys_respect_endpoints() {
        let base = |s: f64, m: &[u8]| {
            let mut key = subkey_version_prefix(0, b"z", 5, SubTag::ZScore);
            key.extend_from_slice(&zset_score_sub(s, m));
            key
        };
        let span = ScoreSpan::new((1.0, false), (2.0, true));
        let (start, end) = zset_score_range_keys(0, b"z", 5, &span).unwrap();
        assert!(
            base(1.0, b"a") < start,
            "exclusive min excludes all members of 1"
        );
        assert!(base(1.5, b"a") >= start && base(1.5, b"a") < end);
        assert!(
            base(2.0, b"zz") < end,
            "inclusive max includes all members of 2"
        );
        assert!(base(2.5, b"a") >= end);
        assert!(
            zset_score_range_keys(0, b"z", 5, &ScoreSpan::new((3.0, true), (2.0, true))).is_none()
        );
        assert!(
            zset_score_range_keys(0, b"z", 5, &ScoreSpan::new((2.0, false), (2.0, false)))
                .is_none()
        );
    }

    /// subkey 与其父主键的路由 key 必须一致, 保证同 slot.
    #[test]
    fn routing_key_matches_parent_for_both_spaces() {
        let sub = encode_subkey(0, b"{tag}k", 9, SubTag::Hash, b"f");
        assert_eq!(routing_user_key(&sub).as_deref(), Some(b"{tag}k".as_ref()));
        assert_eq!(
            routing_user_key(b"0:{tag}k").as_deref(),
            Some(b"{tag}k".as_ref())
        );
    }
}
