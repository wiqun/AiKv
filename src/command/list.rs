//! List 命令: LPUSH/LPUSHX/RPUSH/RPUSHX/LPOP/RPOP/LLEN/LRANGE/LINDEX/LSET/LREM/LINSERT/LMOVE/
//! RPOPLPUSH/LPOS/LTRIM, 以及阻塞 BLPOP/BRPOP/BLMOVE/BRPOPLPUSH.
//!
//! # 存储表示
//!
//! ```text
//! ValueType::List(VecDeque<Vec<u8>>)
//!   LPUSH → push_front      LPOP → pop_front
//!   RPUSH → push_back       RPOP → pop_back
//!   空 list → delete (不留空 key)
//! ```
//!
//! # 阻塞 pop (BLPOP/BRPOP/BLMOVE)
//!
//! ```text
//! try_pop_any 非阻塞尝试 (遍历 keys, 取首个非空 list)
//!   ├─ 命中 → 返回 [key, element]
//!   ├─ timeout=0 → 无限阻塞 (Redis 8.8)
//!   ├─ timeout<0 → ERR timeout is negative
//!   └─ 否则 BlockingRegistry::register(key, dur) + 10ms 轮询 try_recv;
//!        写侧 LPUSH/RPUSH 成功后 notify(key, OK) 唤醒
//! ```
//!
//! 类型分轨: `get_typed`/`set_typed`; LMOVE/BLMOVE 双 key 用 `lock_two` (字典序).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::oneshot;

use crate::command::blocking::{self, BlockedClientGuard, BlockingRegistry};
use crate::command::router::{self, KeyLock};
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::server::config::TransactionGate;
use crate::server::ServerMetrics;
use crate::storage::{KvStorage, StoredValue, ValueType};

pub struct ListCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
    metrics: Option<Arc<ServerMetrics>>,
    transaction_gate: TransactionGate,
}

impl ListCommands {
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

    pub async fn lpush(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("LPUSH", args, 2)?;
        self.push(db, &args[0], &args[1..], true, true).await
    }

    pub async fn rpush(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("RPUSH", args, 2)?;
        self.push(db, &args[0], &args[1..], false, true).await
    }

    pub async fn lpushx(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("LPUSHX", args, 2)?;
        self.push(db, &args[0], &args[1..], true, false).await
    }

    pub async fn rpushx(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("RPUSHX", args, 2)?;
        self.push(db, &args[0], &args[1..], false, false).await
    }

    pub async fn rpoplpush(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("RPOPLPUSH", args, 2)?;
        let moved = [
            args[0].clone(),
            args[1].clone(),
            Bytes::from_static(b"RIGHT"),
            Bytes::from_static(b"LEFT"),
        ];
        self.lmove(db, &moved).await
    }

    pub async fn brpoplpush(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("BRPOPLPUSH", args, 3)?;
        let moved = [
            args[0].clone(),
            args[1].clone(),
            Bytes::from_static(b"RIGHT"),
            Bytes::from_static(b"LEFT"),
            args[2].clone(),
        ];
        self.blmove_blocking(db, &moved).await
    }

    async fn push(
        &self,
        db: usize,
        key: &Bytes,
        elements: &[Bytes],
        front: bool,
        create: bool,
    ) -> Result<RespValue> {
        let _lock = self.key_lock.lock(key).await;
        let mut stored = if create {
            self.load_or_create_list(db, key).await?
        } else {
            match self.storage.get_typed(db, key).await? {
                None => return Ok(router::integer(0)),
                Some(stored) => match stored.value {
                    ValueType::List(_) => stored,
                    _ => return Err(router::wrongtype()),
                },
            }
        };
        let list = stored.as_list_mut()?;
        if front {
            for el in elements {
                list.push_front(el.to_vec());
            }
        } else {
            for el in elements {
                list.push_back(el.to_vec());
            }
        }
        let len = list.len() as i64;
        self.storage.rewrite_stored(db, key, stored).await?;
        BlockingRegistry::global().notify(key, RespValue::SimpleString("OK".into()));
        Ok(router::integer(len))
    }

    pub async fn lpop(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LPOP", args, 1)?;
        let count = if args.len() > 1 {
            Some(parse_pop_count(&args[1])?)
        } else {
            None
        };
        self.pop(db, &args[0], count, true).await
    }

    pub async fn rpop(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("RPOP", args, 1)?;
        let count = if args.len() > 1 {
            Some(parse_pop_count(&args[1])?)
        } else {
            None
        };
        self.pop(db, &args[0], count, false).await
    }

    async fn pop(
        &self,
        db: usize,
        key: &Bytes,
        count: Option<i64>,
        front: bool,
    ) -> Result<RespValue> {
        let _lock = self.key_lock.lock(key).await;
        let Some(mut stored) = self.storage.get_typed(db, key).await? else {
            return Ok(router::nil_bulk());
        };
        let list = stored.as_list_mut()?;
        let count = count.unwrap_or(1);
        if count == 0 {
            return Ok(RespValue::Array(Some(vec![])));
        }
        let n = (count as usize).min(list.len());
        let mut popped = Vec::with_capacity(n);
        for _ in 0..n {
            let v = if front {
                list.pop_front()
            } else {
                list.pop_back()
            };
            popped.push(v.expect("len checked"));
        }
        if list.is_empty() {
            self.storage.delete(db, key).await?;
        } else {
            self.storage.rewrite_stored(db, key, stored).await?;
        }
        if count == 1 && popped.len() == 1 {
            return Ok(router::bulk(popped.into_iter().next().unwrap()));
        }
        Ok(array_of_bulk(popped))
    }

    pub async fn llen(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LLEN", args, 1)?;
        let list = self.load_list(db, &args[0]).await?;
        Ok(router::integer(list.map(|l| l.len() as i64).unwrap_or(0)))
    }

    pub async fn lrange(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LRANGE", args, 3)?;
        let start = parse_i64(&args[1])?;
        let stop = parse_i64(&args[2])?;
        let list = self.load_list(db, &args[0]).await?;
        let Some(list) = list else {
            return Ok(RespValue::Array(Some(vec![])));
        };
        let (start_idx, stop_idx) = normalize_range(list.len(), start, stop);
        if start_idx > stop_idx {
            return Ok(RespValue::Array(Some(vec![])));
        }
        let items: Vec<Vec<u8>> = list
            .iter()
            .skip(start_idx)
            .take(stop_idx - start_idx + 1)
            .cloned()
            .collect();
        Ok(array_of_bulk(items))
    }

    pub async fn lindex(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LINDEX", args, 2)?;
        let index = parse_i64(&args[1])?;
        let list = self.load_list(db, &args[0]).await?;
        let Some(list) = list else {
            return Ok(router::nil_bulk());
        };
        let idx = normalize_index(list.len(), index);
        match list.get(idx) {
            None => Ok(router::nil_bulk()),
            Some(v) => Ok(router::bulk(v.clone())),
        }
    }

    pub async fn lset(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LSET", args, 3)?;
        let index = parse_i64(&args[1])?;
        let _lock = self.key_lock.lock(&args[0]).await;
        let Some(mut stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Err(Error::Command("ERR no such key".into()));
        };
        let list = stored.as_list_mut()?;
        let idx = normalize_index(list.len(), index);
        let Some(slot) = list.get_mut(idx) else {
            return Err(Error::Command("ERR index out of range".into()));
        };
        *slot = args[2].to_vec();
        self.storage.rewrite_stored(db, &args[0], stored).await?;
        Ok(router::ok())
    }

    pub async fn lrem(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LREM", args, 3)?;
        let count = parse_i64(&args[1])?;
        let element = args[2].as_ref();
        let _lock = self.key_lock.lock(&args[0]).await;
        let Some(mut stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(router::integer(0));
        };
        let list = stored.as_list_mut()?;
        let removed = if count == 0 {
            let before = list.len();
            list.retain(|v| v.as_slice() != element);
            before - list.len()
        } else if count > 0 {
            remove_direction(list, element, count as usize, true)
        } else {
            remove_direction(list, element, (-count) as usize, false)
        };
        if list.is_empty() {
            self.storage.delete(db, &args[0]).await?;
        } else {
            self.storage.rewrite_stored(db, &args[0], stored).await?;
        }
        Ok(router::integer(removed as i64))
    }

    pub async fn linsert(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LINSERT", args, 4)?;
        let key = &args[0];
        let where_arg = &args[1];
        let pivot = args[2].as_ref();
        let element = args[3].to_vec();
        let before = if eq_ignore_case(where_arg, b"BEFORE") {
            true
        } else if eq_ignore_case(where_arg, b"AFTER") {
            false
        } else {
            return Err(Error::Command("ERR syntax error".into()));
        };

        let _lock = self.key_lock.lock(key).await;
        let Some(mut stored) = self.storage.get_typed(db, key).await? else {
            return Ok(router::integer(0));
        };
        let list = stored.as_list_mut()?;
        let Some(idx) = list.iter().position(|v| v.as_slice() == pivot) else {
            return Ok(router::integer(-1));
        };
        let insert_at = if before { idx } else { idx + 1 };
        list.insert(insert_at, element);
        let len = list.len() as i64;
        self.storage.rewrite_stored(db, key, stored).await?;
        Ok(router::integer(len))
    }

    pub async fn lmove(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LMOVE", args, 4)?;
        let source = &args[0];
        let destination = &args[1];
        let from_left = parse_side(&args[2])?;
        let to_left = parse_side(&args[3])?;

        let (_lock_a, _lock_b) = self.key_lock.lock_two(source, destination).await;
        let same_key = source == destination;

        let Some(mut src_stored) = self.storage.get_typed(db, source).await? else {
            return Ok(router::nil_bulk());
        };
        let src_list = src_stored.as_list_mut()?;
        if src_list.is_empty() {
            return Ok(router::nil_bulk());
        }

        if !same_key {
            if let Some(dest_stored) = self.storage.get_typed(db, destination).await? {
                if !matches!(dest_stored.value, ValueType::List(_)) {
                    return Err(router::wrongtype());
                }
            }
        }

        let element = if from_left {
            src_list.pop_front().expect("non-empty")
        } else {
            src_list.pop_back().expect("non-empty")
        };

        if same_key {
            if to_left {
                src_list.push_front(element.clone());
            } else {
                src_list.push_back(element.clone());
            }
            self.storage.rewrite_stored(db, source, src_stored).await?;
            return Ok(router::bulk(element));
        }

        if src_list.is_empty() {
            self.storage.delete(db, source).await?;
        } else {
            self.storage.rewrite_stored(db, source, src_stored).await?;
        }

        let mut dest_stored = match self.storage.get_typed(db, destination).await? {
            None => StoredValue::new_list(VecDeque::new()),
            Some(stored) => stored,
        };
        let dest_list = dest_stored.as_list_mut()?;
        if to_left {
            dest_list.push_front(element.clone());
        } else {
            dest_list.push_back(element.clone());
        }
        self.storage
            .rewrite_stored(db, destination, dest_stored)
            .await?;
        Ok(router::bulk(element))
    }

    pub async fn lpos(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("LPOS", args, 2)?;
        let key = &args[0];
        let element = args[1].as_ref();
        let opts = parse_lpos_options(&args[2..])?;

        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return if opts.count.is_some() {
                Ok(RespValue::Array(Some(vec![])))
            } else {
                Ok(router::nil_bulk())
            };
        };
        let list = stored.as_list()?;
        find_lpos(list, element, &opts)
    }

    pub async fn ltrim(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LTRIM", args, 3)?;
        let start = parse_i64(&args[1])?;
        let stop = parse_i64(&args[2])?;
        let _lock = self.key_lock.lock(&args[0]).await;
        let Some(mut stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(router::ok());
        };
        let list = stored.as_list_mut()?;
        let (start_idx, stop_idx) = normalize_range(list.len(), start, stop);
        if start_idx > stop_idx {
            self.storage.delete(db, &args[0]).await?;
            return Ok(router::ok());
        }
        let trimmed: VecDeque<Vec<u8>> = list
            .iter()
            .skip(start_idx)
            .take(stop_idx - start_idx + 1)
            .cloned()
            .collect();
        *list = trimmed;
        if list.is_empty() {
            self.storage.delete(db, &args[0]).await?;
        } else {
            self.storage.rewrite_stored(db, &args[0], stored).await?;
        }
        Ok(router::ok())
    }

    async fn load_list(&self, db: usize, key: &[u8]) -> Result<Option<VecDeque<Vec<u8>>>> {
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(None);
        };
        Ok(Some(stored.as_list()?.clone()))
    }

    async fn load_or_create_list(&self, db: usize, key: &[u8]) -> Result<StoredValue> {
        match self.storage.get_typed(db, key).await? {
            None => Ok(StoredValue::new_list(VecDeque::new())),
            Some(stored) => match stored.value {
                ValueType::List(_) => Ok(stored),
                _ => Err(router::wrongtype()),
            },
        }
    }

    // -- 阻塞命令 --

    /// Parse timeout float from Bytes (last arg for BLPOP/BRPOP/BZPOP*)
    pub(crate) fn parse_timeout_secs(b: &Bytes) -> Result<f64> {
        let s = std::str::from_utf8(b)
            .map_err(|_| Error::Command("ERR timeout is not a float".into()))?;
        let n = s
            .parse::<f64>()
            .map_err(|_| Error::Command("ERR timeout is not a float".into()))?;
        if n < 0.0 || n.is_nan() {
            return Err(Error::Command("ERR timeout is negative".into()));
        }
        Ok(n)
    }

    /// Non-blocking: try to pop from first non-empty key. Returns None if all empty.
    async fn try_pop_any(
        &self,
        db: usize,
        keys: &[&Bytes],
        left: bool,
    ) -> Result<Option<RespValue>> {
        for &key in keys {
            let Some(mut stored) = self.storage.get_typed(db, key).await? else {
                continue;
            };
            let list = match stored.as_list_mut() {
                Ok(l) => l,
                Err(_) => continue,
            };
            if list.is_empty() {
                continue;
            }
            let element = if left {
                list.pop_front().expect("non-empty")
            } else {
                list.pop_back().expect("non-empty")
            };
            if list.is_empty() {
                self.storage.delete(db, key).await?;
            } else {
                self.storage.rewrite_stored(db, key, stored).await?;
            }
            return Ok(Some(RespValue::Array(Some(vec![
                RespValue::BulkString(Some(key.clone())),
                RespValue::BulkString(Some(bytes::Bytes::from(element))),
            ]))));
        }
        Ok(None)
    }

    pub async fn blpop(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("BLPOP", args, 2)?;
        let keys: Vec<&Bytes> = args[..args.len() - 1].iter().collect();
        let timeout = Self::parse_timeout_secs(&args[args.len() - 1])?;
        self.blocking_pop(db, &keys, timeout, true).await
    }

    pub async fn brpop(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("BRPOP", args, 2)?;
        let keys: Vec<&Bytes> = args[..args.len() - 1].iter().collect();
        let timeout = Self::parse_timeout_secs(&args[args.len() - 1])?;
        self.blocking_pop(db, &keys, timeout, false).await
    }

    async fn blocking_pop(
        &self,
        db: usize,
        keys: &[&Bytes],
        timeout_secs: f64,
        left: bool,
    ) -> Result<RespValue> {
        let infinite = timeout_secs == 0.0;
        let dur = if infinite {
            Duration::from_secs(60 * 60 * 24 * 365)
        } else {
            Duration::from_secs_f64(timeout_secs)
        };

        let registry = BlockingRegistry::global();
        let deadline = Instant::now() + dur;
        let mut receivers = match self
            .prepare_blocking_pop(db, keys, left, dur, registry)
            .await?
        {
            Ok(response) => return Ok(response),
            Err(receivers) => receivers,
        };
        let _blocked = BlockedClientGuard::enter(&self.metrics);

        while infinite || Instant::now() < deadline {
            let mut notified = false;
            for rx in &mut receivers {
                match rx.try_recv() {
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
                    .prepare_blocking_pop(db, keys, left, remaining, registry)
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

    async fn prepare_blocking_pop(
        &self,
        db: usize,
        keys: &[&Bytes],
        left: bool,
        timeout: Duration,
        registry: &BlockingRegistry,
    ) -> Result<std::result::Result<RespValue, Vec<oneshot::Receiver<RespValue>>>> {
        let gate = Arc::clone(&self.transaction_gate);
        let _guard = gate.read_owned().await;
        if let Some(response) = self.try_pop_any(db, keys, left).await? {
            return Ok(Ok(response));
        }
        let receivers = keys
            .iter()
            .map(|key| registry.register(key.to_vec(), timeout))
            .collect();
        if let Some(response) = self.try_pop_any(db, keys, left).await? {
            return Ok(Ok(response));
        }
        Ok(Err(receivers))
    }

    /// BLMOVE with blocking when source is empty
    pub async fn blmove_blocking(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("BLMOVE", args, 5)?;
        let source = &args[0];
        let timeout = Self::parse_timeout_secs(&args[4])?;

        let infinite = timeout == 0.0;
        let dur = if infinite {
            Duration::from_secs(60 * 60 * 24 * 365)
        } else {
            Duration::from_secs_f64(timeout)
        };

        let registry = BlockingRegistry::global();
        let deadline = Instant::now() + dur;
        let mut receiver = match self.prepare_blmove(db, args, dur, registry, source).await? {
            Ok(response) => return Ok(response),
            Err(receiver) => receiver,
        };
        let _blocked = BlockedClientGuard::enter(&self.metrics);

        while infinite || Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(blocking::nil_blocking_response());
            }
            let result = tokio::time::timeout(remaining, &mut receiver).await;
            match result {
                Ok(_) => {
                    match self
                        .prepare_blmove(db, args, remaining, registry, source)
                        .await?
                    {
                        Ok(response) => return Ok(response),
                        Err(next) => receiver = next,
                    }
                }
                Err(_) => {
                    // Timeout
                    return Ok(blocking::nil_blocking_response());
                }
            }
        }

        Ok(blocking::nil_blocking_response())
    }

    async fn prepare_blmove(
        &self,
        db: usize,
        args: &[Bytes],
        timeout: Duration,
        registry: &BlockingRegistry,
        source: &Bytes,
    ) -> Result<std::result::Result<RespValue, oneshot::Receiver<RespValue>>> {
        let gate = Arc::clone(&self.transaction_gate);
        let _guard = gate.read_owned().await;
        let response = self.lmove(db, &args[..4]).await?;
        if !matches!(response, RespValue::BulkString(None)) {
            return Ok(Ok(response));
        }
        let receiver = registry.register(source.to_vec(), timeout);
        let response = self.lmove(db, &args[..4]).await?;
        if !matches!(response, RespValue::BulkString(None)) {
            return Ok(Ok(response));
        }
        Ok(Err(receiver))
    }
}

pub(crate) fn normalize_range(len: usize, start: i64, stop: i64) -> (usize, usize) {
    let len_i = len as i64;
    let start_idx = if start < 0 {
        (len_i + start).max(0) as usize
    } else {
        (start as usize).min(len)
    };
    let stop_idx = if stop < 0 {
        (len_i + stop).max(0) as usize
    } else {
        (stop as usize).min(len.saturating_sub(1))
    };
    (start_idx, stop_idx)
}

pub(crate) fn normalize_index(len: usize, index: i64) -> usize {
    if index < 0 {
        (len as i64 + index) as usize
    } else {
        index as usize
    }
}

pub(crate) fn remove_direction(
    list: &mut VecDeque<Vec<u8>>,
    element: &[u8],
    max: usize,
    from_front: bool,
) -> usize {
    let mut removed = 0;
    if from_front {
        let mut i = 0;
        while i < list.len() && removed < max {
            if list[i].as_slice() == element {
                list.remove(i);
                removed += 1;
            } else {
                i += 1;
            }
        }
    } else {
        let mut i = list.len();
        while i > 0 && removed < max {
            i -= 1;
            if list[i].as_slice() == element {
                list.remove(i);
                removed += 1;
            }
        }
    }
    removed
}

fn array_of_bulk(items: Vec<Vec<u8>>) -> RespValue {
    RespValue::Array(Some(items.into_iter().map(router::bulk).collect()))
}

fn parse_i64(b: &Bytes) -> Result<i64> {
    let s =
        std::str::from_utf8(b).map_err(|_| Error::Command("ERR value is not an integer".into()))?;
    s.parse::<i64>()
        .map_err(|_| Error::Command("ERR value is not an integer".into()))
}

fn parse_pop_count(b: &Bytes) -> Result<i64> {
    let n = parse_i64(b)?;
    if n < 0 {
        return Err(Error::Command(
            "ERR value is not an integer or out of range".into(),
        ));
    }
    Ok(n)
}

fn eq_ignore_case(a: &Bytes, b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.eq_ignore_ascii_case(y))
}

pub(crate) fn parse_side(b: &Bytes) -> Result<bool> {
    if eq_ignore_case(b, b"LEFT") {
        Ok(true)
    } else if eq_ignore_case(b, b"RIGHT") {
        Ok(false)
    } else {
        Err(Error::Command("ERR syntax error".into()))
    }
}

pub(crate) struct LposOptions {
    pub(crate) rank: i64,
    pub(crate) count: Option<i64>,
    pub(crate) maxlen: i64,
}

pub(crate) fn parse_lpos_options(args: &[Bytes]) -> Result<LposOptions> {
    let mut rank = 1_i64;
    let mut count = None;
    let mut maxlen = 0_i64;
    let mut i = 0;
    while i < args.len() {
        if eq_ignore_case(&args[i], b"RANK") {
            if i + 1 >= args.len() {
                return Err(router::wrong_args("LPOS", ""));
            }
            rank = parse_i64(&args[i + 1])?;
            i += 2;
        } else if eq_ignore_case(&args[i], b"COUNT") {
            if i + 1 >= args.len() {
                return Err(router::wrong_args("LPOS", ""));
            }
            count = Some(parse_i64(&args[i + 1])?);
            i += 2;
        } else if eq_ignore_case(&args[i], b"MAXLEN") {
            if i + 1 >= args.len() {
                return Err(router::wrong_args("LPOS", ""));
            }
            maxlen = parse_i64(&args[i + 1])?;
            if maxlen < 0 {
                return Err(Error::Command(
                    "ERR value is not an integer or out of range".into(),
                ));
            }
            i += 2;
        } else {
            return Err(router::wrong_args("LPOS", ""));
        }
    }
    Ok(LposOptions {
        rank,
        count,
        maxlen,
    })
}

pub(crate) fn find_lpos(
    list: &VecDeque<Vec<u8>>,
    element: &[u8],
    opts: &LposOptions,
) -> Result<RespValue> {
    if opts.rank == 0 {
        return Err(Error::Command("ERR RANK can't be zero".into()));
    }

    let len = list.len();
    let abs_rank = opts.rank.unsigned_abs() as usize;

    let indices: Vec<usize> = if opts.rank > 0 {
        let end = if opts.maxlen > 0 {
            (opts.maxlen as usize).min(len)
        } else {
            len
        };
        (0..end)
            .filter(|&i| list[i].as_slice() == element)
            .collect()
    } else {
        let start = if opts.maxlen > 0 {
            len.saturating_sub(opts.maxlen as usize)
        } else {
            0
        };
        (start..len)
            .rev()
            .filter(|&i| list[i].as_slice() == element)
            .collect()
    };

    if indices.len() < abs_rank {
        return if opts.count.is_some() {
            Ok(RespValue::Array(Some(vec![])))
        } else {
            Ok(router::nil_bulk())
        };
    }

    let remaining = &indices[abs_rank - 1..];
    match opts.count {
        None => Ok(router::integer(remaining[0] as i64)),
        Some(0) => Ok(RespValue::Array(Some(
            remaining
                .iter()
                .map(|&i| router::integer(i as i64))
                .collect(),
        ))),
        Some(n) if n > 0 => {
            let take = (n as usize).min(remaining.len());
            Ok(RespValue::Array(Some(
                remaining[..take]
                    .iter()
                    .map(|&i| router::integer(i as i64))
                    .collect(),
            )))
        }
        Some(_) => Err(Error::Command(
            "ERR value is not an integer or out of range".into(),
        )),
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

    /// Issue #78: BLPOP 被空通知唤醒后必须无丢通知地完成重新注册.
    #[tokio::test]
    async fn issue_78_blpop_reregisters_after_empty_notification() {
        let key = Bytes::from_static(b"issue-78-blpop-reregister");
        let commands = Arc::new(ListCommands::new(
            MemoryEngine::new(1),
            Arc::new(KeyLock::new(4096)),
            Arc::new(tokio::sync::RwLock::new(())),
        ));
        let task = {
            let commands = Arc::clone(&commands);
            let key = key.clone();
            tokio::spawn(async move { commands.blpop(0, &[key, Bytes::from_static(b"5")]).await })
        };

        wait_for_waiter(&key).await;
        BlockingRegistry::global().notify(&key, RespValue::SimpleString("OK".into()));
        wait_for_waiter(&key).await;
        task.abort();
    }

    /// Issue #78: BLMOVE 被空通知唤醒后必须无丢通知地完成重新注册.
    #[tokio::test]
    async fn issue_78_blmove_reregisters_after_empty_notification() {
        let source = Bytes::from_static(b"issue-78-blmove-reregister");
        let commands = Arc::new(ListCommands::new(
            MemoryEngine::new(1),
            Arc::new(KeyLock::new(4096)),
            Arc::new(tokio::sync::RwLock::new(())),
        ));
        let task = {
            let commands = Arc::clone(&commands);
            let source = source.clone();
            tokio::spawn(async move {
                commands
                    .blmove_blocking(
                        0,
                        &[
                            source,
                            Bytes::from_static(b"issue-78-blmove-dest"),
                            Bytes::from_static(b"LEFT"),
                            Bytes::from_static(b"RIGHT"),
                            Bytes::from_static(b"5"),
                        ],
                    )
                    .await
            })
        };

        wait_for_waiter(&source).await;
        BlockingRegistry::global().notify(&source, RespValue::SimpleString("OK".into()));
        wait_for_waiter(&source).await;
        task.abort();
    }
}
