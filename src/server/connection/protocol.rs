//! 连接响应编码: `queue_response` / `queue_error` 写入 `write_buf`,
//! `flush_responses` 聚合 `write_all`; RESP3 null 适配.

use tokio::io::AsyncWriteExt;

use crate::error::{Error, Result};
use crate::protocol::{ProtocolVersion, RespValue};

use super::Connection;

pub const DEFAULT_WRITE_BUF_CAPACITY: usize = 8 * 1024;
pub const MAX_WRITE_BUF_RETAIN_CAPACITY: usize = 64 * 1024;

impl Connection {
    pub(super) fn queue_error(&mut self, err: &Error) -> Result<()> {
        let msg = match err {
            Error::Protocol(s) => format!("Protocol error: {s}"),
            Error::Command(s) => s.clone(),
            Error::Io(e) => return Err(Error::Io(std::io::Error::new(e.kind(), e.to_string()))),
            Error::Storage(s) => format!("Internal storage error: {s}"),
            Error::Config(s) => format!("CONFIG error: {s}"),
            #[cfg(feature = "cluster")]
            Error::Cluster(s) => format!("CLUSTER error: {s}"),
        };
        self.queue_response(&RespValue::Error(msg));
        Ok(())
    }

    pub(super) async fn write_error(&mut self, err: &Error) -> Result<()> {
        self.queue_error(err)
    }

    pub(super) fn queue_response(&mut self, value: &RespValue) {
        // 仅 RESP3 协商后才做 null 适配, 否则直接流式编码 (免克隆, F-034)
        if self.protocol_negotiated && self.protocol_version == ProtocolVersion::Resp3 {
            self.adapt_null_to_resp3(value)
                .encode_into(&mut self.write_buf);
        } else {
            value.encode_into(&mut self.write_buf);
        }
    }

    pub(super) async fn write_response(&mut self, value: RespValue) -> Result<()> {
        self.queue_response(&value);
        Ok(())
    }

    pub(super) async fn flush_responses(&mut self) -> Result<()> {
        if self.write_buf.is_empty() {
            return Ok(());
        }
        let len = self.write_buf.len();
        self.stream.write_all(&self.write_buf).await?;
        self.state.metrics().on_net_output_bytes(len as u64);
        tracing::debug!(response_size = len, "kv.write.complete");
        self.write_buf.clear();
        if self.write_buf.capacity() > MAX_WRITE_BUF_RETAIN_CAPACITY {
            self.write_buf = bytes::BytesMut::with_capacity(DEFAULT_WRITE_BUF_CAPACITY);
        }
        Ok(())
    }

    /// 递归检查 RespValue 树中是否包含 RESP2 风格的 null
    /// (BulkString(None) 或 Array(None)),用于免克隆短路 (F-034).
    fn contains_resp_null(value: &RespValue) -> bool {
        match value {
            RespValue::BulkString(None) | RespValue::Array(None) => true,
            RespValue::Array(Some(items)) => items.iter().any(Self::contains_resp_null),
            RespValue::Map(pairs) => pairs
                .iter()
                .any(|(k, v)| Self::contains_resp_null(k) || Self::contains_resp_null(v)),
            RespValue::Set(items) => items.iter().any(Self::contains_resp_null),
            RespValue::Push(items) => items.iter().any(Self::contains_resp_null),
            RespValue::Attribute { attributes, data } => {
                attributes
                    .iter()
                    .any(|(k, v)| Self::contains_resp_null(k) || Self::contains_resp_null(v))
                    || Self::contains_resp_null(data)
            }
            _ => false,
        }
    }

    /// RESP3 模式下将 RESP2 风格的 null 表示转为 RESP3 原生 Null.
    /// redis-py 8.0 的 RESP3 解析器对 `$-1\r\n` / `*-1\r\n` 处理有兼容性问题,
    /// 需使用 RESP3 原生 `_\r\n` (Null) 替代.
    fn adapt_null_to_resp3(&self, value: &RespValue) -> RespValue {
        match value {
            RespValue::BulkString(None) | RespValue::Array(None) => RespValue::Null,
            RespValue::Array(Some(items)) => {
                if !items.iter().any(Self::contains_resp_null) {
                    return value.clone();
                }
                RespValue::Array(Some(
                    items.iter().map(|v| self.adapt_null_to_resp3(v)).collect(),
                ))
            }
            RespValue::Map(pairs) => {
                if !pairs
                    .iter()
                    .any(|(k, v)| Self::contains_resp_null(k) || Self::contains_resp_null(v))
                {
                    return value.clone();
                }
                RespValue::Map(
                    pairs
                        .iter()
                        .map(|(k, v)| (self.adapt_null_to_resp3(k), self.adapt_null_to_resp3(v)))
                        .collect(),
                )
            }
            RespValue::Set(items) => {
                if !items.iter().any(Self::contains_resp_null) {
                    return value.clone();
                }
                RespValue::Set(items.iter().map(|v| self.adapt_null_to_resp3(v)).collect())
            }
            RespValue::Push(items) => {
                if !items.iter().any(Self::contains_resp_null) {
                    return value.clone();
                }
                RespValue::Push(items.iter().map(|v| self.adapt_null_to_resp3(v)).collect())
            }
            RespValue::Attribute { attributes, data } => {
                if !attributes
                    .iter()
                    .any(|(k, v)| Self::contains_resp_null(k) || Self::contains_resp_null(v))
                    && !Self::contains_resp_null(data)
                {
                    return value.clone();
                }
                RespValue::Attribute {
                    attributes: attributes
                        .iter()
                        .map(|(k, v)| (self.adapt_null_to_resp3(k), self.adapt_null_to_resp3(v)))
                        .collect(),
                    data: Box::new(self.adapt_null_to_resp3(data)),
                }
            }
            other => other.clone(),
        }
    }
}
