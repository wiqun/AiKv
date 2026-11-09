//! RESP 编码器: `RespValue` → 字节帧 (`RespValue::serialize` / `encode_into`), 与
//! `parser.rs` 对称.
//!
//! # 编码约定
//!
//! - 每个变体输出 marker + 载荷 + `\r\n`; 容器变体 (Array / Map / Set / Push /
//!   Attribute / StreamedString) 递归 `encode_into`; bulk 载荷二进制安全
//!   (`bytes::Bytes` 原样拷贝).
//! - RESP2 null 线格式: `BulkString(None)` → `$-1\r\n`, `Array(None)` → `*-1\r\n`.
//! - 特判: `Double` 的 nan / inf / -inf / -0; `VerbatimString` 要求 3 字符 format + `:`.
//!
//! # Invariant
//!
//! - 与解析器对称: `serialize()` 输出可被 `RespParser::parse` 完整恢复 (roundtrip).
//! - `ProtocolVersion` 不影响 `serialize()` / `encode_into` 输出; 版本相关 null 线格式适配
//!   (`$-1`/`*-1` → `_`) 在 `server/connection/protocol.rs` 的 `queue_response` / `adapt_null_to_resp3`.
//! - `serialize()` 是便利封装: 新建缓冲后调用 `encode_into`; 连接层优先直接 `encode_into`.

use bytes::{BufMut, Bytes, BytesMut};

use crate::protocol::types::RespValue;

#[inline]
fn sanitize_single_line(s: &str, buf: &mut BytesMut) {
    if !s.as_bytes().iter().any(|&b| b == b'\r' || b == b'\n') {
        buf.put_slice(s.as_bytes());
    } else {
        for &b in s.as_bytes() {
            if b == b'\r' || b == b'\n' {
                buf.put_u8(b' ');
            } else {
                buf.put_u8(b);
            }
        }
    }
}

impl RespValue {
    /// 编码为 RESP 帧
    pub fn serialize(&self) -> Bytes {
        let mut buf = BytesMut::new();
        self.encode_into(&mut buf);
        buf.freeze()
    }

    /// 将 RespValue 编码追加到提供的缓冲区中 (零新堆分配).
    pub fn encode_into(&self, buf: &mut BytesMut) {
        match self {
            RespValue::SimpleString(s) => {
                buf.put_slice(b"+");
                sanitize_single_line(s, buf);
                buf.put_slice(b"\r\n");
            }
            RespValue::Error(e) => {
                buf.put_slice(b"-");
                sanitize_single_line(e, buf);
                buf.put_slice(b"\r\n");
            }
            RespValue::Integer(i) => {
                buf.put_slice(b":");
                buf.put_slice(itoa::Buffer::new().format(*i).as_bytes());
                buf.put_slice(b"\r\n");
            }
            RespValue::BulkString(None) => buf.put_slice(b"$-1\r\n"),
            RespValue::BulkString(Some(b)) => {
                buf.put_slice(b"$");
                buf.put_slice(itoa::Buffer::new().format(b.len()).as_bytes());
                buf.put_slice(b"\r\n");
                buf.put_slice(b);
                buf.put_slice(b"\r\n");
            }
            RespValue::Array(None) => buf.put_slice(b"*-1\r\n"),
            RespValue::Array(Some(items)) => {
                buf.put_slice(b"*");
                buf.put_slice(itoa::Buffer::new().format(items.len()).as_bytes());
                buf.put_slice(b"\r\n");
                for item in items {
                    item.encode_into(buf);
                }
            }
            RespValue::Null => buf.put_slice(b"_\r\n"),
            RespValue::Boolean(b) => {
                if *b {
                    buf.put_slice(b"#t\r\n");
                } else {
                    buf.put_slice(b"#f\r\n");
                }
            }
            RespValue::Double(d) => {
                buf.put_slice(b",");
                if d.is_nan() {
                    buf.put_slice(b"nan");
                } else if *d == f64::INFINITY {
                    buf.put_slice(b"inf");
                } else if *d == f64::NEG_INFINITY {
                    buf.put_slice(b"-inf");
                } else if *d == 0.0 && d.is_sign_negative() {
                    buf.put_slice(b"-0");
                } else {
                    buf.put_slice(ryu::Buffer::new().format(*d).as_bytes());
                }
                buf.put_slice(b"\r\n");
            }
            RespValue::BigNumber(s) => {
                buf.put_slice(b"(");
                buf.put_slice(s.as_bytes());
                buf.put_slice(b"\r\n");
            }
            RespValue::BulkError(e) => {
                buf.put_slice(b"!");
                buf.put_slice(itoa::Buffer::new().format(e.len()).as_bytes());
                buf.put_slice(b"\r\n");
                buf.put_slice(e.as_bytes());
                buf.put_slice(b"\r\n");
            }
            RespValue::VerbatimString { format, data } => {
                let total = format.len() + 1 + data.len();
                buf.put_slice(b"=");
                buf.put_slice(itoa::Buffer::new().format(total).as_bytes());
                buf.put_slice(b"\r\n");
                buf.put_slice(format.as_bytes());
                buf.put_slice(b":");
                buf.put_slice(data);
                buf.put_slice(b"\r\n");
            }
            RespValue::Map(pairs) => {
                buf.put_slice(b"%");
                buf.put_slice(itoa::Buffer::new().format(pairs.len()).as_bytes());
                buf.put_slice(b"\r\n");
                for (k, v) in pairs {
                    k.encode_into(buf);
                    v.encode_into(buf);
                }
            }
            RespValue::Set(items) => {
                buf.put_slice(b"~");
                buf.put_slice(itoa::Buffer::new().format(items.len()).as_bytes());
                buf.put_slice(b"\r\n");
                for item in items {
                    item.encode_into(buf);
                }
            }
            RespValue::Push(items) => {
                buf.put_slice(b">");
                buf.put_slice(itoa::Buffer::new().format(items.len()).as_bytes());
                buf.put_slice(b"\r\n");
                for item in items {
                    item.encode_into(buf);
                }
            }
            RespValue::Attribute { attributes, data } => {
                buf.put_slice(b"|");
                buf.put_slice(itoa::Buffer::new().format(attributes.len()).as_bytes());
                buf.put_slice(b"\r\n");
                for (k, v) in attributes {
                    k.encode_into(buf);
                    v.encode_into(buf);
                }
                data.encode_into(buf);
            }
            RespValue::StreamedString(chunks) => {
                buf.put_slice(b"$?\r\n");
                for chunk in chunks {
                    buf.put_slice(b";");
                    buf.put_slice(itoa::Buffer::new().format(chunk.len()).as_bytes());
                    buf.put_slice(b"\r\n");
                    buf.put_slice(chunk);
                    buf.put_slice(b"\r\n");
                }
                buf.put_slice(b";0\r\n");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_into_matches_serialize_all_variants() {
        let variants: Vec<RespValue> = vec![
            RespValue::SimpleString("OK".into()),
            RespValue::Error("ERR test".into()),
            RespValue::Integer(12345),
            RespValue::BulkString(None),
            RespValue::BulkString(Some(Bytes::from_static(b"foobar"))),
            RespValue::Array(None),
            RespValue::Array(Some(vec![
                RespValue::Integer(1),
                RespValue::SimpleString("two".into()),
            ])),
            RespValue::Null,
            RespValue::Boolean(true),
            RespValue::Boolean(false),
            RespValue::Double(1.2345),
            RespValue::Double(f64::NAN),
            RespValue::Double(f64::INFINITY),
            RespValue::Double(f64::NEG_INFINITY),
            RespValue::Double(-0.0),
            RespValue::Double(0.0),
            RespValue::BigNumber("3492890328409238509324850943850943825024385".into()),
            RespValue::BulkError("SYNTAX invalid syntax".into()),
            RespValue::VerbatimString {
                format: "txt".into(),
                data: Bytes::from_static(b"some text\n"),
            },
            RespValue::Map(vec![
                (RespValue::SimpleString("k1".into()), RespValue::Integer(10)),
                (RespValue::SimpleString("k2".into()), RespValue::Integer(20)),
            ]),
            RespValue::Set(vec![
                RespValue::SimpleString("a".into()),
                RespValue::SimpleString("b".into()),
            ]),
            RespValue::Push(vec![
                RespValue::SimpleString("message".into()),
                RespValue::SimpleString("channel".into()),
            ]),
            RespValue::Attribute {
                attributes: vec![(
                    RespValue::SimpleString("ttl".into()),
                    RespValue::Integer(3600),
                )],
                data: Box::new(RespValue::SimpleString("data".into())),
            },
            RespValue::StreamedString(vec![
                Bytes::from_static(b"chunk1"),
                Bytes::from_static(b"chunk2"),
            ]),
        ];

        for val in &variants {
            let serialized = val.serialize();
            let mut buf = BytesMut::with_capacity(128);
            val.encode_into(&mut buf);
            assert_eq!(
                buf.as_ref(),
                serialized.as_ref(),
                "Mismatch for variant {:?}",
                val
            );
        }
    }

    #[test]
    fn test_encode_into_multi_frame_append() {
        let mut buf = BytesMut::with_capacity(256);
        buf.put_slice(b"PREFIX:");

        let frame1 = RespValue::SimpleString("PONG".into());
        let frame2 = RespValue::Integer(42);

        frame1.encode_into(&mut buf);
        frame2.encode_into(&mut buf);

        let mut expected = BytesMut::new();
        expected.put_slice(b"PREFIX:");
        expected.put_slice(&frame1.serialize());
        expected.put_slice(&frame2.serialize());

        assert_eq!(buf.as_ref(), expected.as_ref());
        assert_eq!(buf.as_ref(), b"PREFIX:+PONG\r\n:42\r\n");
    }

    #[test]
    fn test_sanitize_single_line_error_and_simple_string() {
        let err = RespValue::Error("ERR line1\nline2\rline3\r\nend".into());
        assert_eq!(err.serialize().as_ref(), b"-ERR line1 line2 line3  end\r\n");

        let simple = RespValue::SimpleString("hello\nworld".into());
        assert_eq!(simple.serialize().as_ref(), b"+hello world\r\n");
    }
}
