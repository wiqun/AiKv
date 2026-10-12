//! 大集合 (Hash/Set) 的版本化 subkey 编码.
//!
//! ```text
//! header: {db}:{user_key}                                  值: postcard(StoredValue{CollectionHeader})
//! subkey: {db}\x01{len u32 BE}{user_key}{version u64 BE}{tag}{sub}
//!         tag = b'H' (值为 field 值) | b'S' (值为空)
//! ```
//!
//! `\x01 < b':'`, 所以 `{db}\x01` 与 `{db}:` 不共享前缀, 主键扫描看不到 subkey.
//! 只有版本与 header 相同的 subkey 可见; 其余由 compaction filter 回收.
//! 物理 key 不再依赖 user key 中不含分隔符, 任意字节的 key 均可使用.

use crate::storage::aidb::AiDbEngine;
use crate::storage::types::CollectionKind;

/// Hash field subkey 的标签字节.
const HASH_TAG: u8 = b'H';
/// Set member subkey 的标签字节.
const SET_TAG: u8 = b'S';

// ---- 版本化 subkey 编解码 ----

/// 版本化 subkey 键空间标记 `\x01`: `{db}\x01...` 与主键空间 `{db}:` 不共享前缀.
const SUBKEY_SPACE: u8 = 0x01;

/// 集合类型对应的 subkey 标签字节.
pub fn kind_tag(kind: CollectionKind) -> u8 {
    match kind {
        CollectionKind::Hash => HASH_TAG,
        CollectionKind::Set => SET_TAG,
    }
}

fn kind_from_tag(tag: u8) -> Option<CollectionKind> {
    match tag {
        HASH_TAG => Some(CollectionKind::Hash),
        SET_TAG => Some(CollectionKind::Set),
        _ => None,
    }
}

/// 版本化 subkey 的库前缀: `{db}\x01`.
pub fn subkey_db_prefix(db: usize) -> Vec<u8> {
    let mut prefix = db.to_string().into_bytes();
    prefix.push(SUBKEY_SPACE);
    prefix
}

/// 某个父 key 某个版本的 subkey 扫描前缀.
///
/// 格式: `{db}\x01{user_key_len:u32 BE}{user_key}{version:u64 BE}{tag}`
pub fn subkey_version_prefix(
    db: usize,
    user_key: &[u8],
    version: u64,
    kind: CollectionKind,
) -> Vec<u8> {
    let mut prefix = subkey_db_prefix(db);
    prefix.reserve(4 + user_key.len() + 9);
    prefix.extend_from_slice(&(user_key.len() as u32).to_be_bytes());
    prefix.extend_from_slice(user_key);
    prefix.extend_from_slice(&version.to_be_bytes());
    prefix.push(kind_tag(kind));
    prefix
}

/// 构造版本化 subkey 的完整物理 key.
pub fn encode_subkey(
    db: usize,
    user_key: &[u8],
    version: u64,
    kind: CollectionKind,
    sub: &[u8],
) -> Vec<u8> {
    let mut key = subkey_version_prefix(db, user_key, version, kind);
    key.extend_from_slice(sub);
    key
}

/// 版本化 subkey 解码结果; 借用原始物理 key.
#[derive(Debug, PartialEq, Eq)]
pub struct DecodedSubkey<'a> {
    pub db: usize,
    pub user_key: &'a [u8],
    pub version: u64,
    pub kind: CollectionKind,
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
    let kind = kind_from_tag(*rest.get(8)?)?;
    Some(DecodedSubkey {
        db,
        user_key,
        version,
        kind,
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

    /// 版本化 subkey 编码后应能无损解出 db、父 key (含任意字节)、版本、类型与 sub.
    #[test]
    fn versioned_subkey_roundtrip_with_any_key_bytes() {
        let key = b"a\x01b:c";
        let encoded = encode_subkey(3, key, 42, CollectionKind::Hash, b"f\x00ield");
        let decoded = decode_versioned_subkey(&encoded).expect("decode");
        assert_eq!(decoded.db, 3);
        assert_eq!(decoded.user_key, key);
        assert_eq!(decoded.version, 42);
        assert_eq!(decoded.kind, CollectionKind::Hash);
        assert_eq!(decoded.sub, b"f\x00ield");
    }

    /// 版本化 subkey 不得与主键 `{db}:` 前缀相交, 也不得跨 db 前缀匹配.
    #[test]
    fn versioned_subkeys_never_share_the_main_key_prefix() {
        let sub = encode_subkey(0, b"k", 1, CollectionKind::Set, b"m");
        assert!(!sub.starts_with(b"0:"));
        assert!(sub.starts_with(&subkey_db_prefix(0)));
        assert!(!encode_subkey(10, b"k", 1, CollectionKind::Set, b"m")
            .starts_with(&subkey_db_prefix(1)));
    }

    /// 同一父 key 的不同版本互不匹配; 父 key 为前缀关系 (`k` vs `kk`) 时也互不匹配.
    #[test]
    fn version_prefix_isolates_versions() {
        let p1 = subkey_version_prefix(0, b"k", 1, CollectionKind::Hash);
        let v2 = encode_subkey(0, b"k", 2, CollectionKind::Hash, b"f");
        assert!(!v2.starts_with(&p1));
        let other = encode_subkey(0, b"kk", 1, CollectionKind::Hash, b"f");
        assert!(!other.starts_with(&p1));
    }

    /// 主键 (无 `\x01` 或不含版本段) 不能被误判为版本化 subkey.
    #[test]
    fn main_keys_are_not_versioned_subkeys() {
        assert!(decode_versioned_subkey(b"0:a\x01b").is_none());
        assert!(decode_versioned_subkey(b"0:plain").is_none());
    }

    /// subkey 与其父主键的路由 key 必须一致, 保证同 slot.
    #[test]
    fn routing_key_matches_parent_for_both_spaces() {
        let sub = encode_subkey(0, b"{tag}k", 9, CollectionKind::Hash, b"f");
        assert_eq!(routing_user_key(&sub).as_deref(), Some(b"{tag}k".as_ref()));
        assert_eq!(
            routing_user_key(b"0:{tag}k").as_deref(),
            Some(b"{tag}k".as_ref())
        );
    }
}
