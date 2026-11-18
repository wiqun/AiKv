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
use tokio::sync::{oneshot, Notify};

use crate::command::blocking::{self, BlockedClientGuard, BlockingRegistry};
use crate::command::router::{self, KeyLock};
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::server::config::TransactionGate;
use crate::server::ServerMetrics;
use crate::storage::{
    list_subkey, AiDbEngine, CollectionKind, KvStorage, StorageEngineKind, StoredValue, ValueType,
    WriteOp,
};

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
        let stored = if create {
            self.load_or_create_list(db, key).await?
        } else {
            match self.storage.get_typed(db, key).await? {
                None => return Ok(router::integer(0)),
                Some(stored) => match stored.value {
                    ValueType::List(_)
                    | ValueType::CollectionHeader {
                        kind: CollectionKind::List,
                        ..
                    } => stored,
                    _ => return Err(router::wrongtype()),
                },
            }
        };
        let len = self
            .push_into_stored(db, key, stored, elements, front)
            .await?;
        BlockingRegistry::global().notify(key, RespValue::SimpleString("OK".into()));
        Ok(router::integer(len as i64))
    }

    async fn push_into_stored(
        &self,
        db: usize,
        key: &[u8],
        mut stored: StoredValue,
        elements: &[Bytes],
        front: bool,
    ) -> Result<usize> {
        match &mut stored.value {
            ValueType::List(ref mut list) => {
                if front {
                    for el in elements {
                        list.push_front(el.to_vec());
                    }
                } else {
                    for el in elements {
                        list.push_back(el.to_vec());
                    }
                }
                let len = list.len();
                if self.storage.engine_kind() == StorageEngineKind::AiDb
                    && len > list_subkey::list_max_inline()
                {
                    let ops = list_subkey::replace_list_ops(db, key, list, stored.expires_at);
                    self.storage.write_batch(db, ops).await?;
                } else {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(len)
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::List,
                ref mut count,
                version,
                ref mut head,
                ref mut tail,
            } => {
                let encoded = AiDbEngine::encode_key(db, key);
                let version = *version;
                let mut ops = Vec::with_capacity(1 + elements.len());
                if front {
                    for el in elements {
                        *head -= 1;
                        *count = count.saturating_add(1);
                        let sk = list_subkey::encode_list_element_key(&encoded, version, *head);
                        ops.push((sk, WriteOp::put_engine(el.to_vec())));
                    }
                } else {
                    for el in elements {
                        let idx = *tail;
                        *tail += 1;
                        *count = count.saturating_add(1);
                        let sk = list_subkey::encode_list_element_key(&encoded, version, idx);
                        ops.push((sk, WriteOp::put_engine(el.to_vec())));
                    }
                }
                let len = *count as usize;
                ops.push((key.to_vec(), WriteOp::put_stored(stored)));
                self.storage.write_batch(db, ops).await?;
                Ok(len)
            }
            _ => Err(router::wrongtype()),
        }
    }

    async fn pop_from_stored(
        &self,
        db: usize,
        key: &[u8],
        mut stored: StoredValue,
        n: usize,
        front: bool,
    ) -> Result<Vec<Vec<u8>>> {
        match &mut stored.value {
            ValueType::List(ref mut list) => {
                let take_n = n.min(list.len());
                let mut popped = Vec::with_capacity(take_n);
                for _ in 0..take_n {
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
                Ok(popped)
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::List,
                ref mut count,
                version,
                ref mut head,
                ref mut tail,
            } => {
                let take_n = n.min(*count as usize);
                if take_n == 0 {
                    return Ok(Vec::new());
                }
                let encoded = AiDbEngine::encode_key(db, key);
                let version = *version;
                let mut subkeys = Vec::with_capacity(take_n);
                for i in 0..take_n {
                    let idx = if front {
                        *head + i as u64
                    } else {
                        *tail - 1 - i as u64
                    };
                    subkeys.push(list_subkey::encode_list_element_key(&encoded, version, idx));
                }
                let raw_vals = self.storage.raw_subkey_mget(db, subkeys.clone()).await?;
                let mut popped = Vec::with_capacity(take_n);
                let mut ops = Vec::with_capacity(take_n + 1);
                for (sk, raw) in subkeys.into_iter().zip(raw_vals) {
                    if let Some(v) = raw {
                        popped.push(v.to_vec());
                    }
                    ops.push((sk, WriteOp::DeleteEngine));
                }
                if front {
                    *head += take_n as u64;
                } else {
                    *tail -= take_n as u64;
                }
                *count = count.saturating_sub(take_n as u32);
                if *count == 0 {
                    ops.push((key.to_vec(), WriteOp::Delete));
                } else {
                    ops.push((key.to_vec(), WriteOp::put_stored(stored)));
                }
                self.storage.write_batch(db, ops).await?;
                Ok(popped)
            }
            _ => Err(router::wrongtype()),
        }
    }

    pub async fn lpop(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() || args.len() > 2 {
            return Err(router::wrong_args("LPOP", ""));
        }
        let count = if args.len() > 1 {
            Some(parse_pop_count(&args[1])?)
        } else {
            None
        };
        self.pop(db, &args[0], count, true).await
    }

    pub async fn rpop(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() || args.len() > 2 {
            return Err(router::wrong_args("RPOP", ""));
        }
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
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(router::nil_bulk());
        };
        let list_len = Self::stored_list_len(&stored)?;
        if list_len == 0 {
            return Ok(router::nil_bulk());
        }
        let is_count_given = count.is_some();
        let limit = count.unwrap_or(1);
        if limit == 0 {
            return Ok(RespValue::Array(Some(vec![])));
        }
        let popped = self
            .pop_from_stored(db, key, stored, limit as usize, front)
            .await?;
        if popped.is_empty() {
            return Ok(router::nil_bulk());
        }
        if !is_count_given {
            return Ok(router::bulk(popped.into_iter().next().unwrap()));
        }
        Ok(array_of_bulk(popped))
    }

    fn stored_list_len(stored: &StoredValue) -> Result<usize> {
        match &stored.value {
            ValueType::List(list) => Ok(list.len()),
            ValueType::CollectionHeader {
                kind: CollectionKind::List,
                count,
                ..
            } => Ok(*count as usize),
            _ => Err(router::wrongtype()),
        }
    }

    pub async fn llen(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LLEN", args, 1)?;
        let Some(stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(router::integer(0));
        };
        let len = Self::stored_list_len(&stored)?;
        Ok(router::integer(len as i64))
    }

    pub async fn lrange(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LRANGE", args, 3)?;
        let start = parse_i64(&args[1])?;
        let stop = parse_i64(&args[2])?;
        let Some(stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(RespValue::Array(Some(vec![])));
        };
        match stored.value {
            ValueType::List(list) => {
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
            ValueType::CollectionHeader {
                kind: CollectionKind::List,
                count,
                version,
                head,
                ..
            } => {
                let (start_idx, stop_idx) = normalize_range(count as usize, start, stop);
                if start_idx > stop_idx {
                    return Ok(RespValue::Array(Some(vec![])));
                }
                let encoded = AiDbEngine::encode_key(db, &args[0]);
                let items = list_subkey::load_list_range(
                    self.storage.as_ref(),
                    db,
                    &encoded,
                    version,
                    head,
                    start_idx,
                    stop_idx,
                )
                .await?;
                Ok(array_of_bulk(items))
            }
            _ => Err(router::wrongtype()),
        }
    }

    pub async fn lindex(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LINDEX", args, 2)?;
        let index = parse_i64(&args[1])?;
        let Some(stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(router::nil_bulk());
        };
        match stored.value {
            ValueType::List(list) => {
                let idx = normalize_index(list.len(), index);
                match list.get(idx) {
                    None => Ok(router::nil_bulk()),
                    Some(v) => Ok(router::bulk(v.clone())),
                }
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::List,
                count,
                version,
                head,
                ..
            } => {
                let idx = normalize_index(count as usize, index);
                if idx >= count as usize {
                    return Ok(router::nil_bulk());
                }
                let encoded = AiDbEngine::encode_key(db, &args[0]);
                let sk = list_subkey::encode_list_element_key(&encoded, version, head + idx as u64);
                match self.storage.raw_subkey_get(db, sk).await? {
                    None => Ok(router::nil_bulk()),
                    Some(v) => Ok(router::bulk(v)),
                }
            }
            _ => Err(router::wrongtype()),
        }
    }

    pub async fn lset(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LSET", args, 3)?;
        let index = parse_i64(&args[1])?;
        let _lock = self.key_lock.lock(&args[0]).await;
        let Some(mut stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Err(Error::Command("ERR no such key".into()));
        };
        match &mut stored.value {
            ValueType::List(ref mut list) => {
                let idx = normalize_index(list.len(), index);
                let Some(slot) = list.get_mut(idx) else {
                    return Err(Error::Command("ERR index out of range".into()));
                };
                *slot = args[2].to_vec();
                self.storage.rewrite_stored(db, &args[0], stored).await?;
                Ok(router::ok())
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::List,
                count,
                version,
                head,
                ..
            } => {
                let idx = normalize_index(*count as usize, index);
                if idx >= *count as usize {
                    return Err(Error::Command("ERR index out of range".into()));
                }
                let encoded = AiDbEngine::encode_key(db, &args[0]);
                let sk =
                    list_subkey::encode_list_element_key(&encoded, *version, *head + idx as u64);
                self.storage
                    .raw_subkey_set(db, sk, args[2].to_vec())
                    .await?;
                Ok(router::ok())
            }
            _ => Err(router::wrongtype()),
        }
    }

    pub async fn lrem(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LREM", args, 3)?;
        let count = parse_i64(&args[1])?;
        let element = args[2].as_ref();
        let _lock = self.key_lock.lock(&args[0]).await;
        let Some(stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(router::integer(0));
        };
        let expires_at = stored.expires_at;
        let mut list = self.materialize_list(db, &args[0], stored).await?;
        let removed = if count == 0 {
            let before = list.len();
            list.retain(|v| v.as_slice() != element);
            before - list.len()
        } else if count > 0 {
            remove_direction(&mut list, element, count as usize, true)
        } else {
            remove_direction(&mut list, element, (-count) as usize, false)
        };
        self.write_materialized_list(db, &args[0], list, expires_at)
            .await?;
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
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(router::integer(0));
        };
        let expires_at = stored.expires_at;
        let mut list = self.materialize_list(db, key, stored).await?;
        let Some(idx) = list.iter().position(|v| v.as_slice() == pivot) else {
            return Ok(router::integer(-1));
        };
        let insert_at = if before { idx } else { idx + 1 };
        list.insert(insert_at, element);
        let len = list.len() as i64;
        self.write_materialized_list(db, key, list, expires_at)
            .await?;
        BlockingRegistry::global().notify(key, RespValue::SimpleString("OK".into()));
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

        let Some(src_stored) = self.storage.get_typed(db, source).await? else {
            return Ok(router::nil_bulk());
        };
        if Self::stored_list_len(&src_stored)? == 0 {
            return Ok(router::nil_bulk());
        }

        if !same_key {
            if let Some(dest_stored) = self.storage.get_typed(db, destination).await? {
                Self::stored_list_len(&dest_stored)?;
            }
        }

        let popped = self
            .pop_from_stored(db, source, src_stored, 1, from_left)
            .await?;
        let Some(element) = popped.into_iter().next() else {
            return Ok(router::nil_bulk());
        };

        let elem_bytes = Bytes::from(element.clone());
        let dest_stored = self.load_or_create_list(db, destination).await?;
        self.push_into_stored(db, destination, dest_stored, &[elem_bytes], to_left)
            .await?;
        BlockingRegistry::global().notify(destination, RespValue::SimpleString("OK".into()));
        Ok(router::bulk(element))
    }

    pub async fn lpos(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("LPOS", args, 2)?;
        let key = &args[0];
        let element = args[1].as_ref();
        let opts = parse_lpos_options(&args[2..])?;

        let Some(list) = self.load_list(db, key).await? else {
            return if opts.count.is_some() {
                Ok(RespValue::Array(Some(vec![])))
            } else {
                Ok(router::nil_bulk())
            };
        };
        find_lpos(&list, element, &opts)
    }

    pub async fn ltrim(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("LTRIM", args, 3)?;
        let start = parse_i64(&args[1])?;
        let stop = parse_i64(&args[2])?;
        let _lock = self.key_lock.lock(&args[0]).await;
        let Some(mut stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(router::ok());
        };
        match &mut stored.value {
            ValueType::List(ref mut list) => {
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
            ValueType::CollectionHeader {
                kind: CollectionKind::List,
                count,
                version,
                head,
                tail,
            } => {
                let len = *count as usize;
                let (start_idx, stop_idx) = normalize_range(len, start, stop);
                if start_idx > stop_idx {
                    self.storage.delete(db, &args[0]).await?;
                    return Ok(router::ok());
                }
                let new_len = stop_idx - start_idx + 1;
                if new_len == len {
                    return Ok(router::ok());
                }
                let encoded = AiDbEngine::encode_key(db, &args[0]);
                if new_len <= list_subkey::list_max_inline() {
                    let kept = list_subkey::load_list_range(
                        self.storage.as_ref(),
                        db,
                        &encoded,
                        *version,
                        *head,
                        start_idx,
                        stop_idx,
                    )
                    .await?;
                    let mut new_stored = StoredValue::new_list(kept.into());
                    new_stored.expires_at = stored.expires_at;
                    self.storage
                        .rewrite_stored(db, &args[0], new_stored)
                        .await?;
                    return Ok(router::ok());
                }
                let removed_count = len - new_len;
                if removed_count > new_len {
                    // 保留部分更小: 用新 version 重写保留元素, 旧 version 交由 compaction 回收
                    let kept = list_subkey::load_list_range(
                        self.storage.as_ref(),
                        db,
                        &encoded,
                        *version,
                        *head,
                        start_idx,
                        stop_idx,
                    )
                    .await?;
                    let ops = list_subkey::replace_list_ops(
                        db,
                        &args[0],
                        &kept.into(),
                        stored.expires_at,
                    );
                    self.storage.write_batch(db, ops).await?;
                } else {
                    let mut ops = Vec::with_capacity(removed_count + 1);
                    for i in 0..start_idx {
                        let sk = list_subkey::encode_list_element_key(
                            &encoded,
                            *version,
                            *head + i as u64,
                        );
                        ops.push((sk, WriteOp::DeleteEngine));
                    }
                    for i in (stop_idx + 1)..len {
                        let sk = list_subkey::encode_list_element_key(
                            &encoded,
                            *version,
                            *head + i as u64,
                        );
                        ops.push((sk, WriteOp::DeleteEngine));
                    }
                    *head += start_idx as u64;
                    *tail = *head + new_len as u64;
                    *count = new_len as u32;
                    ops.push((args[0].to_vec(), WriteOp::put_stored(stored)));
                    self.storage.write_batch(db, ops).await?;
                }
                Ok(router::ok())
            }
            _ => Err(router::wrongtype()),
        }
    }

    async fn materialize_list(
        &self,
        db: usize,
        key: &[u8],
        stored: StoredValue,
    ) -> Result<VecDeque<Vec<u8>>> {
        match stored.value {
            ValueType::List(list) => Ok(list),
            ValueType::CollectionHeader {
                kind: CollectionKind::List,
                version,
                ..
            } => {
                let encoded = AiDbEngine::encode_key(db, key);
                list_subkey::load_list_full(self.storage.as_ref(), db, &encoded, version).await
            }
            _ => Err(router::wrongtype()),
        }
    }

    async fn write_materialized_list(
        &self,
        db: usize,
        key: &[u8],
        list: VecDeque<Vec<u8>>,
        expires_at: Option<u64>,
    ) -> Result<()> {
        if list.is_empty() {
            let _ = self.storage.delete(db, key).await?;
            return Ok(());
        }
        if self.storage.engine_kind() == StorageEngineKind::AiDb
            && list.len() > list_subkey::list_max_inline()
        {
            let ops = list_subkey::replace_list_ops(db, key, &list, expires_at);
            self.storage.write_batch(db, ops).await
        } else {
            let mut stored = StoredValue::new_list(list);
            stored.expires_at = expires_at;
            self.storage.rewrite_stored(db, key, stored).await
        }
    }

    async fn load_list(&self, db: usize, key: &[u8]) -> Result<Option<VecDeque<Vec<u8>>>> {
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(None);
        };
        Ok(Some(self.materialize_list(db, key, stored).await?))
    }

    async fn load_or_create_list(&self, db: usize, key: &[u8]) -> Result<StoredValue> {
        match self.storage.get_typed(db, key).await? {
            None => Ok(StoredValue::new_list(VecDeque::new())),
            Some(stored) => match stored.value {
                ValueType::List(_)
                | ValueType::CollectionHeader {
                    kind: CollectionKind::List,
                    ..
                } => Ok(stored),
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
            let Some(stored) = self.storage.get_typed(db, key).await? else {
                continue;
            };
            let Ok(list_len) = Self::stored_list_len(&stored) else {
                continue;
            };
            if list_len == 0 {
                continue;
            }
            let popped = self.pop_from_stored(db, key, stored, 1, left).await?;
            if let Some(el) = popped.into_iter().next() {
                return Ok(Some(RespValue::Array(Some(vec![
                    RespValue::BulkString(Some(key.clone())),
                    RespValue::BulkString(Some(bytes::Bytes::from(el))),
                ]))));
            }
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
        let mut notify = match self
            .prepare_blocking_pop(db, keys, left, dur, registry)
            .await?
        {
            Ok(response) => return Ok(response),
            Err(notify) => notify,
        };
        let _blocked = BlockedClientGuard::enter(&self.metrics);

        while infinite || Instant::now() < deadline {
            let remaining = if infinite {
                Duration::from_secs(60 * 60 * 24 * 365)
            } else {
                deadline.saturating_duration_since(Instant::now())
            };
            if remaining.is_zero() {
                break;
            }
            if tokio::time::timeout(remaining, notify.notified())
                .await
                .is_err()
            {
                break;
            }
            let next_remaining = if infinite {
                Duration::from_secs(60 * 60 * 24 * 365)
            } else {
                deadline.saturating_duration_since(Instant::now())
            };
            if next_remaining.is_zero() {
                break;
            }
            match self
                .prepare_blocking_pop(db, keys, left, next_remaining, registry)
                .await?
            {
                Ok(response) => return Ok(response),
                Err(next_notify) => notify = next_notify,
            }
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
    ) -> Result<std::result::Result<RespValue, Arc<Notify>>> {
        let gate = Arc::clone(&self.transaction_gate);
        let _guard = gate.read_owned().await;
        if let Some(response) = self.try_pop_any(db, keys, left).await? {
            return Ok(Ok(response));
        }
        let notify = Arc::new(Notify::new());
        for key in keys {
            registry.register_notify(key.to_vec(), Arc::clone(&notify), timeout);
        }
        if let Some(response) = self.try_pop_any(db, keys, left).await? {
            return Ok(Ok(response));
        }
        Ok(Err(notify))
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
    if len == 0 {
        return (1, 0);
    }
    let len_i = len as i64;
    let start_idx = if start < 0 {
        (len_i + start).max(0) as usize
    } else {
        (start as usize).min(len)
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

    #[tokio::test]
    async fn test_lpop_rpop_with_count() {
        let commands = ListCommands::new(
            MemoryEngine::new(1),
            Arc::new(KeyLock::new(4096)),
            Arc::new(tokio::sync::RwLock::new(())),
        );
        let key = Bytes::from_static(b"list_pop_count");
        commands
            .rpush(
                0,
                &[
                    key.clone(),
                    Bytes::from_static(b"a"),
                    Bytes::from_static(b"b"),
                    Bytes::from_static(b"c"),
                    Bytes::from_static(b"d"),
                ],
            )
            .await
            .unwrap();

        // 1. 无 count: 返回单个 bulk string
        let res = commands.lpop(0, std::slice::from_ref(&key)).await.unwrap();
        assert_eq!(res, router::bulk(b"a".to_vec()));

        // 2. 带 count 2: 返回包含 2 个元素的 array
        let res = commands
            .lpop(0, &[key.clone(), Bytes::from_static(b"2")])
            .await
            .unwrap();
        assert_eq!(res, array_of_bulk(vec![b"b".to_vec(), b"c".to_vec()]));

        // 3. RPOP 带 count 1: 返回包含 1 个元素的 array (带 count 始终返回 Array)
        let res = commands
            .rpop(0, &[key.clone(), Bytes::from_static(b"1")])
            .await
            .unwrap();
        assert_eq!(res, array_of_bulk(vec![b"d".to_vec()]));

        // 4. 列表已空: 返回 nil
        let res = commands
            .lpop(0, &[key.clone(), Bytes::from_static(b"1")])
            .await
            .unwrap();
        assert_eq!(res, router::nil_bulk());
    }

    #[tokio::test]
    async fn list_upgrades_to_subkey_past_inline_threshold() {
        use crate::storage::{AiDbEngine, CollectionKind, KvStorageAdapter};

        let dir = tempfile::TempDir::new().unwrap();
        let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
        let storage = KvStorageAdapter::new(engine);
        let commands = ListCommands::new(
            storage.clone(),
            Arc::new(KeyLock::new(256)),
            Arc::new(tokio::sync::RwLock::new(())),
        );
        let key = Bytes::from_static(b"big_list");
        let inline_cap = crate::storage::list_subkey::list_max_inline();
        let total = inline_cap + 5;

        let mut push_args = vec![key.clone()];
        for i in 0..total {
            push_args.push(Bytes::from(format!("e{i:02}")));
        }
        let len = commands.rpush(0, &push_args).await.unwrap();
        assert_eq!(len, router::integer(total as i64));

        // 应升级为 CollectionHeader { kind: List }
        let stored = storage.get_typed(0, &key).await.unwrap().unwrap();
        assert!(matches!(
            stored.value,
            ValueType::CollectionHeader {
                kind: CollectionKind::List,
                count,
                ..
            } if count as usize == total
        ));

        // LLEN / LINDEX / LRANGE / LSET / LINSERT / LREM / LPOS / LTRIM / LPOP / RPOP
        assert_eq!(
            commands.llen(0, std::slice::from_ref(&key)).await.unwrap(),
            router::integer(total as i64)
        );
        assert_eq!(
            commands
                .lindex(0, &[key.clone(), Bytes::from_static(b"0")])
                .await
                .unwrap(),
            router::bulk(b"e00".to_vec())
        );
        assert_eq!(
            commands
                .lindex(0, &[key.clone(), Bytes::from_static(b"-1")])
                .await
                .unwrap(),
            router::bulk(format!("e{:02}", total - 1).into_bytes())
        );
        commands
            .lset(
                0,
                &[
                    key.clone(),
                    Bytes::from_static(b"1"),
                    Bytes::from_static(b"mid"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(
            commands
                .lrange(
                    0,
                    &[
                        key.clone(),
                        Bytes::from_static(b"0"),
                        Bytes::from_static(b"2"),
                    ]
                )
                .await
                .unwrap(),
            array_of_bulk(vec![b"e00".to_vec(), b"mid".to_vec(), b"e02".to_vec()])
        );
        commands
            .linsert(
                0,
                &[
                    key.clone(),
                    Bytes::from_static(b"BEFORE"),
                    Bytes::from_static(b"mid"),
                    Bytes::from_static(b"ins"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(
            commands
                .lpos(0, &[key.clone(), Bytes::from_static(b"ins")])
                .await
                .unwrap(),
            router::integer(1)
        );
        assert_eq!(
            commands
                .lrem(
                    0,
                    &[
                        key.clone(),
                        Bytes::from_static(b"1"),
                        Bytes::from_static(b"ins")
                    ]
                )
                .await
                .unwrap(),
            router::integer(1)
        );
        commands
            .ltrim(
                0,
                &[
                    key.clone(),
                    Bytes::from_static(b"0"),
                    Bytes::from_static(b"1"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(
            commands.lpop(0, std::slice::from_ref(&key)).await.unwrap(),
            router::bulk(b"e00".to_vec())
        );
        assert_eq!(
            commands.rpop(0, std::slice::from_ref(&key)).await.unwrap(),
            router::bulk(b"mid".to_vec())
        );
        // 弹空后 header 必须被删除
        assert!(storage.get_typed(0, &key).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn blpop_wakes_immediately_on_lmove_and_linsert() {
        let commands = Arc::new(ListCommands::new(
            MemoryEngine::new(1),
            Arc::new(KeyLock::new(4096)),
            Arc::new(tokio::sync::RwLock::new(())),
        ));

        // 1. LMOVE 唤醒目标 key 上的 BLPOP
        let src = Bytes::from_static(b"blpop_lmove_src");
        let dst = Bytes::from_static(b"blpop_lmove_dst");
        commands
            .rpush(0, &[src.clone(), Bytes::from_static(b"v1")])
            .await
            .unwrap();

        let cmd1 = Arc::clone(&commands);
        let dst1 = dst.clone();
        let handle =
            tokio::spawn(async move { cmd1.blpop(0, &[dst1, Bytes::from_static(b"0.3")]).await });
        wait_for_waiter(&dst).await;

        let t0 = Instant::now();
        commands
            .lmove(
                0,
                &[
                    src,
                    dst.clone(),
                    Bytes::from_static(b"LEFT"),
                    Bytes::from_static(b"RIGHT"),
                ],
            )
            .await
            .unwrap();
        let res = handle.await.unwrap().unwrap();
        assert_eq!(res, array_of_bulk(vec![dst.to_vec(), b"v1".to_vec()]));
        assert!(
            t0.elapsed() < Duration::from_millis(50),
            "LMOVE should wake BLPOP immediately"
        );

        // 2. LINSERT 唤醒等待者
        let ins_key = Bytes::from_static(b"blpop_linsert_k");
        commands
            .rpush(0, &[ins_key.clone(), Bytes::from_static(b"pivot")])
            .await
            .unwrap();
        // 先弹空后再测试? LINSERT 需要有 pivot，这里测试并发时: 插入元素后立刻通知
        let _ = commands.lpop(0, std::slice::from_ref(&ins_key)).await;
        // 通过两个并发: 先让一个 BLPOP 等在 k 上, 不好与 LINSERT 并发(LINSERT 需非空);
        // 改为: 预先注册 waiter 在 ins_key 上验证 linsert 触发了 notify
        let notify_rx =
            BlockingRegistry::global().register(ins_key.to_vec(), Duration::from_secs(1));
        commands
            .rpush(0, &[ins_key.clone(), Bytes::from_static(b"pivot")])
            .await
            .unwrap();
        // 清掉 rpush 的 notify, 重新注册验证 linsert 自身也会 notify
        drop(notify_rx);
        let rx_after =
            BlockingRegistry::global().register(ins_key.to_vec(), Duration::from_secs(1));
        commands
            .linsert(
                0,
                &[
                    ins_key,
                    Bytes::from_static(b"BEFORE"),
                    Bytes::from_static(b"pivot"),
                    Bytes::from_static(b"new_el"),
                ],
            )
            .await
            .unwrap();
        let woke = tokio::time::timeout(Duration::from_millis(50), rx_after).await;
        assert!(woke.is_ok(), "LINSERT must notify BlockingRegistry waiters");
    }

    #[tokio::test]
    async fn blpop_multi_waiter_spurious_wakeup_continues_waiting() {
        let commands = Arc::new(ListCommands::new(
            MemoryEngine::new(1),
            Arc::new(KeyLock::new(4096)),
            Arc::new(tokio::sync::RwLock::new(())),
        ));
        let key = Bytes::from_static(b"blpop_multi_waiter");

        let c1 = Arc::clone(&commands);
        let k1 = key.clone();
        let h1 = tokio::spawn(async move { c1.blpop(0, &[k1, Bytes::from_static(b"1")]).await });

        let c2 = Arc::clone(&commands);
        let k2 = key.clone();
        let h2 = tokio::spawn(async move { c2.blpop(0, &[k2, Bytes::from_static(b"1")]).await });

        wait_for_waiter(&key).await;
        // 第一次仅 push 1 个元素
        commands
            .rpush(0, &[key.clone(), Bytes::from_static(b"first")])
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            h1.is_finished() ^ h2.is_finished(),
            "only one waiter should pop the single element"
        );

        // 第二次 push 第 2 个元素, 另一个继续等待的 waiter 必须也能正常收到
        commands
            .rpush(0, &[key.clone(), Bytes::from_static(b"second")])
            .await
            .unwrap();

        let r1 = tokio::time::timeout(Duration::from_millis(200), h1)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let r2 = tokio::time::timeout(Duration::from_millis(200), h2)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(r1 != blocking::nil_blocking_response());
        assert!(r2 != blocking::nil_blocking_response());
    }
}
