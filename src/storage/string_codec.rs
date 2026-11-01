//! 针对 String 类型的快速紧凑编解码与零拷贝切片提取.
//! 与 `postcard(StoredValue)` 二进制格式严格 100% 双向兼容.

use std::ops::Range;

#[derive(Debug, PartialEq, Eq)]
pub enum FastStringDecode {
    /// 成功识别 String: (payload 在 raw 中的字节区间, expires_at 毫秒)
    Valid(Range<usize>, Option<u64>),
    /// 明确检测到非 String 类型 (Hash, List, Set, ZSet 等)
    WrongType,
    /// 数据损坏或非常规结构, 需回退到 postcard 完整反序列化
    Fallback,
}

#[inline]
fn varint_usize_len(mut val: usize) -> usize {
    let mut len = 1;
    while val >= 0x80 {
        len += 1;
        val >>= 7;
    }
    len
}

#[inline]
fn encode_varint_usize(mut val: usize, buf: &mut Vec<u8>) {
    while val >= 0x80 {
        buf.push((val as u8 & 0x7f) | 0x80);
        val >>= 7;
    }
    buf.push(val as u8);
}

#[inline]
fn encode_varint_u64(mut val: u64, buf: &mut Vec<u8>) {
    while val >= 0x80 {
        buf.push((val as u8 & 0x7f) | 0x80);
        val >>= 7;
    }
    buf.push(val as u8);
}

#[inline]
fn decode_varint_usize(slice: &[u8]) -> Option<(usize, usize)> {
    let mut result = 0usize;
    let mut shift = 0;
    for (i, &byte) in slice.iter().enumerate() {
        if i >= 10 {
            return None;
        }
        let val = (byte & 0x7f) as usize;
        let shifted = val.checked_shl(shift)?;
        result |= shifted;
        if (byte & 0x80) == 0 {
            return Some((result, i + 1));
        }
        shift += 7;
        if shift >= usize::BITS {
            return None;
        }
    }
    None
}

#[inline]
fn decode_varint_u64(slice: &[u8]) -> Option<(u64, usize)> {
    let mut result = 0u64;
    let mut shift = 0;
    for (i, &byte) in slice.iter().enumerate() {
        if i >= 10 {
            return None;
        }
        let val = (byte & 0x7f) as u64;
        let shifted = val.checked_shl(shift)?;
        result |= shifted;
        if (byte & 0x80) == 0 {
            return Some((result, i + 1));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

/// 快速将 String 编码为 postcard 兼容的字节串, 避免 serde 反射与双重分配.
pub fn encode_string_fast(value: &[u8], expire_at: Option<u64>) -> Vec<u8> {
    let len_varint_len = varint_usize_len(value.len());
    let ttl_len = match expire_at {
        None => 1,
        Some(_) => 1 + 10,
    };
    let cap = 1 + len_varint_len + value.len() + ttl_len;
    let mut buf = Vec::with_capacity(cap);

    // ValueType::String 变体编号为 0
    buf.push(0x00);
    encode_varint_usize(value.len(), &mut buf);
    buf.extend_from_slice(value);

    // expires_at: Option<u64>
    match expire_at {
        None => buf.push(0x00),
        Some(exp) => {
            buf.push(0x01);
            encode_varint_u64(exp, &mut buf);
        }
    }

    buf
}

/// 快速从 raw 字节流中解析 String 的 payload 切片区间与过期时间.
pub fn decode_string_fast(raw: &[u8]) -> FastStringDecode {
    // 最小合法编码: [0x00, 0x00, 0x00] 代表空串 String(b"") 且 None
    if raw.len() < 3 {
        return FastStringDecode::Fallback;
    }

    // 检查 ValueType 变体编号
    if raw[0] != 0x00 {
        if (1..=5).contains(&raw[0]) {
            return FastStringDecode::WrongType;
        }
        return FastStringDecode::Fallback;
    }

    let Some((payload_len, varint_len)) = decode_varint_usize(&raw[1..]) else {
        return FastStringDecode::Fallback;
    };

    let payload_start = 1 + varint_len;
    let payload_end = match payload_start.checked_add(payload_len) {
        Some(end) if end <= raw.len() => end,
        _ => return FastStringDecode::Fallback,
    };

    let remaining = &raw[payload_end..];
    if remaining.is_empty() {
        return FastStringDecode::Fallback;
    }

    let expires_at = match remaining[0] {
        0x00 => {
            if remaining.len() != 1 {
                return FastStringDecode::Fallback;
            }
            None
        }
        0x01 => {
            let Some((exp, exp_varint_len)) = decode_varint_u64(&remaining[1..]) else {
                return FastStringDecode::Fallback;
            };
            if 1 + exp_varint_len != remaining.len() {
                return FastStringDecode::Fallback;
            }
            Some(exp)
        }
        _ => return FastStringDecode::Fallback,
    };

    FastStringDecode::Valid(payload_start..payload_end, expires_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::types::{StoredValue, ValueType};
    use std::collections::HashMap;

    #[test]
    fn test_encode_decode_roundtrip_with_postcard() {
        let test_cases: Vec<(&[u8], Option<u64>)> = vec![
            (b"", None),
            (b"", Some(0)),
            (b"", Some(1_700_000_000_000)),
            (b"a", None),
            (b"hello world", None),
            (b"hello world", Some(9_999_999_999)),
            (&[42u8; 127], None),
            (&[42u8; 128], Some(12345)),
            (&[42u8; 16383], None),
            (&[42u8; 16384], Some(99999)),
            (&[42u8; 70000], None),
        ];

        for (val, exp) in test_cases {
            let stored = StoredValue {
                value: ValueType::String(val.to_vec()),
                expires_at: exp,
            };

            // 1. 验证快速编码与 postcard::to_allocvec 严格一致
            let postcard_bytes = postcard::to_allocvec(&stored).unwrap();
            let fast_bytes = encode_string_fast(val, exp);
            assert_eq!(
                postcard_bytes,
                fast_bytes,
                "fast encode mismatch for len {}",
                val.len()
            );

            // 2. 验证快速解码 postcard_bytes
            match decode_string_fast(&postcard_bytes) {
                FastStringDecode::Valid(range, parsed_exp) => {
                    assert_eq!(&postcard_bytes[range], val);
                    assert_eq!(parsed_exp, exp);
                }
                other => panic!("expected Valid, got {:?}", other),
            }

            // 3. 验证快速解码 fast_bytes
            match decode_string_fast(&fast_bytes) {
                FastStringDecode::Valid(range, parsed_exp) => {
                    assert_eq!(&fast_bytes[range], val);
                    assert_eq!(parsed_exp, exp);
                }
                other => panic!("expected Valid, got {:?}", other),
            }
        }
    }

    #[test]
    fn test_wrongtype_detection() {
        let mut map = HashMap::new();
        map.insert(b"k1".to_vec(), b"v1".to_vec());
        let hash_val = StoredValue {
            value: ValueType::Hash(map),
            expires_at: None,
        };
        let hash_bytes = postcard::to_allocvec(&hash_val).unwrap();
        assert_eq!(decode_string_fast(&hash_bytes), FastStringDecode::WrongType);
    }

    #[test]
    fn test_corrupted_bytes_fallback() {
        assert_eq!(decode_string_fast(&[]), FastStringDecode::Fallback);
        assert_eq!(decode_string_fast(&[0x00]), FastStringDecode::Fallback);
        assert_eq!(
            decode_string_fast(&[0x00, 0x05]),
            FastStringDecode::Fallback
        );
        assert_eq!(
            decode_string_fast(&[0x00, 0x05, b'a', b'b', 0x00]),
            FastStringDecode::Fallback
        );
    }
}
