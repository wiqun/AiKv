//! Sorted Set 解析、范围边界与聚合辅助.

use std::collections::BTreeMap;

use bytes::Bytes;

use crate::command::router;
use crate::error::{Error, Result};
use crate::protocol::RespValue;

#[derive(Debug, Clone)]
pub(crate) struct ScoreBound {
    pub(crate) value: f64,
    pub(crate) inclusive: bool,
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
    if len == 0 {
        return (1, 0);
    }
    let len_i = len as i64;
    let start_idx = if start < 0 {
        (len_i + start).max(0) as usize
    } else {
        (start as usize).min(len.saturating_sub(1))
    };
    let stop_idx = if stop < 0 {
        let s = len_i + stop;
        if s < 0 {
            return (1, 0);
        }
        s as usize
    } else {
        (stop as usize).min(len.saturating_sub(1))
    };
    (start_idx, stop_idx)
}

pub(crate) fn members_in_rank(
    zset: &BTreeMap<Vec<u8>, f64>,
    start: i64,
    stop: i64,
) -> Vec<Vec<u8>> {
    let sorted = sorted_by_score(zset, false);
    if sorted.is_empty() {
        return Vec::new();
    }
    let (start_idx, stop_idx) = normalize_range(sorted.len(), start, stop);
    if start_idx > stop_idx {
        return Vec::new();
    }
    sorted[start_idx..=stop_idx]
        .iter()
        .map(|(member, _)| member.clone())
        .collect()
}

pub(crate) fn members_to_resp(members: &[(Vec<u8>, f64)], withscores: bool) -> RespValue {
    let cap = members.len() * if withscores { 2 } else { 1 };
    let mut items = Vec::with_capacity(cap);
    for (member, score) in members {
        items.push(router::bulk(member.clone()));
        if withscores {
            items.push(router::bulk(format_score(*score).into_bytes()));
        }
    }
    RespValue::Array(Some(items))
}

/// `members_to_resp` 的消费版: 成员 `Vec<u8>` 直接移进 `Bytes`, 省掉每个成员一次分配与拷贝.
pub(crate) fn members_into_resp(members: Vec<(Vec<u8>, f64)>, withscores: bool) -> RespValue {
    let cap = members.len() * if withscores { 2 } else { 1 };
    let mut items = Vec::with_capacity(cap);
    for (member, score) in members {
        items.push(router::bulk(member));
        if withscores {
            items.push(router::bulk(format_score(score).into_bytes()));
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ZAddFlags {
    pub nx: bool,
    pub xx: bool,
    pub gt: bool,
    pub lt: bool,
    pub ch: bool,
    pub incr: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ZAddOutcome {
    Count(i64),
    Score(Option<f64>),
}

pub fn parse_zadd_flags(args: &[Bytes]) -> Result<(ZAddFlags, usize)> {
    let mut flags = ZAddFlags::default();
    let mut idx = 1;
    while idx < args.len() {
        let arg = &args[idx];
        if arg.eq_ignore_ascii_case(b"NX") {
            flags.nx = true;
        } else if arg.eq_ignore_ascii_case(b"XX") {
            flags.xx = true;
        } else if arg.eq_ignore_ascii_case(b"GT") {
            flags.gt = true;
        } else if arg.eq_ignore_ascii_case(b"LT") {
            flags.lt = true;
        } else if arg.eq_ignore_ascii_case(b"CH") {
            flags.ch = true;
        } else if arg.eq_ignore_ascii_case(b"INCR") {
            flags.incr = true;
        } else {
            break;
        }
        idx += 1;
    }

    if flags.nx && flags.xx {
        return Err(Error::Command(
            "ERR XX and NX options at the same time are not compatible".into(),
        ));
    }
    if flags.nx && (flags.gt || flags.lt) {
        return Err(Error::Command(
            "ERR GT, LT, and/or NX options at the same time are not compatible".into(),
        ));
    }
    if flags.gt && flags.lt {
        return Err(Error::Command(
            "ERR GT and LT options at the same time are not compatible".into(),
        ));
    }

    let pairs_len = args.len() - idx;
    if pairs_len == 0 || !pairs_len.is_multiple_of(2) {
        return Err(router::wrong_args("ZADD", ""));
    }
    if flags.incr && pairs_len != 2 {
        return Err(Error::Command(
            "ERR INCR option supports a single increment-element pair".into(),
        ));
    }

    Ok((flags, idx))
}

pub fn apply_zadd_flags(
    map: &mut BTreeMap<Vec<u8>, f64>,
    pairs: &[(f64, Vec<u8>)],
    flags: ZAddFlags,
) -> Result<ZAddOutcome> {
    if flags.incr {
        let (incr_score, member) = &pairs[0];
        let old_score = map.get(member).copied();
        if old_score.is_none() && flags.xx {
            return Ok(ZAddOutcome::Score(None));
        }
        if old_score.is_some() && flags.nx {
            return Ok(ZAddOutcome::Score(None));
        }
        let current = old_score.unwrap_or(0.0);
        let new_score = current + incr_score;
        if new_score.is_nan() {
            return Err(Error::Command(
                "ERR resulting score is not a number (NaN)".into(),
            ));
        }
        if let Some(old) = old_score {
            if flags.gt && new_score <= old {
                return Ok(ZAddOutcome::Score(None));
            }
            if flags.lt && new_score >= old {
                return Ok(ZAddOutcome::Score(None));
            }
        }
        map.insert(member.clone(), new_score);
        return Ok(ZAddOutcome::Score(Some(new_score)));
    }

    let mut added = 0i64;
    let mut changed = 0i64;

    for (score, member) in pairs {
        match map.get(member).copied() {
            None => {
                if flags.xx {
                    continue;
                }
                map.insert(member.clone(), *score);
                added += 1;
                changed += 1;
            }
            Some(old_score) => {
                if flags.nx {
                    continue;
                }
                if flags.gt && *score <= old_score {
                    continue;
                }
                if flags.lt && *score >= old_score {
                    continue;
                }
                if *score != old_score {
                    changed += 1;
                }
                map.insert(member.clone(), *score);
            }
        }
    }

    if flags.ch {
        Ok(ZAddOutcome::Count(changed))
    } else {
        Ok(ZAddOutcome::Count(added))
    }
}

#[cfg(test)]
mod members_resp_tests {
    use super::*;

    #[test]
    fn members_into_resp_matches_members_to_resp() {
        let rows = vec![
            (b"a".to_vec(), 1.0),
            (b"bb".to_vec(), -2.5),
            (Vec::new(), 0.0),
        ];
        for withscores in [false, true] {
            assert_eq!(
                members_into_resp(rows.clone(), withscores),
                members_to_resp(&rows, withscores)
            );
        }
        assert_eq!(
            members_into_resp(Vec::new(), true),
            RespValue::Array(Some(Vec::new()))
        );
    }

    #[test]
    fn test_normalize_range_negative_out_of_bounds() {
        // len = 1, start = 0, stop = -3: len + stop = -2 < 0. Must yield start > stop (empty range).
        let (start, stop) = normalize_range(1, 0, -3);
        assert!(start > stop, "expected start > stop, got ({start}, {stop})");

        // len = 1, start = 0, stop = -1: len + stop = 0. Range [0, 0].
        let (start, stop) = normalize_range(1, 0, -1);
        assert_eq!((start, stop), (0, 0));
    }

    #[test]
    fn test_zadd_flags_parsing_and_application() {
        let mut map = BTreeMap::new();
        map.insert(b"m1".to_vec(), 10.0);

        // 1. NX: 不更新已有元素
        let flags = ZAddFlags {
            nx: true,
            ..Default::default()
        };
        let res = apply_zadd_flags(&mut map, &[(20.0, b"m1".to_vec())], flags).unwrap();
        assert_eq!(res, ZAddOutcome::Count(0));
        assert_eq!(map.get(b"m1".as_slice()), Some(&10.0));

        // 2. XX: 仅更新已有元素, 不添加新元素
        let flags = ZAddFlags {
            xx: true,
            ..Default::default()
        };
        let res = apply_zadd_flags(&mut map, &[(30.0, b"m2".to_vec())], flags).unwrap();
        assert_eq!(res, ZAddOutcome::Count(0));
        assert!(!map.contains_key(b"m2".as_slice()));

        // 3. GT: 仅在 score 大于旧分时更新
        let flags = ZAddFlags {
            gt: true,
            ..Default::default()
        };
        let res = apply_zadd_flags(&mut map, &[(5.0, b"m1".to_vec())], flags).unwrap();
        assert_eq!(res, ZAddOutcome::Count(0));
        assert_eq!(map.get(b"m1".as_slice()), Some(&10.0));
        let res = apply_zadd_flags(&mut map, &[(15.0, b"m1".to_vec())], flags).unwrap();
        assert_eq!(res, ZAddOutcome::Count(0));
        assert_eq!(map.get(b"m1".as_slice()), Some(&15.0));

        // 4. CH: 统计修改数
        let flags = ZAddFlags {
            ch: true,
            ..Default::default()
        };
        let res = apply_zadd_flags(
            &mut map,
            &[(20.0, b"m1".to_vec()), (30.0, b"m2".to_vec())],
            flags,
        )
        .unwrap();
        assert_eq!(res, ZAddOutcome::Count(2)); // m1 变更 + m2 新增

        // 5. INCR
        let flags = ZAddFlags {
            incr: true,
            ..Default::default()
        };
        let res = apply_zadd_flags(&mut map, &[(5.5, b"m1".to_vec())], flags).unwrap();
        assert_eq!(res, ZAddOutcome::Score(Some(25.5)));
    }
}
