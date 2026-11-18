//! Hash 命令: HSET/HMSET/HGET/HDEL/HEXISTS/HLEN/HKEYS/HVALS/HGETALL/HSETNX/
//! HINCRBY/HINCRBYFLOAT/HSCAN. HMSET 走 HSET 逻辑并返回 `OK`.
//!
//! # 存储表示 (双格式)
//!
//! ```text
//! 小 hash (≤ 64 fields): ValueType::Hash(HashMap<field, value>)
//!                        postcard 存于 StoredValue
//! 大 hash (> 64 fields): ValueType::CollectionHeader { kind: Hash, count }
//!                        field 独立 subkey:
//!                        {encoded_user_key}\x01H{field_len:2B}{field} → raw value
//! ```
//!
//! 写路径: `key_lock.lock(key)` → 读 `InFlightBuffer` 或底库 → 内存修改并写入在途缓冲 →
//! 立即释放 `KeyLock` → 锁外入队 `enqueue_batch` 提交 (含超过 64 时迁移为 subkey).
//! subkey 读写走 `raw_subkey_*` / `WriteOp::{PutEngine,DeleteEngine}`, 仅持久化引擎支持.
//! 类型分轨: `get_typed` / `InFlightBuffer`; 非 Hash → WRONGTYPE.

mod persist;

use std::sync::Arc;

use bytes::Bytes;

use crate::command::router::{self, KeyLock};
use crate::command::scan_util;
use crate::command::shared::InFlightContext;
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::memory::glob_match;
use crate::storage::subkey;
use crate::storage::{AiDbEngine, CollectionKind, KvStorage, ValueType, WRONGTYPE};

use self::persist::HashWriteContext;

pub struct HashCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
    pub(crate) in_flight_ctx: Arc<InFlightContext>,
}

impl HashCommands {
    pub fn new(
        storage: Arc<dyn KvStorage>,
        key_lock: Arc<KeyLock>,
        in_flight_ctx: Arc<InFlightContext>,
    ) -> Self {
        Self {
            storage,
            key_lock,
            in_flight_ctx,
        }
    }

    pub async fn hset(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        if args.len() < 3 || args.len().is_multiple_of(2) {
            return Err(router::wrong_args("HSET", ""));
        }
        let key = &args[0];
        let lock = self.key_lock.lock(key).await;
        let pairs: Vec<(&[u8], &[u8])> = args[1..]
            .chunks(2)
            .map(|c| (c[0].as_ref(), c[1].as_ref()))
            .collect();
        let ctx = HashWriteContext {
            storage: self.storage.as_ref(),
            in_flight: &self.in_flight_ctx,
            db,
            key,
        };
        let new_fields = persist::hset_pipeline(&ctx, &pairs, lock).await?;
        Ok(router::integer(new_fields))
    }

    /// HMSET — Redis 兼容: 始终返回 `OK` (HSET 返回新增字段数).
    pub async fn hmset(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        self.hset(db, args).await?;
        Ok(RespValue::SimpleString("OK".into()))
    }

    pub async fn hget(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HGET", args, 2)?;
        let key_bytes = &args[0];
        let field = args[1].as_ref();

        let Some(stored) = self.storage.get_typed(db, key_bytes).await? else {
            return Ok(router::nil_bulk());
        };

        match &stored.value {
            ValueType::Hash(map) => match map.get(field) {
                Some(v) => Ok(router::bulk(v.clone())),
                None => Ok(router::nil_bulk()),
            },
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                ..
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key_bytes);
                let subkey_key = subkey::encode_hash_field_key(&encoded_user_key, field);
                let raw = self
                    .storage
                    .raw_subkey_get(db, subkey_key)
                    .await
                    .unwrap_or_default();
                match raw {
                    Some(v) => Ok(router::bulk(v)),
                    None => Ok(router::nil_bulk()),
                }
            }
            _ => Err(router::wrongtype()),
        }
    }

    pub async fn hdel(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("HDEL", args, 2)?;
        let key = &args[0];
        let lock = self.key_lock.lock(key).await;
        let fields: Vec<&[u8]> = args[1..].iter().map(|b| b.as_ref()).collect();
        let ctx = HashWriteContext {
            storage: self.storage.as_ref(),
            in_flight: &self.in_flight_ctx,
            db,
            key,
        };
        let removed = persist::hdel_pipeline(&ctx, &fields, lock).await?;
        Ok(router::integer(removed))
    }

    pub async fn hexists(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HEXISTS", args, 2)?;
        let Some(stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(router::integer(0));
        };
        let exists = match &stored.value {
            ValueType::Hash(map) => map.contains_key(args[1].as_ref()),
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                ..
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, &args[0]);
                let subkey_key = subkey::encode_hash_field_key(&encoded_user_key, args[1].as_ref());
                self.storage
                    .raw_subkey_get(db, subkey_key)
                    .await
                    .unwrap_or_default()
                    .is_some()
            }
            _ => return Err(router::wrongtype()),
        };
        Ok(router::integer(i64::from(exists)))
    }

    pub async fn hlen(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HLEN", args, 1)?;
        let len = match self.storage.get_typed(db, &args[0]).await? {
            None => 0,
            Some(stored) => match stored.value {
                ValueType::Hash(map) => map.len() as i64,
                ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count,
                } => count as i64,
                _ => return Err(router::wrongtype()),
            },
        };
        Ok(router::integer(len))
    }

    pub async fn hkeys(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HKEYS", args, 1)?;
        let pairs = self.load_hash_pairs(db, &args[0]).await?;
        Ok(array_of_bulk(pairs.into_iter().map(|(f, _)| f).collect()))
    }

    pub async fn hvals(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HVALS", args, 1)?;
        let pairs = self.load_hash_pairs(db, &args[0]).await?;
        Ok(array_of_bulk(pairs.into_iter().map(|(_, v)| v).collect()))
    }

    pub async fn hgetall(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HGETALL", args, 1)?;
        let pairs = self.load_hash_pairs(db, &args[0]).await?;
        let mut items = Vec::with_capacity(pairs.len() * 2);
        for (k, v) in pairs {
            items.push(router::bulk(k));
            items.push(router::bulk(v));
        }
        Ok(RespValue::Array(Some(items)))
    }

    pub async fn hmget(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("HMGET", args, 2)?;
        let stored = self.storage.get_typed(db, &args[0]).await?;
        let mut items = Vec::with_capacity(args.len() - 1);

        match stored {
            None => {
                for _ in &args[1..] {
                    items.push(router::nil_bulk());
                }
            }
            Some(stored) => match &stored.value {
                ValueType::Hash(map) => {
                    for field in &args[1..] {
                        match map.get(field.as_ref()) {
                            Some(v) => items.push(router::bulk(v.clone())),
                            None => items.push(router::nil_bulk()),
                        }
                    }
                }
                ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    ..
                } => {
                    let encoded_user_key = AiDbEngine::encode_key(db, &args[0]);
                    let subkey_keys: Vec<Vec<u8>> = args[1..]
                        .iter()
                        .map(|field| {
                            subkey::encode_hash_field_key(&encoded_user_key, field.as_ref())
                        })
                        .collect();
                    let values = self.storage.raw_subkey_mget(db, subkey_keys).await?;
                    for raw in values {
                        match raw {
                            Some(v) => items.push(router::bulk(v.to_vec())),
                            None => items.push(router::nil_bulk()),
                        }
                    }
                }
                _ => return Err(router::wrongtype()),
            },
        }
        Ok(RespValue::Array(Some(items)))
    }

    pub async fn hsetnx(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HSETNX", args, 3)?;
        let key = &args[0];
        let lock = self.key_lock.lock(key).await;
        let ctx = HashWriteContext {
            storage: self.storage.as_ref(),
            in_flight: &self.in_flight_ctx,
            db,
            key,
        };
        let res = persist::hsetnx_pipeline(&ctx, args[1].as_ref(), args[2].as_ref(), lock).await?;
        Ok(router::integer(res))
    }

    pub async fn hscan(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("HSCAN", args, 2)?;
        let cursor = scan_util::parse_u64(&args[1])?;
        let opts = scan_util::parse_scan_options("HSCAN", args, 2)?;

        let Some(stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(hscan_response(0, &[]));
        };

        match stored.value {
            ValueType::Hash(map) => {
                let mut p: Vec<_> = map
                    .into_iter()
                    .filter(|(field, _)| {
                        opts.pattern
                            .as_ref()
                            .is_none_or(|p| glob_match(p, field.as_slice()))
                    })
                    .collect();
                p.sort_by(|a, b| a.0.cmp(&b.0));
                let (next_cursor, page) = scan_util::paginate_slice(&p, cursor, opts.count)?;
                Ok(hscan_response(next_cursor, page))
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                ..
            } => self.hscan_subkeys(db, &args[0], cursor, &opts).await,
            _ => Err(router::wrongtype()),
        }
    }

    async fn hscan_subkeys(
        &self,
        db: usize,
        key: &[u8],
        cursor: u64,
        opts: &scan_util::ScanOptions,
    ) -> Result<RespValue> {
        let Some(cursor_mgr) = self.storage.cursor_mgr() else {
            let encoded_user_key = AiDbEngine::encode_key(db, key);
            let mut p = self
                .scan_hash_subkeys(db, &encoded_user_key, opts.pattern.as_deref())
                .await?;
            p.sort_by(|a, b| a.0.cmp(&b.0));
            let (next_cursor, page) = scan_util::paginate_slice(&p, cursor, opts.count)?;
            return Ok(hscan_response(next_cursor, page));
        };

        let encoded_user_key = AiDbEngine::encode_key(db, key);
        let prefix = subkey::hash_subkey_prefix(&encoded_user_key);
        let end_prefix = AiDbEngine::prefix_end(&prefix).unwrap_or_default();

        let start_field = cursor_mgr
            .get(cursor, db, crate::storage::ScanKind::Hash)
            .map_err(|e| Error::Command(e.into()))?
            .map(|(f, _)| f);

        let mut seek_pos = match start_field {
            None => prefix.clone(),
            Some(last_field) => {
                let mut k = subkey::encode_hash_field_key(&encoded_user_key, &last_field);
                k.push(0);
                k
            }
        };

        let budget = (opts.count * 10).clamp(100, 2000);
        let mut pairs = Vec::with_capacity(opts.count);
        let mut raw_read_count = 0;
        let mut last_seen_field = None;
        let mut reached_end = false;

        while pairs.len() < opts.count && raw_read_count < budget {
            let batch_size = ((opts.count - pairs.len()) * 2).clamp(16, 256);
            let raw_entries = self
                .storage
                .raw_range_scan(seek_pos.clone(), end_prefix.clone(), batch_size, false)
                .await?;
            if raw_entries.is_empty() {
                reached_end = true;
                break;
            }
            let is_eof = raw_entries.len() < batch_size;

            for (raw_k, raw_v) in raw_entries {
                raw_read_count += 1;
                seek_pos = raw_k.clone();
                seek_pos.push(0);

                if let Some((kind, field)) = subkey::decode_subkey_ref(&raw_k) {
                    if kind != CollectionKind::Hash {
                        continue;
                    }
                    if opts.pattern.as_ref().is_none_or(|p| glob_match(p, field)) {
                        pairs.push((field.to_vec(), raw_v));
                    }
                    last_seen_field = Some(field.to_vec());
                }

                if pairs.len() >= opts.count || raw_read_count >= budget {
                    break;
                }
            }

            if is_eof && pairs.len() < opts.count && raw_read_count < budget {
                reached_end = true;
                break;
            }
        }

        let next_cursor = if reached_end {
            0
        } else if let Some(last_field) = last_seen_field {
            cursor_mgr.put(db, crate::storage::ScanKind::Hash, last_field, false)
        } else {
            0
        };

        Ok(hscan_response(next_cursor, &pairs))
    }

    pub async fn hincrbyfloat(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HINCRBYFLOAT", args, 3)?;
        let delta = parse_f64_field_bytes(&args[2])?;
        if !delta.is_finite() {
            return Err(Error::Command("ERR value is not a valid float".into()));
        }
        let key = &args[0];
        let lock = self.key_lock.lock(key).await;
        let ctx = HashWriteContext {
            storage: self.storage.as_ref(),
            in_flight: &self.in_flight_ctx,
            db,
            key,
        };
        let bytes = persist::hincrbyfloat_pipeline(&ctx, args[1].as_ref(), delta, lock).await?;
        Ok(router::bulk(bytes))
    }

    pub async fn hincrby(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HINCRBY", args, 3)?;
        let delta = parse_i64_field(&args[2])?;
        let key = &args[0];
        let lock = self.key_lock.lock(key).await;
        let ctx = HashWriteContext {
            storage: self.storage.as_ref(),
            in_flight: &self.in_flight_ctx,
            db,
            key,
        };
        let new_val = persist::hincrby_pipeline(&ctx, args[1].as_ref(), delta, lock).await?;
        Ok(router::integer(new_val))
    }

    // ---- helpers ----

    /// 读取 hash 的所有 (field, value) 对 (兼容 inline 和 subkey), 不经过 `HashMap` 重建,
    /// subkey 格式下 field 唯一, 无需去重.
    async fn load_hash_pairs(&self, db: usize, key: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(Vec::new());
        };
        match stored.value {
            ValueType::Hash(map) => Ok(map.into_iter().collect()),
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                count,
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                self.load_hash_subkeys_bounded(db, &encoded_user_key, count)
                    .await
            }
            _ => Err(router::wrongtype()),
        }
    }

    /// header 格式 hash 的有界读取: 用 header 的 field count 触发引擎有界扫描快路径.
    /// count 在并发写下可能略旧, 多取 `COUNT_SLACK` 条; 达到上限时回退流式扫描,
    /// 保证不漏 field.
    async fn load_hash_subkeys_bounded(
        &self,
        db: usize,
        encoded_user_key: &[u8],
        count: u32,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        const COUNT_SLACK: usize = 8;
        let prefix = subkey::hash_subkey_prefix(encoded_user_key);
        let Some(end) = AiDbEngine::prefix_end(&prefix) else {
            return self.scan_hash_subkeys(db, encoded_user_key, None).await;
        };
        let limit = (count as usize).saturating_add(COUNT_SLACK);
        let mut raw = self
            .storage
            .raw_range_scan(prefix.clone(), end, limit, false)
            .await?;
        if raw.len() >= limit {
            return self.scan_hash_subkeys(db, encoded_user_key, None).await;
        }
        let field_offset = prefix.len() + 2;
        let mut out = Vec::with_capacity(raw.len());
        for (mut encoded, value) in raw.drain(..) {
            if encoded.len() < field_offset {
                continue;
            }
            encoded.drain(..field_offset);
            out.push((encoded, value));
        }
        Ok(out)
    }

    /// 扫描 subkey hash 的所有 field → value.
    async fn scan_hash_subkeys(
        &self,
        db: usize,
        encoded_user_key: &[u8],
        pattern: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let prefix = subkey::hash_subkey_prefix(encoded_user_key);
        // field 紧跟在 `{prefix}{field_len:2B}` 之后, 直接在原 key 缓冲上截取, 省去逐 field 再分配.
        let field_offset = prefix.len() + 2;
        let out = Arc::new(std::sync::Mutex::new(Vec::new()));
        let out_c = out.clone();
        let pattern = pattern.map(|p| p.to_vec());

        let _ = self
            .storage
            .raw_subkey_for_each(
                db,
                prefix,
                Box::new(move |mut encoded, raw| {
                    if encoded.len() < field_offset {
                        return Ok(());
                    }
                    encoded.drain(..field_offset);
                    if let Some(ref pat) = pattern {
                        if !glob_match(pat, &encoded) {
                            return Ok(());
                        }
                    }
                    out_c.lock().unwrap().push((encoded, raw));
                    Ok(())
                }),
            )
            .await;

        Ok(Arc::try_unwrap(out).unwrap().into_inner().unwrap())
    }
}

fn array_of_bulk(items: Vec<Vec<u8>>) -> RespValue {
    RespValue::Array(Some(items.into_iter().map(router::bulk).collect()))
}

fn hscan_response(cursor: u64, page: &[(Vec<u8>, Vec<u8>)]) -> RespValue {
    let mut items = vec![RespValue::BulkString(Some(Bytes::from(cursor.to_string())))];
    let mut pairs = Vec::new();
    for (field, value) in page {
        pairs.push(router::bulk(field.clone()));
        pairs.push(router::bulk(value.clone()));
    }
    items.push(RespValue::Array(Some(pairs)));
    RespValue::Array(Some(items))
}

pub(crate) fn parse_i64_field(b: &[u8]) -> Result<i64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::Command(WRONGTYPE.into()))?;
    s.parse::<i64>()
        .map_err(|_| Error::Command(WRONGTYPE.into()))
}

pub(crate) fn parse_f64_field_bytes(b: &[u8]) -> Result<f64> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Error::Command("ERR hash value is not a valid float".into()))?;
    s.parse::<f64>()
        .map_err(|_| Error::Command("ERR hash value is not a valid float".into()))
}

pub(crate) fn format_hash_float(v: f64) -> String {
    let s = v.to_string();
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}
