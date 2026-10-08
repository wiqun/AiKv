//! Subkey 编码: 将大集合 (Hash/Set) 的 field/member 存储为独立 key.
//!
//! # 编码方案
//!
//! ```text
//! Inline (小集合):    {encoded_user_key}
//!                     值: postcard(StoredValue)
//!
//! Subkey 元数据:     {encoded_user_key}  (ValueType::CollectionHeader)
//!
//! Hash field subkey: {encoded_user_key}\x01H{field_len:2B}{field}
//!                     值: raw bytes (field value)
//!
//! Set member subkey: {encoded_user_key}\x01S{member_len:2B}{member}
//!                     值: 空 (仅 key 存在性表示 member)
//! ```
//!
//! `\x01` 用于分隔 user key 和 subkey 部分.
//! 常规 key 不含 `\x01`, 因此 subkey entries 可被 `collect_db_entries` 过滤.

use crate::error::{Error, Result};
use crate::storage::types::CollectionKind;

/// Subkey 键编码相关常量.
const SUBKEY_SEPARATOR: u8 = 0x01;
const HASH_TAG: u8 = b'H';
const SET_TAG: u8 = b'S';
/// Subkey 中 field/member 长度字段的字节数 (u16).
const SUBKEY_LEN_BYTES: usize = 2;

// ---- 编码函数 ----

/// 构造 Hash field subkey 的完整 engine key.
///
/// 格式: `{user_key}\x01H{field_len:2B}{field}`
pub fn encode_hash_field_key(user_key: &[u8], field: &[u8]) -> Vec<u8> {
    encode_collection_subkey(user_key, HASH_TAG, field)
}

/// 构造 Set member subkey 的完整 engine key.
///
/// 格式: `{user_key}\x01S{member_len:2B}{member}`
pub fn encode_set_member_key(user_key: &[u8], member: &[u8]) -> Vec<u8> {
    encode_collection_subkey(user_key, SET_TAG, member)
}

/// 构造 Hash field 的扫描前缀.
///
/// 格式: `{user_key}\x01H`
pub fn hash_subkey_prefix(user_key: &[u8]) -> Vec<u8> {
    let mut prefix = Vec::with_capacity(user_key.len() + 2);
    prefix.extend_from_slice(user_key);
    prefix.push(SUBKEY_SEPARATOR);
    prefix.push(HASH_TAG);
    prefix
}

/// 构造 Set member 的扫描前缀.
///
/// 格式: `{user_key}\x01S`
pub fn set_subkey_prefix(user_key: &[u8]) -> Vec<u8> {
    let mut prefix = Vec::with_capacity(user_key.len() + 2);
    prefix.extend_from_slice(user_key);
    prefix.push(SUBKEY_SEPARATOR);
    prefix.push(SET_TAG);
    prefix
}

/// 父 key 里的 `0x01` 会让 `decode_subkey` 从错误的位置切开。
pub fn reject_subkey_parent(user_key: &[u8]) -> Result<()> {
    if user_key.contains(&SUBKEY_SEPARATOR) {
        Err(Error::Command(
            "ERR key must not contain a subkey separator".into(),
        ))
    } else {
        Ok(())
    }
}

/// field 长度按 u16 写入。更长的 field 会被截断，读回来对不上。
pub fn reject_subkey_field(field: &[u8]) -> Result<()> {
    if field.len() > u16::MAX as usize {
        Err(Error::Command("ERR field is too long".into()))
    } else {
        Ok(())
    }
}

fn encode_collection_subkey(user_key: &[u8], tag: u8, sub: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(user_key.len() + 3 + SUBKEY_LEN_BYTES + sub.len());
    key.extend_from_slice(user_key);
    key.push(SUBKEY_SEPARATOR);
    key.push(tag);
    let len = (sub.len() as u16).to_be_bytes();
    key.extend_from_slice(&len);
    key.extend_from_slice(sub);
    key
}

// ---- 解码函数 ----

/// 解码结果: (collection_kind, field_or_member_bytes).
pub fn decode_subkey(encoded_key: &[u8]) -> Option<(CollectionKind, Vec<u8>)> {
    let sep_pos = encoded_key.iter().position(|&b| b == SUBKEY_SEPARATOR)?;
    if sep_pos + 1 >= encoded_key.len() {
        return None;
    }
    let tag = encoded_key[sep_pos + 1];
    let kind = match tag {
        HASH_TAG => CollectionKind::Hash,
        SET_TAG => CollectionKind::Set,
        _ => return None,
    };
    // 跳过 tag + 2B 长度前缀
    let data_start = sep_pos + 4;
    if data_start > encoded_key.len() {
        return None;
    }
    let field = encoded_key[data_start..].to_vec();
    Some((kind, field))
}

/// 判断某个 engine key 是否为 subkey entry (field/member, 非 metadata).
///
/// 返回 user_key + subkey 标记; 常规 key/meta key 返回 None.
pub fn extract_user_key_from_subkey(encoded_key: &[u8]) -> Option<Vec<u8>> {
    let sep_pos = encoded_key.iter().position(|&b| b == SUBKEY_SEPARATOR)?;
    if sep_pos + 1 >= encoded_key.len() {
        return None;
    }
    let tag = encoded_key[sep_pos + 1];
    match tag {
        HASH_TAG | SET_TAG => Some(encoded_key[..sep_pos].to_vec()),
        _ => None,
    }
}

/// 检查是否为 regular key 或 metadata key (不含 subkey 分隔符).
pub fn is_not_subkey(encoded_key: &[u8]) -> bool {
    !encoded_key.contains(&SUBKEY_SEPARATOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 Hash 字段 Key 拼接编码与解码 Roundtrip
    #[test]
    fn test_hash_field_roundtrip() {
        let user_key = b"myhash";
        let field = b"email";
        let encoded = encode_hash_field_key(user_key, field);

        assert!(encoded.starts_with(user_key));
        assert_eq!(encoded[user_key.len()], SUBKEY_SEPARATOR);
        assert_eq!(encoded[user_key.len() + 1], HASH_TAG);

        let decoded = decode_subkey(&encoded);
        assert!(decoded.is_some());
        let (kind, field2) = decoded.unwrap();
        assert_eq!(kind, CollectionKind::Hash);
        assert_eq!(field2, field);
    }

    /// 验证 Set 成员 Key 编码与解码 Roundtrip
    #[test]
    fn test_set_member_roundtrip() {
        let user_key = b"myset";
        let member = b"alice";
        let encoded = encode_set_member_key(user_key, member);

        let decoded = decode_subkey(&encoded).unwrap();
        assert_eq!(decoded.0, CollectionKind::Set);
        assert_eq!(decoded.1, member);
    }

    /// 验证 普通 Key 与 SubKey 判定分割符
    #[test]
    fn test_regular_key_is_not_subkey() {
        assert!(is_not_subkey(b"0:mykey"));
        assert!(!is_not_subkey(b"0:mykey\x01H\x00\x05hello"));
    }

    /// 验证 SubKey 前缀构建格式 (\x01H / \x01S)
    #[test]
    fn test_subkey_prefixes() {
        let user_key = b"myhash";
        let h_prefix = hash_subkey_prefix(user_key);
        let s_prefix = set_subkey_prefix(user_key);

        assert_eq!(h_prefix, b"myhash\x01H");
        assert_eq!(s_prefix, b"myhash\x01S");
    }

    /// 验证 从复合 SubKey 中提取原始 UserKey
    #[test]
    fn test_extract_user_key_from_subkey() {
        let encoded = encode_hash_field_key(b"mykey", b"field1");
        let extracted = extract_user_key_from_subkey(&encoded);
        assert_eq!(extracted, Some(b"mykey".to_vec()));

        // Regular key returns None
        assert_eq!(extract_user_key_from_subkey(b"0:mykey"), None);
    }
}
