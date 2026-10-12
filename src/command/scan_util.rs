//! SCAN 族命令共享 helper (SSCAN, ZSCAN 等)

use bytes::Bytes;

use crate::command::router;
use crate::error::{Error, Result};
use crate::protocol::RespValue;

pub(crate) struct ScanOptions {
    pub pattern: Option<Vec<u8>>,
    pub count: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            pattern: None,
            count: 10,
        }
    }
}

pub(crate) fn parse_scan_options(cmd: &str, args: &[Bytes], start: usize) -> Result<ScanOptions> {
    let mut opts = ScanOptions::default();
    let mut i = start;
    while i < args.len() {
        if eq_ignore_case(&args[i], b"MATCH") {
            if i + 1 >= args.len() {
                return Err(router::wrong_args(cmd, ""));
            }
            opts.pattern = Some(args[i + 1].to_vec());
            i += 2;
        } else if eq_ignore_case(&args[i], b"COUNT") {
            if i + 1 >= args.len() {
                return Err(router::wrong_args(cmd, ""));
            }
            opts.count = parse_i64(&args[i + 1])? as usize;
            i += 2;
        } else {
            return Err(router::wrong_args(cmd, ""));
        }
    }
    Ok(opts)
}

/// 按游标切一页. 游标超过长度时返回空页与游标 0, 不越界.
pub(crate) fn paginate_slice<T>(entries: &[T], cursor: u64, count: usize) -> (u64, &[T]) {
    let skip = usize::try_from(cursor)
        .unwrap_or(usize::MAX)
        .min(entries.len());
    let end = skip.saturating_add(count).min(entries.len());
    let next_cursor = if end < entries.len() { end as u64 } else { 0 };
    (next_cursor, &entries[skip..end])
}

pub(crate) fn scan_response_bulk(cursor: u64, members: &[Vec<u8>]) -> RespValue {
    let mut items = vec![RespValue::BulkString(Some(Bytes::from(cursor.to_string())))];
    items.push(RespValue::Array(Some(
        members.iter().cloned().map(router::bulk).collect(),
    )));
    RespValue::Array(Some(items))
}

pub(crate) fn parse_u64(b: &Bytes) -> Result<u64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::Command("ERR invalid cursor".into()))?;
    s.parse::<u64>()
        .map_err(|_| Error::Command("ERR invalid cursor".into()))
}

fn parse_i64(b: &Bytes) -> Result<i64> {
    let s =
        std::str::from_utf8(b).map_err(|_| Error::Command("ERR value is not an integer".into()))?;
    s.parse::<i64>()
        .map_err(|_| Error::Command("ERR value is not an integer".into()))
}

fn eq_ignore_case(a: &Bytes, b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.eq_ignore_ascii_case(y))
}
