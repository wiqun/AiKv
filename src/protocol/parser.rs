//! RESP 流式解析器: 维护字节 buffer, 支持 `feed` 追加 / `parse` 消费. 每次 `parse()`
//! 至多从 buffer 头部解析 **一个** 完整顶层 `RespValue`; 数据不足时返回 `Ok(None)`
//! 并保留 buffer 待后续 `feed`, 由此支撑 pipeline (连续帧留在同一 buffer).
//!
//! # 解析状态机
//!
//! ```text
//! TCP bytes ──feed──> BytesMut buffer
//!                         │
//! parse(): 冻结 buffer 为 Bytes, Cursor 顺序读
//!   ├─ parse_value(depth): 读 1 字节 marker ── 类型分发 ──┐
//!   │    + - :  → read_line (CRLF 结尾, max_line_len 校验)
//!   │    $     → parse_dollar: length 行 → parse_bulk (len + CRLF 校验)
//!   │           $? → parse_streamed_string (多块 chunk, ;0 终止)
//!   │    * % ~ > | → parse_length → 递归 parse_value (depth+1) 循环;
//!   │                % 与 | 为 pair 型 (key + value 两两读)
//!   │    _ # , ( ! = ; → 各自特判 (Null / Boolean / Double / BigNumber /
//!   │                     BulkError / VerbatimString / 顶层孤立 ; 拒绝)
//!   ├─ 完整帧   → 消费后保留剩余字节, 返回 Ok(Some(value))
//!   ├─ 数据不足 → cursor 回退到帧头, buffer 复原, 返回 Ok(None)
//!   ├─ 可恢复错误 → 跳过 1 字节 (unknown marker 跳过整行), 返回 Err (可重试)
//!   └─ 不可恢复错误 → buffer 不前进, 返回 Err (上层应断连)
//! ```
//!
//! # Invariant
//!
//! - 单帧语义: 每次 `parse()` 至多消费一个完整顶层帧; pipeline 由调用方循环 `parse()`
//!   (见 `server/connection/mod.rs` 的内层循环).
//! - 不完整不消费: 数据不足 → `Ok(None)`, cursor 回退, buffer 保留待 `feed`.
//! - 可恢复错误 (`is_recoverable`): `unknown type marker` 跳过整行 (消费到第一个 `\n`,
//!   无 `\n` 则消费整个 buffer), 其余跳过 1 字节后返回 `Err`; 调用方可写 ERR 响应并继续.
//! - 不可恢复错误 (depth / too large / buffer size / line too long / length 类):
//!   不 advance, 上层应断连.
//! - 默认 limits: `max_bulk_len` 512 MiB / `max_buffer_size` 64 MiB / `max_parse_depth` 128 /
//!   `max_array_len` 4 MiB 元素 / `max_line_len` 1 MiB; 生产路径 `new()` 使用默认值.

use std::io::Cursor;

use bytes::{Buf, Bytes, BytesMut};

use crate::error::{Error, Result};
use crate::protocol::types::RespValue;

const DEFAULT_MAX_BULK_LEN: usize = 512 * 1024 * 1024;
const DEFAULT_MAX_BUFFER_SIZE: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_PARSE_DEPTH: u8 = 128;
const DEFAULT_MAX_ARRAY_LEN: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_LINE_LEN: usize = 1024 * 1024;

/// RESP 帧解析器
#[derive(Debug)]
pub struct RespParser {
    buffer: BytesMut,
    max_bulk_len: usize,
    max_buffer_size: usize,
    max_parse_depth: u8,
    max_array_len: usize,
    max_line_len: usize,
}

impl RespParser {
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::with_capacity(8192),
            max_bulk_len: DEFAULT_MAX_BULK_LEN,
            max_buffer_size: DEFAULT_MAX_BUFFER_SIZE,
            max_parse_depth: DEFAULT_MAX_PARSE_DEPTH,
            max_array_len: DEFAULT_MAX_ARRAY_LEN,
            max_line_len: DEFAULT_MAX_LINE_LEN,
        }
    }

    pub fn with_limits(
        max_bulk_len: usize,
        max_buffer_size: usize,
        max_parse_depth: u8,
        max_array_len: usize,
        max_line_len: usize,
    ) -> Self {
        Self {
            buffer: BytesMut::with_capacity(8192),
            max_bulk_len,
            max_buffer_size,
            max_parse_depth,
            max_array_len,
            max_line_len,
        }
    }

    /// 使用连接级配置 (bulk / buffer / depth) 构造解析器, 其余沿用默认上限
    pub fn with_connection_limits(
        max_bulk_len: usize,
        max_buffer_size: usize,
        max_parse_depth: u8,
    ) -> Self {
        Self::with_limits(
            max_bulk_len,
            max_buffer_size,
            max_parse_depth,
            DEFAULT_MAX_ARRAY_LEN,
            DEFAULT_MAX_LINE_LEN,
        )
    }

    pub fn feed(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    pub fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    pub fn max_buffer_size(&self) -> usize {
        self.max_buffer_size
    }

    /// 尝试从 buffer 头部解析一个完整帧
    pub fn parse(&mut self) -> Result<Option<RespValue>> {
        if self.buffer.is_empty() {
            return Ok(None);
        }
        let start_len = self.buffer.len();

        // 冻结 buffer 为 Bytes 以支持零拷贝 slice() 操作.
        // BytesMut::from(Bytes) 是 O(1) 零拷贝, 解析完成后恢复.
        let owned = std::mem::take(&mut self.buffer);
        let frozen: Bytes = owned.freeze();
        let buf_len = frozen.len();
        let mut cursor = Cursor::new(&frozen[..]);

        match parse_value(&mut cursor, 0, self, &frozen) {
            Ok(Some(value)) => {
                let consumed = cursor.position() as usize;
                // 保留未消费部分, 零拷贝转回 BytesMut
                let remaining = frozen.slice(consumed..buf_len);
                self.buffer = BytesMut::from(remaining);
                Ok(Some(value))
            }
            Ok(None) => {
                // 不完整帧, 恢复整个 buffer
                self.buffer = BytesMut::from(frozen);
                Ok(None)
            }
            Err(e) if is_recoverable(&e) => {
                self.buffer = BytesMut::from(frozen);
                if is_unknown_marker(&e) {
                    // 跳过整行: 消费到第一个 \n (含); 无 \n 则消费整个 buffer,
                    // 避免对不完整行逐字节报错.
                    if let Some(nl) = self.buffer.iter().position(|&b| b == b'\n') {
                        self.buffer.advance(nl + 1);
                    } else {
                        self.buffer.advance(self.buffer.len());
                    }
                } else {
                    self.buffer.advance(1);
                }
                Err(e)
            }
            Err(e) => {
                let _ = start_len;
                self.buffer = BytesMut::from(frozen);
                Err(e)
            }
        }
    }
}

impl Default for RespParser {
    fn default() -> Self {
        Self::new()
    }
}

fn is_recoverable(err: &Error) -> bool {
    match err {
        Error::Protocol(msg) => {
            !msg.contains("depth")
                && !msg.contains("too large")
                && !msg.contains("buffer size")
                && !msg.contains("line too long")
                && !msg.contains("length overflow")
                && !msg.contains("invalid bulk length")
                && !msg.contains("invalid array length")
                && !msg.contains("invalid map length")
                && !msg.contains("invalid set length")
                && !msg.contains("invalid push length")
                && !msg.contains("invalid attribute length")
                && !msg.contains("invalid verbatim length")
                && !msg.contains("invalid bulk error length")
                && !msg.contains("length mismatch")
                && !msg.contains("malformed verbatim")
        }
        _ => false,
    }
}

fn is_unknown_marker(err: &Error) -> bool {
    matches!(err, Error::Protocol(msg) if msg.starts_with("unknown type marker"))
}

/// 判定协议错误是否不可恢复 (fatal): parser 对这类错误不消费 buffer,
/// 上层收到后应关闭连接. 与 `is_recoverable` 互补:
/// recoverable 错误跳过输入后继续解析; fatal 错误保持 buffer 不变.
///
/// 单一来源: server 层直接复用本判定, 勿自行维护另一份 contains 列表,
/// 否则 parser 不消费而 server 不断连会对同一 buffer 反复报错 (死循环).
pub fn is_fatal_protocol(err: &Error) -> bool {
    matches!(err, Error::Protocol(_)) && !is_recoverable(err)
}

fn parse_value(
    cursor: &mut Cursor<&[u8]>,
    depth: u8,
    parser: &RespParser,
    buffer: &Bytes,
) -> Result<Option<RespValue>> {
    if depth > parser.max_parse_depth {
        return Err(Error::Protocol("parse depth exceeded".into()));
    }

    let pos = cursor.position() as usize;
    let slice = cursor.get_ref();
    if pos >= slice.len() {
        return Ok(None);
    }

    let marker = slice[pos];
    cursor.set_position((pos + 1) as u64);

    match marker {
        b'+' => {
            read_line(cursor, parser).map(|o| o.map(|s| RespValue::SimpleString(s.to_string())))
        }
        b'-' => read_line(cursor, parser).map(|o| o.map(|s| RespValue::Error(s.to_string()))),
        b':' => read_line(cursor, parser).and_then(|o| match o {
            None => Ok(None),
            Some(s) => s
                .parse::<i64>()
                .map(RespValue::Integer)
                .map(Some)
                .map_err(|_| Error::Protocol(format!("invalid integer: {s}"))),
        }),
        b'$' => parse_dollar(cursor, depth, parser, buffer),
        b'*' => parse_array(cursor, depth, parser, buffer),
        b'_' => read_crlf(cursor).map(|o| o.map(|()| RespValue::Null)),
        b'#' => parse_boolean(cursor),
        b',' => parse_double(cursor, parser),
        b'(' => read_line(cursor, parser).map(|o| o.map(|s| RespValue::BigNumber(s.to_string()))),
        b'!' => parse_bulk_error(cursor, parser, buffer).map(|o| o.map(RespValue::BulkError)),
        b'=' => parse_verbatim_string(cursor, parser, buffer),
        b'%' => parse_map(cursor, depth, parser, buffer),
        b'~' => parse_set(cursor, depth, parser, buffer),
        b'>' => parse_push(cursor, depth, parser, buffer),
        b'|' => parse_attribute(cursor, depth, parser, buffer),
        b';' => Err(Error::Protocol("unexpected streamed chunk marker".into())),
        _ => Err(Error::Protocol(format!("unknown type marker: {marker}"))),
    }
}

fn read_line<'a>(cursor: &mut Cursor<&'a [u8]>, parser: &RespParser) -> Result<Option<&'a str>> {
    let start = cursor.position() as usize;
    let slice = cursor.get_ref();
    if start >= slice.len() {
        return Ok(None);
    }
    let remaining = &slice[start..];
    match remaining.iter().position(|&b| b == b'\r') {
        Some(cr_pos) if cr_pos + 1 < remaining.len() && remaining[cr_pos + 1] == b'\n' => {
            if cr_pos > parser.max_line_len {
                return Err(Error::Protocol("line too long".into()));
            }
            let line = std::str::from_utf8(&remaining[..cr_pos])
                .map_err(|_| Error::Protocol("invalid utf-8 in line".into()))?;
            cursor.set_position((start + cr_pos + 2) as u64);
            Ok(Some(line))
        }
        Some(_) => {
            cursor.set_position(start as u64);
            Ok(None)
        }
        None => {
            if remaining.len() > parser.max_line_len {
                return Err(Error::Protocol("line too long".into()));
            }
            cursor.set_position(start as u64);
            Ok(None)
        }
    }
}

fn read_crlf(cursor: &mut Cursor<&[u8]>) -> Result<Option<()>> {
    let start = cursor.position() as usize;
    let slice = cursor.get_ref();
    if start + 2 > slice.len() {
        cursor.set_position(start as u64);
        return Ok(None);
    }
    if slice[start] == b'\r' && slice[start + 1] == b'\n' {
        cursor.set_position((start + 2) as u64);
        Ok(Some(()))
    } else {
        Err(Error::Protocol("expected CRLF".into()))
    }
}

fn parse_length(cursor: &mut Cursor<&[u8]>, parser: &RespParser) -> Result<Option<i64>> {
    let line = match read_line(cursor, parser)? {
        Some(l) => l,
        None => return Ok(None),
    };
    line.parse::<i64>()
        .map(Some)
        .map_err(|_| Error::Protocol("length overflow".into()))
}

fn parse_bulk(
    cursor: &mut Cursor<&[u8]>,
    len: usize,
    parser: &RespParser,
    buffer: &Bytes,
) -> Result<Option<Bytes>> {
    if len > parser.max_bulk_len {
        return Err(Error::Protocol("bulk string too large".into()));
    }
    let start = cursor.position() as usize;
    let slice = cursor.get_ref();
    if start + len + 2 > slice.len() {
        cursor.set_position(start as u64);
        return Ok(None);
    }
    if slice[start + len] != b'\r' || slice[start + len + 1] != b'\n' {
        return Err(Error::Protocol("bulk string length mismatch".into()));
    }
    let data = buffer.slice_ref(&slice[start..start + len]);
    cursor.set_position((start + len + 2) as u64);
    Ok(Some(data))
}

fn parse_dollar(
    cursor: &mut Cursor<&[u8]>,
    _depth: u8,
    parser: &RespParser,
    buffer: &Bytes,
) -> Result<Option<RespValue>> {
    let marker_pos = cursor.position() as usize - 1;
    let slice = cursor.get_ref();
    let after_dollar = marker_pos + 1;
    if after_dollar < slice.len() && slice[after_dollar] == b'?' {
        cursor.set_position((after_dollar + 1) as u64);
        if read_crlf(cursor)?.is_none() {
            cursor.set_position(marker_pos as u64);
            return Ok(None);
        }
        return parse_streamed_string(cursor, parser, buffer);
    }

    let saved = cursor.position();
    let len = match parse_length(cursor, parser)? {
        Some(l) => l,
        None => {
            cursor.set_position(marker_pos as u64);
            return Ok(None);
        }
    };

    if len == -1 {
        return Ok(Some(RespValue::BulkString(None)));
    }
    if len < -1 {
        return Err(Error::Protocol(format!("invalid bulk length: {len}")));
    }
    let len = len as usize;
    match parse_bulk(cursor, len, parser, buffer)? {
        Some(data) => Ok(Some(RespValue::BulkString(Some(data)))),
        None => {
            cursor.set_position(saved - 1);
            Ok(None)
        }
    }
}

fn parse_streamed_string(
    cursor: &mut Cursor<&[u8]>,
    parser: &RespParser,
    buffer: &Bytes,
) -> Result<Option<RespValue>> {
    let mut chunks = Vec::new();
    loop {
        let pos = cursor.position() as usize;
        let slice = cursor.get_ref();
        if pos >= slice.len() {
            return Ok(None);
        }
        if slice[pos] != b';' {
            return Err(Error::Protocol("expected streamed chunk".into()));
        }
        cursor.set_position((pos + 1) as u64);
        let len = match parse_length(cursor, parser)? {
            Some(l) => l,
            None => {
                cursor.set_position(pos as u64);
                return Ok(None);
            }
        };
        if len < 0 {
            return Err(Error::Protocol("invalid streamed chunk length".into()));
        }
        if len == 0 {
            return Ok(Some(RespValue::StreamedString(chunks)));
        }
        let chunk = match parse_bulk(cursor, len as usize, parser, buffer)? {
            Some(c) => c,
            None => {
                cursor.set_position(pos as u64);
                return Ok(None);
            }
        };
        chunks.push(chunk);
    }
}

fn parse_array(
    cursor: &mut Cursor<&[u8]>,
    depth: u8,
    parser: &RespParser,
    buffer: &Bytes,
) -> Result<Option<RespValue>> {
    let saved = cursor.position() - 1;
    let len = match parse_length(cursor, parser)? {
        Some(l) => l,
        None => {
            cursor.set_position(saved);
            return Ok(None);
        }
    };
    if len == -1 {
        return Ok(Some(RespValue::Array(None)));
    }
    if len < -1 {
        return Err(Error::Protocol(format!("invalid array length: {len}")));
    }
    let len = len as usize;
    if len > parser.max_array_len {
        return Err(Error::Protocol("array too large".into()));
    }
    let mut items = Vec::with_capacity(len.min(1024));
    for _ in 0..len {
        match parse_value(cursor, depth + 1, parser, buffer)? {
            Some(v) => items.push(v),
            None => {
                cursor.set_position(saved);
                return Ok(None);
            }
        }
    }
    Ok(Some(RespValue::Array(Some(items))))
}

fn parse_map(
    cursor: &mut Cursor<&[u8]>,
    depth: u8,
    parser: &RespParser,
    buffer: &Bytes,
) -> Result<Option<RespValue>> {
    let saved = cursor.position() - 1;
    let len = match parse_length(cursor, parser)? {
        Some(l) => l,
        None => {
            cursor.set_position(saved);
            return Ok(None);
        }
    };
    if len < 0 {
        return Err(Error::Protocol(format!("invalid map length: {len}")));
    }
    let len = len as usize;
    if len > parser.max_array_len {
        return Err(Error::Protocol("map too large".into()));
    }
    let mut pairs = Vec::with_capacity(len.min(1024));
    for _ in 0..len {
        let key = match parse_value(cursor, depth + 1, parser, buffer)? {
            Some(v) => v,
            None => {
                cursor.set_position(saved);
                return Ok(None);
            }
        };
        let val = match parse_value(cursor, depth + 1, parser, buffer)? {
            Some(v) => v,
            None => {
                cursor.set_position(saved);
                return Ok(None);
            }
        };
        pairs.push((key, val));
    }
    Ok(Some(RespValue::Map(pairs)))
}

fn parse_set(
    cursor: &mut Cursor<&[u8]>,
    depth: u8,
    parser: &RespParser,
    buffer: &Bytes,
) -> Result<Option<RespValue>> {
    let saved = cursor.position() - 1;
    let len = match parse_length(cursor, parser)? {
        Some(l) => l,
        None => {
            cursor.set_position(saved);
            return Ok(None);
        }
    };
    if len < 0 {
        return Err(Error::Protocol(format!("invalid set length: {len}")));
    }
    let len = len as usize;
    if len > parser.max_array_len {
        return Err(Error::Protocol("set too large".into()));
    }
    let mut items = Vec::with_capacity(len.min(1024));
    for _ in 0..len {
        match parse_value(cursor, depth + 1, parser, buffer)? {
            Some(v) => items.push(v),
            None => {
                cursor.set_position(saved);
                return Ok(None);
            }
        }
    }
    Ok(Some(RespValue::Set(items)))
}

fn parse_push(
    cursor: &mut Cursor<&[u8]>,
    depth: u8,
    parser: &RespParser,
    buffer: &Bytes,
) -> Result<Option<RespValue>> {
    let saved = cursor.position() - 1;
    let len = match parse_length(cursor, parser)? {
        Some(l) => l,
        None => {
            cursor.set_position(saved);
            return Ok(None);
        }
    };
    if len < 0 {
        return Err(Error::Protocol(format!("invalid push length: {len}")));
    }
    let len = len as usize;
    if len > parser.max_array_len {
        return Err(Error::Protocol("push too large".into()));
    }
    let mut items = Vec::with_capacity(len.min(1024));
    for _ in 0..len {
        match parse_value(cursor, depth + 1, parser, buffer)? {
            Some(v) => items.push(v),
            None => {
                cursor.set_position(saved);
                return Ok(None);
            }
        }
    }
    Ok(Some(RespValue::Push(items)))
}

fn parse_attribute(
    cursor: &mut Cursor<&[u8]>,
    depth: u8,
    parser: &RespParser,
    buffer: &Bytes,
) -> Result<Option<RespValue>> {
    let saved = cursor.position() - 1;
    let len = match parse_length(cursor, parser)? {
        Some(l) => l,
        None => {
            cursor.set_position(saved);
            return Ok(None);
        }
    };
    if len < 0 {
        return Err(Error::Protocol(format!("invalid attribute length: {len}")));
    }
    let len = len as usize;
    if len > parser.max_array_len {
        return Err(Error::Protocol("attribute too large".into()));
    }
    let mut attrs = Vec::with_capacity(len.min(1024));
    for _ in 0..len {
        let key = match parse_value(cursor, depth + 1, parser, buffer)? {
            Some(v) => v,
            None => {
                cursor.set_position(saved);
                return Ok(None);
            }
        };
        let val = match parse_value(cursor, depth + 1, parser, buffer)? {
            Some(v) => v,
            None => {
                cursor.set_position(saved);
                return Ok(None);
            }
        };
        attrs.push((key, val));
    }
    let data = match parse_value(cursor, depth + 1, parser, buffer)? {
        Some(v) => v,
        None => {
            cursor.set_position(saved);
            return Ok(None);
        }
    };
    Ok(Some(RespValue::Attribute {
        attributes: attrs,
        data: Box::new(data),
    }))
}

fn parse_boolean(cursor: &mut Cursor<&[u8]>) -> Result<Option<RespValue>> {
    let start = cursor.position() as usize;
    let slice = cursor.get_ref();
    if start + 3 > slice.len() {
        cursor.set_position(start as u64);
        return Ok(None);
    }
    let val = match &slice[start..start + 3] {
        b"t\r\n" => true,
        b"f\r\n" => false,
        _ => return Err(Error::Protocol("invalid boolean".into())),
    };
    cursor.set_position((start + 3) as u64);
    Ok(Some(RespValue::Boolean(val)))
}

fn parse_double(cursor: &mut Cursor<&[u8]>, parser: &RespParser) -> Result<Option<RespValue>> {
    let line = match read_line(cursor, parser)? {
        Some(l) => l,
        None => return Ok(None),
    };
    let d = match line {
        "nan" => f64::NAN,
        "inf" => f64::INFINITY,
        "-inf" => f64::NEG_INFINITY,
        s => s
            .parse::<f64>()
            .map_err(|_| Error::Protocol(format!("invalid double: {line}")))?,
    };
    Ok(Some(RespValue::Double(d)))
}

fn parse_bulk_error(
    cursor: &mut Cursor<&[u8]>,
    parser: &RespParser,
    buffer: &Bytes,
) -> Result<Option<String>> {
    let saved = cursor.position() - 1;
    let len = match parse_length(cursor, parser)? {
        Some(l) => l,
        None => {
            cursor.set_position(saved);
            return Ok(None);
        }
    };
    if len < 0 {
        return Err(Error::Protocol(format!("invalid bulk error length: {len}")));
    }
    let data = match parse_bulk(cursor, len as usize, parser, buffer)? {
        Some(d) => d,
        None => {
            cursor.set_position(saved);
            return Ok(None);
        }
    };
    Ok(Some(
        std::str::from_utf8(&data)
            .map_err(|_| Error::Protocol("invalid utf-8 in bulk error".into()))?
            .to_string(),
    ))
}

fn parse_verbatim_string(
    cursor: &mut Cursor<&[u8]>,
    parser: &RespParser,
    buffer: &Bytes,
) -> Result<Option<RespValue>> {
    let saved = cursor.position() - 1;
    let len = match parse_length(cursor, parser)? {
        Some(l) => l,
        None => {
            cursor.set_position(saved);
            return Ok(None);
        }
    };
    if len < 0 {
        return Err(Error::Protocol(format!("invalid verbatim length: {len}")));
    }
    let data = match parse_bulk(cursor, len as usize, parser, buffer)? {
        Some(d) => d,
        None => {
            cursor.set_position(saved);
            return Ok(None);
        }
    };
    let s = std::str::from_utf8(&data)
        .map_err(|_| Error::Protocol("invalid utf-8 in verbatim string".into()))?;
    let Some((format, rest)) = s.split_once(':') else {
        return Err(Error::Protocol("malformed verbatim string".into()));
    };
    if format.len() != 3 {
        return Err(Error::Protocol("malformed verbatim string format".into()));
    }
    Ok(Some(RespValue::VerbatimString {
        format: format.to_string(),
        data: Bytes::copy_from_slice(rest.as_bytes()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_bulk_then_feed() {
        let mut p = RespParser::new();
        p.feed(b"$5\r\nhel");
        assert_eq!(p.parse().unwrap(), None);
        p.feed(b"lo\r\n");
        assert_eq!(
            p.parse().unwrap(),
            Some(RespValue::BulkString(Some(Bytes::from_static(b"hello"))))
        );
    }

    #[test]
    fn pipeline_two_frames() {
        let mut p = RespParser::new();
        p.feed(b"+OK\r\n:1\r\n");
        assert_eq!(
            p.parse().unwrap(),
            Some(RespValue::SimpleString("OK".into()))
        );
        assert_eq!(p.parse().unwrap(), Some(RespValue::Integer(1)));
        assert_eq!(p.parse().unwrap(), None);
    }

    #[test]
    fn serialize_roundtrip() {
        let values = [
            RespValue::SimpleString("OK".into()),
            RespValue::Integer(42),
            RespValue::BulkString(Some(Bytes::from_static(b"hi"))),
            RespValue::Array(Some(vec![
                RespValue::Integer(1),
                RespValue::SimpleString("x".into()),
            ])),
        ];
        for v in values {
            let mut p = RespParser::new();
            p.feed(&v.serialize());
            assert_eq!(p.parse().unwrap(), Some(v));
        }
    }

    #[test]
    fn unknown_marker_recoverable_then_next_frame() {
        let mut p = RespParser::new();
        p.feed(b"Xbad\r\n:2\r\n");
        let err = p.parse().unwrap_err();
        assert!(!is_fatal_protocol(&err));
        assert!(err.to_string().contains("unknown type marker"));
        assert_eq!(p.parse().unwrap(), Some(RespValue::Integer(2)));
    }

    #[test]
    fn nested_depth_exceeded_is_fatal() {
        let mut p = RespParser::with_limits(1024, 1024, 0, 1024, 1024);
        let data = b"*1\r\n:1\r\n";
        p.feed(data);
        let err = p.parse().unwrap_err();
        assert!(err.to_string().contains("parse depth exceeded"));
        assert!(is_fatal_protocol(&err));
        assert_eq!(p.buffer_len(), data.len());
    }

    #[test]
    fn bulk_too_large_is_fatal() {
        let mut p = RespParser::with_limits(4, 1024, 128, 1024, 1024);
        let data = b"$10\r\nabcdefghij\r\n";
        p.feed(data);
        let err = p.parse().unwrap_err();
        assert!(err.to_string().contains("bulk string too large"));
        assert!(is_fatal_protocol(&err));
        assert_eq!(p.buffer_len(), data.len());
    }

    #[test]
    fn connection_limits_rejects_oversized_bulk() {
        let mut p = RespParser::with_connection_limits(8, 1024, 16);
        let data = b"$16\r\n0123456789abcdef\r\n";
        p.feed(data);
        let err = p.parse().unwrap_err();
        assert!(err.to_string().contains("bulk string too large"));
    }
}
