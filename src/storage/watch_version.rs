//! WATCH 版本计数器 — 存储层 meta key, 随底层 WriteBatch/Raft 复制.
//!
//! 仅当本节点 `WatchRegistry` 显示有连接 WATCH 该 key 时才写入 (`#83`).
//! meta user key 形如 `{hash_tag}\xff\xff/aikv/watch/{user_key}`, 其中 `hash_tag`
//! 与 Redis Cluster / `aidb::cluster::router::extract_hash_tag` 相同, 保证
//! `key_to_slot(meta) == key_to_slot(user_key)` (Issue #83).

/// 保留前缀, 避免与用户 key 冲突; KEYS/SCAN 需过滤.
pub const WATCH_META_PREFIX: &[u8] = b"\xff\xff/aikv/watch/";

/// 提取 Redis hash tag (第一个 `{...}` 之间的内容). 无合法 tag 时返回完整 key.
pub(crate) fn extract_hash_tag(key: &[u8]) -> &[u8] {
    let Some(start) = key.iter().position(|&b| b == b'{') else {
        return key;
    };
    let after_open = &key[start + 1..];
    match after_open.iter().position(|&b| b == b'}') {
        Some(e) if e > 0 => &key[start + 1..start + 1 + e],
        _ => key,
    }
}

/// 若 `key` 以 `{tag}` 开头则返回 tag 之后的切片, 否则返回 `key`.
fn after_leading_hash_tag(key: &[u8]) -> &[u8] {
    if key.first() != Some(&b'{') {
        return key;
    }
    match key.iter().position(|&b| b == b'}') {
        Some(end) if end > 1 && end + 1 < key.len() => &key[end + 1..],
        _ => key,
    }
}

#[inline]
pub fn meta_user_key(user_key: &[u8]) -> Vec<u8> {
    let tag = extract_hash_tag(user_key);
    let mut out = Vec::with_capacity(2 + tag.len() + WATCH_META_PREFIX.len() + user_key.len());
    out.push(b'{');
    out.extend_from_slice(tag);
    out.push(b'}');
    out.extend_from_slice(WATCH_META_PREFIX);
    out.extend_from_slice(user_key);
    out
}

#[inline]
pub fn is_watch_meta_user_key(key: &[u8]) -> bool {
    key.starts_with(WATCH_META_PREFIX) || after_leading_hash_tag(key).starts_with(WATCH_META_PREFIX)
}

#[inline]
pub fn encode_version(version: u64) -> [u8; 8] {
    version.to_be_bytes()
}

#[inline]
pub fn decode_version(bytes: &[u8]) -> u64 {
    if bytes.len() >= 8 {
        u64::from_be_bytes(bytes[..8].try_into().expect("slice length checked"))
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_version_bytes() {
        assert_eq!(decode_version(&encode_version(42)), 42);
        assert_eq!(decode_version(&encode_version(0)), 0);
    }

    #[test]
    fn meta_prefix_detected() {
        let meta = meta_user_key(b"mykey");
        assert!(is_watch_meta_user_key(&meta));
        assert!(!is_watch_meta_user_key(b"mykey"));
    }

    /// Issue #83: meta 须带与 user key 相同的 Redis hash tag, 否则 CRC16 跨 slot,
    /// 集群 SET 会把 version 写进错误 group.
    #[test]
    fn meta_key_wraps_plain_user_key_as_hash_tag() {
        let meta = meta_user_key(b"foo");
        assert!(
            meta.starts_with(b"{foo}"),
            "plain key must be wrapped as {{foo}}... so key_to_slot matches, got {:?}",
            meta
        );
        assert!(is_watch_meta_user_key(&meta));
    }

    /// Issue #83: 已有 `{tag}` 的 user key, meta 必须复用同一 tag.
    #[test]
    fn meta_key_reuses_existing_hash_tag() {
        let meta = meta_user_key(b"user:{123}:data");
        assert!(
            meta.starts_with(b"{123}"),
            "hash tag 123 must be reused, got {:?}",
            meta
        );
        assert!(is_watch_meta_user_key(&meta));
    }
}
