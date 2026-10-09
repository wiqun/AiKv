//! Sorted Set 命令: ZADD/ZREM/ZSCORE/ZRANK/ZREVRANK/ZRANGE*/ZRANGEBYSCORE/ZRANGEBYLEX/
//! ZCOUNT/ZLEXCOUNT/ZPOPMIN/ZPOPMAX/BZPOPMIN/BZPOPMAX/ZINCRBY/ZSCAN, 以及聚合
//! ZINTER/ZUNION/ZDIFF (含 *STORE 变体).
//!
//! 子模块: `helpers` (范围边界 / LIMIT / 聚合解析), `index` (内存分数序).
//!
//! # 存储表示
//!
//! ```text
//! ValueType::ZSet(BTreeMap<Vec<u8>, f64>)
//!   member → score (f64); BTreeMap 按 member 字节序排列
//!   经 get_typed / set_typed 整体读写
//!   分数序只在内存中的 ScoreIndex，不进入磁盘
//! ```
//!
//! # 关键点
//!
//! - 有序访问: 按 score 走内存 `ScoreIndex`，范围在 LIMIT 处停止; 按 lex 利用
//!   BTreeMap 天然字典序 (ZRANGEBYLEX/ZLEXCOUNT); rank 沿分数序计数 (ZRANK/ZREVRANK).
//! - 范围边界: score 用 `ScoreBound` (`-inf`/`+inf`, `(` 开区间, 见 `score_in_range`);
//!   lex 用 `LexBound` (`-`/`+`, `(`/`[`, 见 `lex_in_range`).
//! - 聚合: ZINTER/ZUNION/ZDIFF 经 `parse_zset_combine_args` 解析 numkeys/WEIGHTS/AGGREGATE,
//!   `aggregate_score` 按 SUM/MIN/MAX 合并.
//! - 阻塞 BZPOP*: 先非阻塞 `zpopmin`/`zpopmax`, 失败后 `BlockingRegistry::register` +
//!   10ms 轮询; 写侧 (ZADD) 成功后 `notify(key)` 唤醒; 超时返回 nil Array.
//! - 类型分轨与空容器删除: `get_typed`/`set_typed`; pop/zrem 后集合为空则 `delete`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::oneshot;

use crate::command::blocking::{self, BlockedClientGuard, BlockingRegistry};
use crate::command::router::{self, KeyLock};
use crate::command::scan_util;
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::server::config::TransactionGate;
use crate::server::ServerMetrics;
use crate::storage::memory::glob_match;
use crate::storage::{KvStorage, StoredValue, ValueType};

pub(crate) mod helpers;
pub(crate) mod index;

use self::helpers::{
    aggregate_score, apply_limit, array_of_bulk, bytes_to_str, eq_ignore_case, format_score,
    lex_in_range, members_to_resp, normalize_range, parse_i64, parse_lex_bound, parse_limit_args,
    parse_score, parse_score_bound, parse_zset_combine_args, scan_response, score_in_range,
    sort_by_score_then_member,
};
use self::index::ScoreIndex;

pub struct ZSetCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
    metrics: Option<Arc<ServerMetrics>>,
    transaction_gate: TransactionGate,
}

impl ZSetCommands {
    pub fn new(
        storage: Arc<dyn KvStorage>,
        key_lock: Arc<KeyLock>,
        transaction_gate: TransactionGate,
    ) -> Self {
        Self {
            storage,
            key_lock,
            metrics: None,
            transaction_gate,
        }
    }

    pub fn with_metrics(
        storage: Arc<dyn KvStorage>,
        key_lock: Arc<KeyLock>,
        metrics: Arc<ServerMetrics>,
        transaction_gate: TransactionGate,
    ) -> Self {
        Self {
            storage,
            key_lock,
            metrics: Some(metrics),
            transaction_gate,
        }
    }

    pub async fn zadd(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZADD", args, 3)?;
        if !(args.len() - 1).is_multiple_of(2) {
            return Err(router::wrong_args("ZADD", ""));
        }
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let mut stored = self.load_or_create_zset(db, key).await?;
        let zset = stored.as_zset_mut()?;
        let mut count = 0i64;
        for chunk in args[1..].chunks(2) {
            let score = parse_score(&chunk[0])?;
            let member = chunk[1].to_vec();
            if !zset.contains_key(&member) {
                count += 1;
            }
            zset.insert(member, score);
        }
        self.storage.rewrite_stored(db, key, stored).await?;
        BlockingRegistry::global().notify(key, RespValue::SimpleString("OK".into()));
        Ok(router::integer(count))
    }

    pub async fn zrem(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZREM", args, 2)?;
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let Some(mut stored) = self.storage.get_typed(db, key).await? else {
            return Ok(router::integer(0));
        };
        let zset = stored.as_zset_mut()?;
        let mut count = 0i64;
        for member in &args[1..] {
            if zset.remove(member.as_ref()).is_some() {
                count += 1;
            }
        }
        if zset.is_empty() {
            self.storage.delete(db, key).await?;
        } else {
            self.storage.rewrite_stored(db, key, stored).await?;
        }
        Ok(router::integer(count))
    }

    pub async fn zscore(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("ZSCORE", args, 2)?;
        let zset = self.load_zset(db, &args[0]).await?;
        let Some(zset) = zset else {
            return Ok(router::nil_bulk());
        };
        match zset.get(args[1].as_ref()) {
            None => Ok(router::nil_bulk()),
            Some(score) => Ok(router::bulk(format_score(*score).into_bytes())),
        }
    }

    pub async fn zrank(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("ZRANK", args, 2)?;
        self.rank(db, &args[0], &args[1], false).await
    }

    pub async fn zrevrank(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("ZREVRANK", args, 2)?;
        self.rank(db, &args[0], &args[1], true).await
    }

    async fn rank(
        &self,
        db: usize,
        key: &Bytes,
        member: &Bytes,
        reverse: bool,
    ) -> Result<RespValue> {
        let zset = self.load_zset(db, key).await?;
        let Some(zset) = zset else {
            return Ok(router::nil_bulk());
        };
        if !zset.contains_key(member.as_ref()) {
            return Ok(router::nil_bulk());
        }
        let index = ScoreIndex::from_members(&zset);
        let pos = index
            .rank(member.as_ref(), reverse)
            .expect("member present");
        Ok(router::integer(pos as i64))
    }

    pub async fn zrange(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZRANGE", args, 3)?;
        self.range_by_index(db, &args[0], args, false).await
    }

    pub async fn zrevrange(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZREVRANGE", args, 3)?;
        self.range_by_index(db, &args[0], args, true).await
    }

    async fn range_by_index(
        &self,
        db: usize,
        key: &Bytes,
        args: &[Bytes],
        reverse: bool,
    ) -> Result<RespValue> {
        let start = parse_i64(&args[1])?;
        let stop = parse_i64(&args[2])?;
        let withscores = args.iter().any(|a| eq_ignore_case(a, b"WITHSCORES"));
        let zset = self.load_zset(db, key).await?;
        let Some(zset) = zset else {
            return Ok(RespValue::Array(Some(vec![])));
        };
        let index = ScoreIndex::from_members(&zset);
        let len = zset.len();
        let (start_idx, stop_idx) = normalize_range(len, start, stop);
        if start_idx > stop_idx {
            return Ok(RespValue::Array(Some(vec![])));
        }
        let sorted = index.collect_rank_window(start_idx, stop_idx + 1, reverse);
        Ok(members_to_resp(&sorted, withscores))
    }

    pub async fn zrangebyscore(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZRANGEBYSCORE", args, 3)?;
        self.range_by_score(db, &args[0], args, false).await
    }

    pub async fn zrevrangebyscore(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZREVRANGEBYSCORE", args, 3)?;
        self.range_by_score(db, &args[0], args, true).await
    }

    async fn range_by_score(
        &self,
        db: usize,
        key: &Bytes,
        args: &[Bytes],
        reverse: bool,
    ) -> Result<RespValue> {
        let (min, max) = if reverse {
            (
                parse_score_bound(bytes_to_str(&args[2])?, true)?,
                parse_score_bound(bytes_to_str(&args[1])?, false)?,
            )
        } else {
            (
                parse_score_bound(bytes_to_str(&args[1])?, true)?,
                parse_score_bound(bytes_to_str(&args[2])?, false)?,
            )
        };
        let mut withscores = false;
        let mut limit: Option<(usize, Option<usize>)> = None;
        let mut i = 3;
        while i < args.len() {
            if eq_ignore_case(&args[i], b"WITHSCORES") {
                withscores = true;
                i += 1;
            } else if eq_ignore_case(&args[i], b"LIMIT") {
                if i + 2 >= args.len() {
                    return Err(router::wrong_args("ZRANGEBYSCORE", ""));
                }
                limit = Some(parse_limit_args(
                    "ZRANGEBYSCORE",
                    &args[i + 1],
                    &args[i + 2],
                )?);
                i += 3;
            } else {
                return Err(router::wrong_args("ZRANGEBYSCORE", ""));
            }
        }
        let zset = self.load_zset(db, key).await?;
        let Some(zset) = zset else {
            return Ok(RespValue::Array(Some(vec![])));
        };
        let index = ScoreIndex::from_members(&zset);
        let (offset, count) = limit.unwrap_or((0, None));
        let sorted = index.collect_score_window(&min, &max, reverse, offset, count);
        Ok(members_to_resp(&sorted, withscores))
    }

    pub async fn zcard(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("ZCARD", args, 1)?;
        let zset = self.load_zset(db, &args[0]).await?;
        Ok(router::integer(zset.map(|z| z.len() as i64).unwrap_or(0)))
    }

    pub async fn zcount(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("ZCOUNT", args, 3)?;
        let min = parse_score_bound(bytes_to_str(&args[1])?, true)?;
        let max = parse_score_bound(bytes_to_str(&args[2])?, false)?;
        let zset = self.load_zset(db, &args[0]).await?;
        let count = zset
            .map(|z| {
                z.values()
                    .filter(|score| score_in_range(**score, &min, &max))
                    .count() as i64
            })
            .unwrap_or(0);
        Ok(router::integer(count))
    }

    pub async fn zincrby(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("ZINCRBY", args, 3)?;
        let increment = parse_score(&args[1])?;
        let member = args[2].to_vec();
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let mut stored = self.load_or_create_zset(db, key).await?;
        let zset = stored.as_zset_mut()?;
        let new_score = zset.get(&member).copied().unwrap_or(0.0) + increment;
        zset.insert(member, new_score);
        self.storage.rewrite_stored(db, key, stored).await?;
        Ok(router::bulk(format_score(new_score).into_bytes()))
    }

    pub async fn zscan(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZSCAN", args, 2)?;
        let cursor = scan_util::parse_u64(&args[1])?;
        let opts = scan_util::parse_scan_options("ZSCAN", args, 2)?;
        let zset = self.load_zset(db, &args[0]).await?;
        let Some(zset) = zset else {
            return Ok(scan_response(0, &[]));
        };
        let entries: Vec<(Vec<u8>, f64)> = zset
            .iter()
            .filter(|(m, _)| {
                opts.pattern
                    .as_ref()
                    .is_none_or(|p| glob_match(p, m.as_slice()))
            })
            .map(|(m, s)| (m.clone(), *s))
            .collect();
        let (next_cursor, page) = scan_util::paginate_slice(&entries, cursor, opts.count);
        Ok(scan_response(next_cursor, page))
    }

    pub async fn zpopmin(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZPOPMIN", args, 1)?;
        let count = if args.len() > 1 {
            parse_i64(&args[1])? as usize
        } else {
            1
        };
        self.pop_extreme(db, &args[0], count, false).await
    }

    pub async fn zpopmax(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZPOPMAX", args, 1)?;
        let count = if args.len() > 1 {
            parse_i64(&args[1])? as usize
        } else {
            1
        };
        self.pop_extreme(db, &args[0], count, true).await
    }

    async fn pop_extreme(
        &self,
        db: usize,
        key: &Bytes,
        count: usize,
        max: bool,
    ) -> Result<RespValue> {
        let _lock = self.key_lock.lock(key).await;
        let Some(mut stored) = self.storage.get_typed(db, key).await? else {
            return Ok(RespValue::Array(Some(vec![])));
        };
        let zset = stored.as_zset_mut()?;
        let n = count.min(zset.len());
        let picked = ScoreIndex::from_members(zset).collect_extreme(n, max);
        let mut items = Vec::with_capacity(n * 2);
        for (member, score) in picked {
            zset.remove(&member);
            items.push(router::bulk(member));
            items.push(router::bulk(format_score(score).into_bytes()));
        }
        if zset.is_empty() {
            self.storage.delete(db, key).await?;
        } else {
            self.storage.rewrite_stored(db, key, stored).await?;
        }
        Ok(RespValue::Array(Some(items)))
    }

    pub async fn zrangebylex(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZRANGEBYLEX", args, 3)?;
        self.range_by_lex(db, &args[0], args, false).await
    }

    pub async fn zrevrangebylex(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZREVRANGEBYLEX", args, 3)?;
        self.range_by_lex(db, &args[0], args, true).await
    }

    async fn range_by_lex(
        &self,
        db: usize,
        key: &Bytes,
        args: &[Bytes],
        reverse: bool,
    ) -> Result<RespValue> {
        let (min, max) = if reverse {
            (
                parse_lex_bound(bytes_to_str(&args[2])?)?,
                parse_lex_bound(bytes_to_str(&args[1])?)?,
            )
        } else {
            (
                parse_lex_bound(bytes_to_str(&args[1])?)?,
                parse_lex_bound(bytes_to_str(&args[2])?)?,
            )
        };
        let mut limit: Option<(usize, Option<usize>)> = None;
        let mut i = 3;
        while i < args.len() {
            if eq_ignore_case(&args[i], b"LIMIT") {
                if i + 2 >= args.len() {
                    return Err(router::wrong_args("ZRANGEBYLEX", ""));
                }
                limit = Some(parse_limit_args("ZRANGEBYLEX", &args[i + 1], &args[i + 2])?);
                i += 3;
            } else {
                return Err(router::wrong_args("ZRANGEBYLEX", ""));
            }
        }
        let zset = self.load_zset(db, key).await?;
        let Some(zset) = zset else {
            return Ok(RespValue::Array(Some(vec![])));
        };
        let mut members: Vec<Vec<u8>> = zset
            .keys()
            .filter(|m| lex_in_range(m.as_slice(), &min, &max))
            .cloned()
            .collect();
        if reverse {
            members.reverse();
        }
        if let Some((offset, count)) = limit {
            apply_limit(&mut members, offset, count);
        }
        Ok(array_of_bulk(members))
    }

    pub async fn zlexcount(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("ZLEXCOUNT", args, 3)?;
        let min = parse_lex_bound(bytes_to_str(&args[1])?)?;
        let max = parse_lex_bound(bytes_to_str(&args[2])?)?;
        let zset = self.load_zset(db, &args[0]).await?;
        let count = zset
            .map(|z| {
                z.keys()
                    .filter(|m| lex_in_range(m.as_slice(), &min, &max))
                    .count() as i64
            })
            .unwrap_or(0);
        Ok(router::integer(count))
    }

    async fn load_zset(&self, db: usize, key: &[u8]) -> Result<Option<BTreeMap<Vec<u8>, f64>>> {
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(None);
        };
        Ok(Some(stored.as_zset()?.clone()))
    }

    async fn load_or_create_zset(&self, db: usize, key: &[u8]) -> Result<StoredValue> {
        match self.storage.get_typed(db, key).await? {
            None => Ok(StoredValue::new_zset(BTreeMap::new())),
            Some(stored) => match stored.value {
                ValueType::ZSet(_) => Ok(stored),
                _ => Err(router::wrongtype()),
            },
        }
    }

    pub async fn zinter(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        let opts = parse_zset_combine_args("ZINTER", args)?;
        let mut zsets = Vec::with_capacity(opts.keys.len());
        for key in &opts.keys {
            let zs = self.load_zset(db, key).await?.unwrap_or_default();
            zsets.push(zs);
        }
        let smallest = zsets
            .iter()
            .enumerate()
            .min_by_key(|(_, z)| z.len())
            .map(|(i, _)| i)
            .unwrap_or(0);
        let mut results = Vec::new();
        for member in zsets[smallest].keys() {
            if zsets.iter().all(|z| z.contains_key(member)) {
                let score = aggregate_score(&zsets, member, &opts.weights, &opts.aggregate);
                results.push((member.clone(), score));
            }
        }
        sort_by_score_then_member(&mut results);
        Ok(members_to_resp(&results, opts.withscores))
    }

    pub async fn zunion(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        let opts = parse_zset_combine_args("ZUNION", args)?;
        let mut zsets = Vec::with_capacity(opts.keys.len());
        for key in &opts.keys {
            let zs = self.load_zset(db, key).await?.unwrap_or_default();
            zsets.push(zs);
        }
        let mut seen: Vec<Vec<u8>> = Vec::new();
        for zset in &zsets {
            for member in zset.keys() {
                if !seen.contains(member) {
                    seen.push(member.clone());
                }
            }
        }
        let mut results = Vec::with_capacity(seen.len());
        for member in &seen {
            let score = aggregate_score(&zsets, member, &opts.weights, &opts.aggregate);
            results.push((member.clone(), score));
        }
        sort_by_score_then_member(&mut results);
        Ok(members_to_resp(&results, opts.withscores))
    }

    pub async fn zdiff(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZDIFF", args, 2)?;
        let nkeys_str = bytes_to_str(&args[0])?;
        let numkeys: usize = nkeys_str
            .parse()
            .map_err(|_| Error::Command("ERR value is not an integer or out of range".into()))?;
        if numkeys < 1 || args.len() < 1 + numkeys {
            return Err(router::wrong_args("ZDIFF", ""));
        }
        let withscores = args.iter().any(|a| eq_ignore_case(a, b"WITHSCORES"));
        let mut zsets = Vec::with_capacity(numkeys);
        for i in 0..numkeys {
            let zs = self.load_zset(db, &args[1 + i]).await?.unwrap_or_default();
            zsets.push(zs);
        }
        let mut results = Vec::new();
        for (member, score) in &zsets[0] {
            let in_others = zsets[1..].iter().any(|z| z.contains_key(member));
            if !in_others {
                results.push((member.clone(), *score));
            }
        }
        sort_by_score_then_member(&mut results);
        Ok(members_to_resp(&results, withscores))
    }

    // -- 阻塞命令 --

    pub async fn bzpopmin(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("BZPOPMIN", args, 2)?;
        self.bzpop_common(db, args, false).await
    }

    pub async fn bzpopmax(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("BZPOPMAX", args, 2)?;
        self.bzpop_common(db, args, true).await
    }

    async fn bzpop_common(&self, db: usize, args: &[Bytes], max: bool) -> Result<RespValue> {
        let keys: Vec<&Bytes> = args[..args.len() - 1].iter().collect();
        let timeout_s =
            crate::command::list::ListCommands::parse_timeout_secs(&args[args.len() - 1])?;
        let infinite = timeout_s == 0.0;
        let dur = if infinite {
            Duration::from_secs(60 * 60 * 24 * 365)
        } else {
            Duration::from_secs_f64(timeout_s)
        };
        let registry = BlockingRegistry::global();
        let deadline = Instant::now() + dur;
        let mut receivers = match self.prepare_bzpop(db, &keys, max, dur, registry).await? {
            Ok(response) => return Ok(response),
            Err(receivers) => receivers,
        };
        let _blocked = BlockedClientGuard::enter(&self.metrics);

        while infinite || Instant::now() < deadline {
            let mut notified = false;
            for recv in &mut receivers {
                match recv.try_recv() {
                    Ok(_) | Err(oneshot::error::TryRecvError::Closed) => {
                        notified = true;
                        break;
                    }
                    Err(oneshot::error::TryRecvError::Empty) => {}
                }
            }
            if notified {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match self
                    .prepare_bzpop(db, &keys, max, remaining, registry)
                    .await?
                {
                    Ok(response) => return Ok(response),
                    Err(next) => receivers = next,
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(blocking::nil_blocking_response())
    }

    async fn prepare_bzpop(
        &self,
        db: usize,
        keys: &[&Bytes],
        max: bool,
        timeout: Duration,
        registry: &BlockingRegistry,
    ) -> Result<std::result::Result<RespValue, Vec<oneshot::Receiver<RespValue>>>> {
        let gate = Arc::clone(&self.transaction_gate);
        let _guard = gate.read_owned().await;
        if let Some(response) = self.try_bzpop_any(db, keys, max).await? {
            return Ok(Ok(response));
        }
        let receivers = keys
            .iter()
            .map(|key| registry.register(key.to_vec(), timeout))
            .collect();
        if let Some(response) = self.try_bzpop_any(db, keys, max).await? {
            return Ok(Ok(response));
        }
        Ok(Err(receivers))
    }

    async fn try_bzpop_any(
        &self,
        db: usize,
        keys: &[&Bytes],
        max: bool,
    ) -> Result<Option<RespValue>> {
        for key in keys {
            let pop_args = [(*key).clone(), Bytes::from_static(b"1")];
            let response = if max {
                self.zpopmax(db, &pop_args).await?
            } else {
                self.zpopmin(db, &pop_args).await?
            };
            if let RespValue::Array(Some(mut items)) = response {
                if !items.is_empty() {
                    items.insert(0, RespValue::BulkString(Some((*key).clone())));
                    return Ok(Some(RespValue::Array(Some(items))));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryEngine;

    async fn wait_for_waiter(key: &[u8]) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while BlockingRegistry::global().waiter_count(key) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("waiter registration timeout");
    }

    /// Issue #78: BZPOPMIN 被空通知唤醒后必须无丢通知地完成重新注册.
    #[tokio::test]
    async fn issue_78_bzpopmin_reregisters_after_empty_notification() {
        let key = Bytes::from_static(b"issue-78-bzpopmin-reregister");
        let commands = Arc::new(ZSetCommands::new(
            MemoryEngine::new(1),
            Arc::new(KeyLock::new(1)),
            Arc::new(tokio::sync::RwLock::new(())),
        ));
        let task = {
            let commands = Arc::clone(&commands);
            let key = key.clone();
            tokio::spawn(
                async move { commands.bzpopmin(0, &[key, Bytes::from_static(b"5")]).await },
            )
        };

        wait_for_waiter(&key).await;
        BlockingRegistry::global().notify(&key, RespValue::SimpleString("OK".into()));
        wait_for_waiter(&key).await;
        task.abort();
    }
}
