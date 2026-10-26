//! BITFIELD / BITFIELD_RO / BITCOUNT.
//!
//! 位序与 `string.rs` 的 GETBIT/SETBIT 相同: offset 0 是首字节的最高位.
//! 溢出语义按 Redis 8.8 `bitops.c` 的 WRAP / SAT / FAIL 移植.
//! 类型首字母与 Kvrocks 一样按 ASCII 转成小写, `U32` 与 `u32` 相同. 宽度仍是有符号 1..=64, 无符号 1..=63.
//! 有写子命令时, 即使每条都因 OVERFLOW FAIL 没有改位, 也会把字符串补零到最高写位,
//! 与 Redis 先扩容再执行子命令一致.

use bytes::Bytes;

use crate::error::{Error, Result};
use crate::protocol::RespValue;

const PROTO_MAX_BULK_LEN: u64 = 512 * 1024 * 1024;
const ERR_TYPE: &str = "ERR Invalid bitfield type. Use something like i16 u8. Note that u64 is not supported but i64 is.";
const ERR_OFFSET: &str = "ERR bit offset is not an integer or out of range";
const ERR_OVERFLOW: &str = "ERR Invalid OVERFLOW type specified";
const ERR_VALUE: &str = "ERR value is not an integer or out of range";
const ERR_SYNTAX: &str = "ERR syntax error";
const ERR_RO: &str = "ERR BITFIELD_RO only supports the GET subcommand";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Overflow {
    Wrap,
    Sat,
    Fail,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpKind {
    Get,
    Set,
    Incr,
}

struct Op {
    offset: u64,
    bits: u32,
    signed: bool,
    kind: OpKind,
    value: i64,
    overflow: Overflow,
}

pub(crate) struct BitfieldPlan {
    ops: Vec<Op>,
    pub writes: bool,
    highest_write_bit: u64,
}

pub(crate) struct BitfieldResult {
    pub replies: Vec<RespValue>,
    pub stored: Option<Vec<u8>>,
}

pub(crate) fn parse_bitfield(op_args: &[Bytes], readonly: bool) -> Result<BitfieldPlan> {
    let mut ops = Vec::new();
    let mut overflow = Overflow::Wrap;
    let mut writes = false;
    let mut highest_write_bit = 0u64;
    let mut i = 0;
    while i < op_args.len() {
        let sub = ascii_upper(&op_args[i]);
        let rem = op_args.len() - i - 1;
        if sub == "OVERFLOW" && rem >= 1 {
            overflow = match ascii_upper(&op_args[i + 1]).as_str() {
                "WRAP" => Overflow::Wrap,
                "SAT" => Overflow::Sat,
                "FAIL" => Overflow::Fail,
                _ => return Err(Error::Command(ERR_OVERFLOW.into())),
            };
            i += 2;
            continue;
        }
        let kind = if sub == "GET" && rem >= 2 {
            OpKind::Get
        } else if sub == "SET" && rem >= 3 {
            OpKind::Set
        } else if sub == "INCRBY" && rem >= 3 {
            OpKind::Incr
        } else {
            return Err(Error::Command(ERR_SYNTAX.into()));
        };
        let (signed, bits) = parse_type(&op_args[i + 1])?;
        let offset = parse_offset(&op_args[i + 2], bits)?;
        let value = if kind == OpKind::Get {
            0
        } else {
            parse_i64(&op_args[i + 3], ERR_VALUE)?
        };
        if kind != OpKind::Get {
            writes = true;
            let end = offset
                .checked_add(u64::from(bits - 1))
                .ok_or_else(|| Error::Command(ERR_OFFSET.into()))?;
            if (end >> 3) >= PROTO_MAX_BULK_LEN {
                return Err(Error::Command(ERR_OFFSET.into()));
            }
            highest_write_bit = highest_write_bit.max(end);
        }
        ops.push(Op {
            offset,
            bits,
            signed,
            kind,
            value,
            overflow,
        });
        i += if kind == OpKind::Get { 3 } else { 4 };
    }
    if readonly && writes {
        return Err(Error::Command(ERR_RO.into()));
    }
    Ok(BitfieldPlan {
        ops,
        writes,
        highest_write_bit,
    })
}

pub(crate) fn execute_bitfield(existing: Option<&[u8]>, plan: &BitfieldPlan) -> BitfieldResult {
    let mut data = existing.unwrap_or_default().to_vec();
    if plan.writes {
        let byte = (plan.highest_write_bit >> 3) as usize;
        if data.len() <= byte {
            data.resize(byte + 1, 0);
        }
    }
    let mut replies = Vec::with_capacity(plan.ops.len());
    for op in &plan.ops {
        replies.push(match op.kind {
            OpKind::Get => {
                let raw = get_unsigned(&data, op.offset, op.bits);
                let n = if op.signed {
                    sign_extend(raw, op.bits)
                } else {
                    raw as i64
                };
                RespValue::Integer(n)
            }
            OpKind::Set | OpKind::Incr => apply_write(&mut data, op),
        });
    }
    let stored = plan.writes.then_some(data);
    BitfieldResult { replies, stored }
}

pub(crate) fn bitcount(data: Option<&[u8]>, args: &[Bytes]) -> Result<i64> {
    let rest = &args[1..];
    let bytes = data.unwrap_or(&[]);
    let strlen = i64::try_from(bytes.len()).map_err(|_| Error::Command(ERR_OFFSET.into()))?;
    let (start, end, first_mask, last_mask) = if rest.is_empty() {
        if data.is_none() {
            return Ok(0);
        }
        (0i64, strlen - 1, 0u8, 0u8)
    } else if rest.len() == 2 || rest.len() == 3 {
        let mut start = parse_i64(&rest[0], ERR_VALUE)?;
        let mut end = parse_i64(&rest[1], ERR_VALUE)?;
        let isbit = if rest.len() == 3 {
            match ascii_upper(&rest[2]).as_str() {
                "BIT" => true,
                "BYTE" => false,
                _ => return Err(Error::Command(ERR_SYNTAX.into())),
            }
        } else {
            false
        };
        if data.is_none() {
            return Ok(0);
        }
        if start < 0 && end < 0 && start > end {
            return Ok(0);
        }
        let totlen = if isbit {
            strlen
                .checked_shl(3)
                .ok_or_else(|| Error::Command(ERR_OFFSET.into()))?
        } else {
            strlen
        };
        if start < 0 {
            start += totlen;
        }
        if end < 0 {
            end += totlen;
        }
        if start < 0 {
            start = 0;
        }
        if end < 0 {
            end = 0;
        }
        if end >= totlen {
            end = totlen - 1;
        }
        let mut first = 0u8;
        let mut last = 0u8;
        if isbit && start <= end {
            first = first_byte_mask(start);
            last = last_byte_mask(end);
            start >>= 3;
            end >>= 3;
        }
        (start, end, first, last)
    } else {
        return Err(Error::Command(ERR_SYNTAX.into()));
    };
    if start > end {
        return Ok(0);
    }
    let start_u = usize::try_from(start).map_err(|_| Error::Command(ERR_OFFSET.into()))?;
    let end_u = usize::try_from(end).map_err(|_| Error::Command(ERR_OFFSET.into()))?;
    let mut count: i64 = bytes[start_u..=end_u]
        .iter()
        .map(|b| i64::from(b.count_ones()))
        .sum();
    if first_mask != 0 || last_mask != 0 {
        let mut extra = [0u8, 0u8];
        if first_mask != 0 {
            extra[0] = bytes[start_u] & first_mask;
        }
        if last_mask != 0 {
            extra[1] = bytes[end_u] & last_mask;
        }
        let excluded: i64 = extra.iter().map(|b| i64::from(b.count_ones())).sum();
        count -= excluded;
    }
    Ok(count)
}

fn apply_write(data: &mut [u8], op: &Op) -> RespValue {
    if op.signed {
        let old = sign_extend(get_unsigned(data, op.offset, op.bits), op.bits);
        let (newval, reply) = if op.kind == OpKind::Incr {
            let (overflow, wrapped) = check_signed(old, op.value, op.bits, op.overflow);
            if overflow != 0 && op.overflow == Overflow::Fail {
                return RespValue::BulkString(None);
            }
            let newval = if overflow != 0 {
                wrapped
            } else {
                old.wrapping_add(op.value)
            };
            (newval, newval)
        } else {
            let (overflow, wrapped) = check_signed(op.value, 0, op.bits, op.overflow);
            if overflow != 0 && op.overflow == Overflow::Fail {
                return RespValue::BulkString(None);
            }
            let newval = if overflow != 0 { wrapped } else { op.value };
            (newval, old)
        };
        set_unsigned(data, op.offset, op.bits, newval as u64);
        RespValue::Integer(reply)
    } else {
        let old = get_unsigned(data, op.offset, op.bits);
        let (stored, reply) = if op.kind == OpKind::Incr {
            let (overflow, wrapped) = check_unsigned(old, op.value, op.bits, op.overflow);
            if overflow != 0 && op.overflow == Overflow::Fail {
                return RespValue::BulkString(None);
            }
            let newval = if overflow != 0 {
                wrapped
            } else {
                old.wrapping_add(op.value as u64)
            };
            (newval, newval as i64)
        } else {
            let raw = op.value as u64;
            let (overflow, wrapped) = check_unsigned(raw, 0, op.bits, op.overflow);
            if overflow != 0 && op.overflow == Overflow::Fail {
                return RespValue::BulkString(None);
            }
            let stored = if overflow != 0 { wrapped } else { raw };
            (stored, old as i64)
        };
        set_unsigned(data, op.offset, op.bits, stored);
        RespValue::Integer(reply)
    }
}

fn check_unsigned(value: u64, incr: i64, bits: u32, ow: Overflow) -> (i32, u64) {
    let max = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let maxincr = max.wrapping_sub(value) as i64;
    let minincr = value.wrapping_neg() as i64;
    if value > max || (incr > 0 && incr > maxincr) {
        let limit = match ow {
            Overflow::Wrap => wrap_unsigned(value, incr, bits),
            Overflow::Sat => max,
            Overflow::Fail => 0,
        };
        (1, limit)
    } else if incr < 0 && incr < minincr {
        let limit = match ow {
            Overflow::Wrap => wrap_unsigned(value, incr, bits),
            Overflow::Sat => 0,
            Overflow::Fail => 0,
        };
        (-1, limit)
    } else {
        (0, 0)
    }
}

fn wrap_unsigned(value: u64, incr: i64, bits: u32) -> u64 {
    let mask = u64::MAX << bits;
    value.wrapping_add(incr as u64) & !mask
}

fn check_signed(value: i64, incr: i64, bits: u32, ow: Overflow) -> (i32, i64) {
    let max = if bits == 64 {
        i64::MAX
    } else {
        (1i64 << (bits - 1)) - 1
    };
    let min = max.wrapping_neg().wrapping_sub(1);
    let maxincr = (max as u64).wrapping_sub(value as u64) as i64;
    let minincr = (min as u64).wrapping_sub(value as u64) as i64;
    let high =
        value > max || (bits != 64 && incr > maxincr) || (value >= 0 && incr > 0 && incr > maxincr);
    let low =
        value < min || (bits != 64 && incr < minincr) || (value < 0 && incr < 0 && incr < minincr);
    if high {
        let limit = match ow {
            Overflow::Wrap => wrap_signed(value, incr, bits),
            Overflow::Sat => max,
            Overflow::Fail => 0,
        };
        (1, limit)
    } else if low {
        let limit = match ow {
            Overflow::Wrap => wrap_signed(value, incr, bits),
            Overflow::Sat => min,
            Overflow::Fail => 0,
        };
        (-1, limit)
    } else {
        (0, 0)
    }
}

fn wrap_signed(value: i64, incr: i64, bits: u32) -> i64 {
    let mut c = (value as u64).wrapping_add(incr as u64);
    if bits < 64 {
        let msb = 1u64 << (bits - 1);
        let mask = u64::MAX << bits;
        if c & msb != 0 {
            c |= mask;
        } else {
            c &= !mask;
        }
    }
    c as i64
}

fn get_unsigned(data: &[u8], offset: u64, bits: u32) -> u64 {
    let mut v = 0u64;
    for i in 0..bits {
        v = (v << 1) | u64::from(read_bit(data, offset + u64::from(i)));
    }
    v
}

fn set_unsigned(data: &mut [u8], offset: u64, bits: u32, value: u64) {
    for i in 0..bits {
        let bit = ((value >> (bits - 1 - i)) & 1) as u8;
        write_bit(data, offset + u64::from(i), bit);
    }
}

fn sign_extend(raw: u64, bits: u32) -> i64 {
    if bits == 64 {
        raw as i64
    } else {
        let shift = 64 - bits;
        ((raw << shift) as i64) >> shift
    }
}

fn read_bit(data: &[u8], offset: u64) -> u8 {
    let byte_idx = (offset / 8) as usize;
    let bit_pos = 7 - (offset % 8);
    let byte = data.get(byte_idx).copied().unwrap_or(0);
    (byte >> bit_pos) & 1
}

fn write_bit(data: &mut [u8], offset: u64, bit: u8) {
    let byte_idx = (offset / 8) as usize;
    let bit_pos = 7 - (offset % 8);
    let mask = 1u8 << bit_pos;
    if bit == 1 {
        data[byte_idx] |= mask;
    } else {
        data[byte_idx] &= !mask;
    }
}

fn first_byte_mask(start: i64) -> u8 {
    let shift = 8 - (start & 7);
    ((!((1u32 << shift) - 1)) & 0xFF) as u8
}

fn last_byte_mask(end: i64) -> u8 {
    let shift = 7 - (end & 7);
    ((1u32 << shift) - 1) as u8
}

fn parse_type(raw: &[u8]) -> Result<(bool, u32)> {
    let Some((first, rest)) = raw.split_first() else {
        return Err(type_error(raw));
    };
    let (signed, rest) = match first.to_ascii_lowercase() {
        b'i' => (true, rest),
        b'u' => (false, rest),
        _ => return Err(type_error(raw)),
    };
    let rest = std::str::from_utf8(rest).map_err(|_| type_error(raw))?;
    let bits: i64 = rest.parse().map_err(|_| type_error(raw))?;
    if bits < 1 || (signed && bits > 64) || (!signed && bits > 63) {
        return Err(type_error(raw));
    }
    Ok((signed, bits as u32))
}

/// 客户端仍能看到 Redis 的那句说明. 末尾带上收到的原文, 失败日志才能区分 `U32` / `u64` / `u8.0`.
fn type_error(raw: &[u8]) -> Error {
    let shown: String = String::from_utf8_lossy(raw).chars().take(64).collect();
    Error::Command(format!("{ERR_TYPE} Got '{shown}'."))
}

fn parse_offset(raw: &[u8], bits: u32) -> Result<u64> {
    let s = std::str::from_utf8(raw).map_err(|_| Error::Command(ERR_OFFSET.into()))?;
    let (rest, hashed) = if let Some(rest) = s.strip_prefix('#') {
        (rest, true)
    } else {
        (s, false)
    };
    if rest.is_empty() {
        return Err(Error::Command(ERR_OFFSET.into()));
    }
    let mut n: i64 = rest
        .parse()
        .map_err(|_| Error::Command(ERR_OFFSET.into()))?;
    if hashed {
        n = n
            .checked_mul(i64::from(bits))
            .ok_or_else(|| Error::Command(ERR_OFFSET.into()))?;
    }
    if n < 0 {
        return Err(Error::Command(ERR_OFFSET.into()));
    }
    let offset = u64::try_from(n).map_err(|_| Error::Command(ERR_OFFSET.into()))?;
    if (offset >> 3) >= PROTO_MAX_BULK_LEN {
        return Err(Error::Command(ERR_OFFSET.into()));
    }
    Ok(offset)
}

fn parse_i64(raw: &[u8], err: &str) -> Result<i64> {
    let s = std::str::from_utf8(raw).map_err(|_| Error::Command(err.into()))?;
    s.parse::<i64>().map_err(|_| Error::Command(err.into()))
}

fn ascii_upper(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Bytes {
        Bytes::copy_from_slice(s.as_bytes())
    }

    fn run(existing: Option<&[u8]>, args: &[&str], readonly: bool) -> Result<BitfieldResult> {
        let owned: Vec<Bytes> = args.iter().copied().map(b).collect();
        let plan = parse_bitfield(&owned, readonly)?;
        Ok(execute_bitfield(existing, &plan))
    }

    fn ints(result: &BitfieldResult) -> Vec<Option<i64>> {
        result
            .replies
            .iter()
            .map(|v| match v {
                RespValue::Integer(n) => Some(*n),
                RespValue::BulkString(None) => None,
                other => panic!("unexpected reply {other:?}"),
            })
            .collect()
    }

    /// 业务 BITFIELD_RO 使用 GET uN; SET/INCRBY 的溢出必须和 Redis 8.8 一致.
    #[test]
    fn bitfield_get_set_incr_and_overflow() {
        let set = run(None, &["SET", "u8", "0", "255"], false).unwrap();
        assert_eq!(ints(&set), vec![Some(0)]);
        assert_eq!(set.stored.as_deref(), Some(&[255u8][..]));

        let get = run(set.stored.as_deref(), &["GET", "u8", "0"], true).unwrap();
        assert_eq!(ints(&get), vec![Some(255)]);
        assert!(get.stored.is_none());

        let wrap = run(set.stored.as_deref(), &["INCRBY", "u8", "0", "1"], false).unwrap();
        assert_eq!(ints(&wrap), vec![Some(0)]);

        let sat = run(
            Some(&[255]),
            &["OVERFLOW", "SAT", "INCRBY", "u8", "0", "1"],
            false,
        )
        .unwrap();
        assert_eq!(ints(&sat), vec![Some(255)]);
        assert_eq!(sat.stored.as_deref(), Some(&[255u8][..]));

        let fail = run(
            Some(&[255]),
            &["OVERFLOW", "FAIL", "INCRBY", "u8", "0", "1"],
            false,
        )
        .unwrap();
        assert_eq!(ints(&fail), vec![None]);
        assert_eq!(fail.stored.as_deref(), Some(&[255u8][..]));

        let signed = run(None, &["SET", "i8", "0", "-1"], false).unwrap();
        assert_eq!(signed.stored.as_deref(), Some(&[255u8][..]));
        let signed_get = run(signed.stored.as_deref(), &["GET", "i8", "0"], false).unwrap();
        assert_eq!(ints(&signed_get), vec![Some(-1)]);

        let signed_wrap = run(Some(&[127]), &["INCRBY", "i8", "0", "1"], false).unwrap();
        assert_eq!(ints(&signed_wrap), vec![Some(-128)]);
    }

    #[test]
    fn bitfield_hash_offset_and_readonly() {
        let spaced = run(None, &["SET", "u8", "#1", "1"], false).unwrap();
        assert_eq!(spaced.stored.as_deref(), Some(&[0u8, 1][..]));

        let Err(err) = parse_bitfield(&[b("SET"), b("u8"), b("0"), b("1")], true) else {
            panic!("BITFIELD_RO must reject SET");
        };
        assert!(err.to_string().contains("BITFIELD_RO"));

        let Err(err) = parse_bitfield(&[b("GET"), b("u64"), b("0")], false) else {
            panic!("u64 must be rejected");
        };
        let msg = err.to_string();
        assert!(msg.contains("u64 is not supported"));
        assert!(msg.contains("Got 'u64'"));
    }

    #[test]
    fn bitfield_type_prefix_ignores_ascii_case() {
        let unsigned = run(None, &["GET", "U32", "0"], false).unwrap();
        assert_eq!(ints(&unsigned), vec![Some(0)]);
        let signed = run(None, &["GET", "I8", "0"], false).unwrap();
        assert_eq!(ints(&signed), vec![Some(0)]);

        let Err(err) = parse_bitfield(&[b("GET"), b("U64"), b("0")], false) else {
            panic!("U64 must be rejected");
        };
        assert!(err.to_string().contains("Got 'U64'"));

        let Err(err) = parse_bitfield(&[b("GET"), b("u8.0"), b("0")], false) else {
            panic!("u8.0 must be rejected");
        };
        assert!(err.to_string().contains("Got 'u8.0'"));
    }

    #[test]
    fn bitcount_bytes_and_negative_end() {
        let data = [0xFFu8, 0x0F];
        assert_eq!(bitcount(Some(&data), &[b("k")]).unwrap(), 12);
        assert_eq!(
            bitcount(Some(&data), &[b("k"), b("0"), b("-1")]).unwrap(),
            12
        );
        assert_eq!(bitcount(Some(&data), &[b("k"), b("1"), b("1")]).unwrap(), 4);
        assert_eq!(
            bitcount(Some(&[0x80]), &[b("k"), b("0"), b("0"), b("BIT")]).unwrap(),
            1
        );
        assert_eq!(bitcount(None, &[b("k"), b("0"), b("-1")]).unwrap(), 0);
    }
}
