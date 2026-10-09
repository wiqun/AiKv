//! Sorted Set 解析、范围边界与聚合辅助.

use std::collections::BTreeMap;

use bytes::Bytes;

use crate::command::router;
use crate::error::{Error, Result};
use crate::protocol::RespValue;

#[derive(Debug, Clone)]
pub(crate) struct ScoreBound {
    value: f64,
    inclusive: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum LexBound {
    NegInf,
    PosInf,
    Value(Vec<u8>, bool),
}

/// `LIMIT offset count`; `count < 0` means unlimited (StackExchange.Redis convention).
pub(crate) fn parse_limit_args(
    cmd: &str,
    offset_b: &Bytes,
    count_b: &Bytes,
) -> Result<(usize, Option<usize>)> {
    let offset = parse_i64(offset_b)?;
    if offset < 0 {
        return Err(router::wrong_args(cmd, ""));
    }
    let count_raw = parse_i64(count_b)?;
    let count = if count_raw < 0 {
        None
    } else {
        Some(count_raw as usize)
    };
    Ok((offset as usize, count))
}

pub(crate) fn apply_limit<T: Clone>(items: &mut Vec<T>, offset: usize, count: Option<usize>) {
    if offset >= items.len() {
        items.clear();
        return;
    }
    match count {
        None => *items = items[offset..].to_vec(),
        Some(c) => {
            let end = (offset + c).min(items.len());
            *items = items[offset..end].to_vec();
        }
    }
}

pub(crate) fn sorted_by_score(zset: &BTreeMap<Vec<u8>, f64>, reverse: bool) -> Vec<(Vec<u8>, f64)> {
    let mut v: Vec<(Vec<u8>, f64)> = zset.iter().map(|(k, s)| (k.clone(), *s)).collect();
    if reverse {
        v.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.0.cmp(&a.0))
        });
    } else {
        v.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
    }
    v
}

pub(crate) fn normalize_range(len: usize, start: i64, stop: i64) -> (usize, usize) {
    let len_i = len as i64;
    let start_idx = if start < 0 {
        (len_i + start).max(0) as usize
    } else {
        (start as usize).min(len.saturating_sub(1))
    };
    let stop_idx = if stop < 0 {
        (len_i + stop).max(0) as usize
    } else {
        (stop as usize).min(len.saturating_sub(1))
    };
    (start_idx, stop_idx)
}

pub(crate) fn members_to_resp(members: &[(Vec<u8>, f64)], withscores: bool) -> RespValue {
    let mut items = Vec::new();
    for (member, score) in members {
        items.push(router::bulk(member.clone()));
        if withscores {
            items.push(router::bulk(format_score(*score).into_bytes()));
        }
    }
    RespValue::Array(Some(items))
}

pub(crate) fn scan_response(cursor: u64, page: &[(Vec<u8>, f64)]) -> RespValue {
    let mut items = vec![RespValue::BulkString(Some(Bytes::from(cursor.to_string())))];
    let mut members = Vec::new();
    for (member, score) in page {
        members.push(router::bulk(member.clone()));
        members.push(router::bulk(format_score(*score).into_bytes()));
    }
    items.push(RespValue::Array(Some(members)));
    RespValue::Array(Some(items))
}

pub(crate) fn array_of_bulk(items: Vec<Vec<u8>>) -> RespValue {
    RespValue::Array(Some(items.into_iter().map(router::bulk).collect()))
}

pub(crate) fn format_score(score: f64) -> String {
    if score.fract() == 0.0 && score.is_finite() {
        format!("{}", score as i64)
    } else {
        score.to_string()
    }
}

pub(crate) fn parse_score(b: &Bytes) -> Result<f64> {
    let s = bytes_to_str(b)?;
    let score = s
        .parse::<f64>()
        .map_err(|_| Error::Command("ERR value is not a valid float".into()))?;
    if !score.is_finite() {
        return Err(Error::Command("ERR value is not a valid float".into()));
    }
    Ok(score)
}

pub(crate) fn parse_score_bound(s: &str, _is_min: bool) -> Result<ScoreBound> {
    if s == "-inf" {
        return Ok(ScoreBound {
            value: f64::NEG_INFINITY,
            inclusive: true,
        });
    }
    if s == "+inf" {
        return Ok(ScoreBound {
            value: f64::INFINITY,
            inclusive: true,
        });
    }
    let (inclusive, num_str) = if let Some(rest) = s.strip_prefix('(') {
        (false, rest)
    } else if let Some(rest) = s.strip_prefix('[') {
        (true, rest)
    } else {
        (true, s)
    };
    let value = num_str
        .parse::<f64>()
        .map_err(|_| Error::Command("ERR value is not a valid float".into()))?;
    Ok(ScoreBound { value, inclusive })
}

pub(crate) fn score_in_range(score: f64, min: &ScoreBound, max: &ScoreBound) -> bool {
    let above = if min.inclusive {
        score >= min.value
    } else {
        score > min.value
    };
    let below = if max.inclusive {
        score <= max.value
    } else {
        score < max.value
    };
    above && below
}

pub(crate) fn parse_lex_bound(s: &str) -> Result<LexBound> {
    if s == "-" {
        return Ok(LexBound::NegInf);
    }
    if s == "+" {
        return Ok(LexBound::PosInf);
    }
    let (inclusive, rest) = if let Some(r) = s.strip_prefix('(') {
        (false, r)
    } else if let Some(r) = s.strip_prefix('[') {
        (true, r)
    } else {
        (true, s)
    };
    Ok(LexBound::Value(rest.as_bytes().to_vec(), inclusive))
}

pub(crate) fn lex_in_range(member: &[u8], min: &LexBound, max: &LexBound) -> bool {
    let above_min = match min {
        LexBound::NegInf => true,
        LexBound::PosInf => false,
        LexBound::Value(v, inclusive) => {
            if *inclusive {
                member >= v.as_slice()
            } else {
                member > v.as_slice()
            }
        }
    };
    let below_max = match max {
        LexBound::PosInf => true,
        LexBound::NegInf => false,
        LexBound::Value(v, inclusive) => {
            if *inclusive {
                member <= v.as_slice()
            } else {
                member < v.as_slice()
            }
        }
    };
    above_min && below_max
}

pub(crate) fn bytes_to_str(b: &Bytes) -> Result<&str> {
    std::str::from_utf8(b).map_err(|_| Error::Command("ERR syntax error".into()))
}

pub(crate) fn parse_i64(b: &Bytes) -> Result<i64> {
    let s =
        std::str::from_utf8(b).map_err(|_| Error::Command("ERR value is not an integer".into()))?;
    s.parse::<i64>()
        .map_err(|_| Error::Command("ERR value is not an integer".into()))
}

pub(crate) fn eq_ignore_case(a: &Bytes, b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.eq_ignore_ascii_case(y))
}

// ---- ZINTER / ZUNION 辅助类型 ----

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Aggregate {
    Sum,
    Min,
    Max,
}

pub(crate) struct ZSetCombineArgs {
    pub(crate) keys: Vec<Bytes>,
    pub(crate) weights: Vec<f64>,
    pub(crate) aggregate: Aggregate,
    pub(crate) withscores: bool,
}

pub(crate) fn parse_zset_combine_args(cmd: &str, args: &[Bytes]) -> Result<ZSetCombineArgs> {
    router::require_min_args(cmd, args, 2)?;
    let nkeys_str = bytes_to_str(&args[0])?;
    let numkeys: usize = nkeys_str
        .parse()
        .map_err(|_| Error::Command("ERR value is not an integer or out of range".into()))?;
    if numkeys < 1 || args.len() < 1 + numkeys {
        return Err(Error::Command(format!(
            "ERR at least 1 input key is needed for the '{cmd}' command"
        )));
    }
    let keys = args[1..1 + numkeys].to_vec();
    let mut weights: Vec<f64> = Vec::new();
    let mut aggregate = Aggregate::Sum;
    let mut withscores = false;
    let mut i = 1 + numkeys;
    while i < args.len() {
        if eq_ignore_case(&args[i], b"WEIGHTS") {
            if i + numkeys >= args.len() {
                return Err(router::wrong_args(cmd, ""));
            }
            for j in 0..numkeys {
                weights.push(parse_score(&args[i + 1 + j])?);
            }
            i += 1 + numkeys;
        } else if eq_ignore_case(&args[i], b"AGGREGATE") {
            if i + 1 >= args.len() {
                return Err(router::wrong_args(cmd, ""));
            }
            aggregate = parse_aggregate(&args[i + 1])?;
            i += 2;
        } else if eq_ignore_case(&args[i], b"WITHSCORES") {
            withscores = true;
            i += 1;
        } else {
            return Err(router::wrong_args(cmd, ""));
        }
    }
    Ok(ZSetCombineArgs {
        keys,
        weights,
        aggregate,
        withscores,
    })
}

fn parse_aggregate(b: &Bytes) -> Result<Aggregate> {
    let s = bytes_to_str(b)?;
    match s.to_ascii_uppercase().as_str() {
        "SUM" => Ok(Aggregate::Sum),
        "MIN" => Ok(Aggregate::Min),
        "MAX" => Ok(Aggregate::Max),
        _ => Err(Error::Command(
            "ERR AGGREGATE must be SUM, MIN, or MAX".into(),
        )),
    }
}

pub(crate) fn aggregate_score(
    zsets: &[BTreeMap<Vec<u8>, f64>],
    member: &[u8],
    weights: &[f64],
    aggregate: &Aggregate,
) -> f64 {
    let mut result = match aggregate {
        Aggregate::Sum => 0.0,
        Aggregate::Min => f64::INFINITY,
        Aggregate::Max => f64::NEG_INFINITY,
    };
    let mut has_score = false;
    for (i, zset) in zsets.iter().enumerate() {
        if let Some(score) = zset.get(member) {
            let w = weights.get(i).copied().unwrap_or(1.0);
            let weighted = score * w;
            match aggregate {
                Aggregate::Sum => result += weighted,
                Aggregate::Min => {
                    if !has_score || weighted < result {
                        result = weighted;
                    }
                }
                Aggregate::Max => {
                    if !has_score || weighted > result {
                        result = weighted;
                    }
                }
            }
            has_score = true;
        }
    }
    result
}

pub(crate) fn sort_by_score_then_member(items: &mut [(Vec<u8>, f64)]) {
    items.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
}
