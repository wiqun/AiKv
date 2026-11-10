//! String 命令参数解析与二进制切片工具函数.

use bytes::Bytes;

use crate::command::router;
use crate::error::{Error, Result};
use crate::storage::{now_ms, WRONGTYPE};

use super::SetCondition;

pub(crate) struct SetOptions {
    pub(crate) key: Vec<u8>,
    pub(crate) value: Vec<u8>,
    pub(crate) expire_at: Option<u64>,
    pub(crate) condition: SetCondition,
    pub(crate) return_old: bool,
    pub(crate) keep_ttl: bool,
}

pub(crate) fn parse_set_options(args: &[Bytes]) -> Result<SetOptions> {
    let key = args[0].to_vec();
    let value = args[1].to_vec();
    let mut expire_at: Option<u64> = None;
    let mut condition = SetCondition::None;
    let mut return_old = false;
    let mut keep_ttl = false;
    let mut i = 2;
    while i < args.len() {
        let opt = String::from_utf8_lossy(&args[i]).to_ascii_uppercase();
        match opt.as_str() {
            "EX" => {
                if expire_at.is_some() {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                i += 1;
                if i >= args.len() {
                    return Err(router::wrong_args("SET", ""));
                }
                let secs = parse_u64_arg(&args[i], "SET")?;
                if secs == 0 {
                    return Err(Error::Command(
                        "ERR invalid expire time in 'set' command".into(),
                    ));
                }
                expire_at = Some(now_ms().saturating_add(secs.saturating_mul(1000)));
            }
            "PX" => {
                if expire_at.is_some() {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                i += 1;
                if i >= args.len() {
                    return Err(router::wrong_args("SET", ""));
                }
                let ms = parse_u64_arg(&args[i], "SET")?;
                if ms == 0 {
                    return Err(Error::Command(
                        "ERR invalid expire time in 'set' command".into(),
                    ));
                }
                expire_at = Some(now_ms().saturating_add(ms));
            }
            "EXAT" => {
                if expire_at.is_some() {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                i += 1;
                if i >= args.len() {
                    return Err(router::wrong_args("SET", ""));
                }
                let ts_secs = parse_u64_arg(&args[i], "SET")?;
                if ts_secs == 0 {
                    return Err(Error::Command(
                        "ERR invalid expire time in 'set' command".into(),
                    ));
                }
                expire_at = Some(ts_secs.saturating_mul(1000));
            }
            "PXAT" => {
                if expire_at.is_some() {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                i += 1;
                if i >= args.len() {
                    return Err(router::wrong_args("SET", ""));
                }
                let ts_ms = parse_u64_arg(&args[i], "SET")?;
                if ts_ms == 0 {
                    return Err(Error::Command(
                        "ERR invalid expire time in 'set' command".into(),
                    ));
                }
                expire_at = Some(ts_ms);
            }
            "NX" => {
                if condition != SetCondition::None {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                condition = SetCondition::Nx;
            }
            "XX" => {
                if condition != SetCondition::None {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                condition = SetCondition::Xx;
            }
            "GET" => {
                return_old = true;
            }
            "KEEPTTL" => {
                keep_ttl = true;
            }
            _ => return Err(Error::Command("ERR syntax error".into())),
        }
        i += 1;
    }
    Ok(SetOptions {
        key,
        value,
        expire_at,
        condition,
        return_old,
        keep_ttl,
    })
}

/// Redis bit offset: 0 is the leftmost bit of the first byte.
pub(crate) fn read_bit(data: &[u8], offset: u64) -> u8 {
    let byte_idx = (offset / 8) as usize;
    let bit_pos = 7 - (offset % 8);
    let byte = data.get(byte_idx).copied().unwrap_or(0);
    (byte >> bit_pos) & 1
}

pub(crate) fn write_bit(data: &mut Vec<u8>, offset: u64, bit: u8) {
    let byte_idx = (offset / 8) as usize;
    let bit_pos = 7 - (offset % 8);
    if data.len() <= byte_idx {
        data.resize(byte_idx + 1, 0);
    }
    let mask = 1u8 << bit_pos;
    if bit == 1 {
        data[byte_idx] |= mask;
    } else {
        data[byte_idx] &= !mask;
    }
}

/// Redis GETRANGE inclusive byte range (negative indices count from end).
pub(crate) fn string_range_slice(value: &[u8], start: i64, end: i64) -> Vec<u8> {
    let len = value.len() as i64;
    if len == 0 {
        return Vec::new();
    }

    let start_idx = if start < 0 {
        (len + start).max(0)
    } else {
        start.min(len)
    } as usize;
    let end_idx = if end < 0 {
        let e = len + end;
        if e < 0 {
            return Vec::new();
        }
        e as usize
    } else {
        end.min(len - 1) as usize
    };

    if start_idx > end_idx || start_idx >= value.len() {
        Vec::new()
    } else {
        value[start_idx..=end_idx].to_vec()
    }
}

pub(crate) fn parse_setrange_offset(b: &[u8]) -> Result<usize> {
    let s =
        std::str::from_utf8(b).map_err(|_| Error::Command("ERR offset is out of range".into()))?;
    if s.starts_with('-') {
        return Err(Error::Command("ERR offset is out of range".into()));
    }
    s.parse::<usize>()
        .map_err(|_| Error::Command("ERR offset is out of range".into()))
}

pub(crate) fn parse_bit_offset(b: &[u8]) -> Result<u64> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Error::Command("ERR bit offset is not an integer or out of range".into()))?;
    if s.starts_with('-') {
        return Err(Error::Command(
            "ERR bit offset is not an integer or out of range".into(),
        ));
    }
    s.parse::<u64>()
        .map_err(|_| Error::Command("ERR bit offset is not an integer or out of range".into()))
}

pub(crate) fn parse_bit_value(b: &[u8]) -> Result<u8> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Error::Command("ERR bit is not an integer or out of range".into()))?;
    match s {
        "0" => Ok(0),
        "1" => Ok(1),
        _ => Err(Error::Command(
            "ERR bit is not an integer or out of range".into(),
        )),
    }
}

pub(crate) fn parse_i64_arg(b: &[u8], _cmd: &str) -> Result<i64> {
    parse_i64_bytes(b)
        .map_err(|_| Error::Command("ERR value is not an integer or out of range".into()))
}

pub(crate) fn parse_u64_arg(b: &[u8], _cmd: &str) -> Result<u64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::Command("ERR syntax error".into()))?;
    s.parse::<u64>()
        .map_err(|_| Error::Command("ERR syntax error".into()))
}

pub(crate) fn parse_f64_arg(b: &[u8], _cmd: &str) -> Result<f64> {
    parse_f64_bytes(b)
}

pub(crate) fn parse_i64_bytes(b: &[u8]) -> Result<i64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::Command(WRONGTYPE.into()))?;
    s.parse::<i64>()
        .map_err(|_| Error::Command(WRONGTYPE.into()))
}

pub(crate) fn parse_f64_bytes(b: &[u8]) -> Result<f64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::Command(WRONGTYPE.into()))?;
    s.parse::<f64>()
        .map_err(|_| Error::Command(WRONGTYPE.into()))
}

pub(crate) fn format_float(v: f64) -> String {
    let s = v.to_string();
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}
