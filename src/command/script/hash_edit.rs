//! 脚本里的大 Hash 增量. 避免把 CollectionHeader 展开成整包 HashMap 后再写回.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;

use crate::command::router;
use crate::command::script::transaction::{
    parse_i64_arg, HashDelta, HashFieldChange, ScriptTransaction,
};
use crate::error::Result;
use crate::protocol::RespValue;
use crate::storage::subkey;
use crate::storage::types::{CollectionKind, StoredValue, ValueType};
use crate::storage::{AiDbEngine, WriteOp};

use super::in_flight::InFlightBuffer;

impl ScriptTransaction {
    pub(crate) fn uses_subkey_hash(
        &self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
    ) -> Result<bool> {
        if self.hash_deltas.contains_key(key) {
            return Ok(true);
        }
        if self.write_buffer.contains_key(key) {
            return Ok(false);
        }
        let stored = self.get_value(storage, key)?;
        Ok(matches!(
            stored.as_ref().map(|s| &s.value),
            Some(ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                ..
            })
        ))
    }

    pub(crate) fn point_hash_field(
        &self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
        field: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        if let Some(change) = self
            .hash_deltas
            .get(key)
            .and_then(|delta| delta.fields.get(field))
        {
            return Ok(change.after.clone());
        }
        point_field(self.in_flight.as_ref(), storage, self.db_index, key, field)
    }

    pub(crate) fn subkey_hget(
        &self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
        field: &[u8],
    ) -> Result<RespValue> {
        let val = self.point_hash_field(storage, key, field)?;
        Ok(match val {
            Some(v) => router::bulk(v),
            None => RespValue::Null,
        })
    }

    pub(crate) fn subkey_hexists(
        &self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
        field: &[u8],
    ) -> Result<RespValue> {
        let val = self.point_hash_field(storage, key, field)?;
        Ok(router::integer(if val.is_some() { 1 } else { 0 }))
    }

    pub(crate) fn subkey_hgetall(
        &self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
    ) -> Result<RespValue> {
        let encoded = AiDbEngine::encode_key(self.db_index, key);
        let prefix = subkey::hash_subkey_prefix(&encoded);
        let map_m = std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
        let map_c = map_m.clone();
        storage.raw_subkey_for_each(
            self.db_index,
            prefix,
            Box::new(move |enc, raw| {
                if let Some((kind, field)) = subkey::decode_subkey(&enc) {
                    if kind == CollectionKind::Hash {
                        map_c.lock().unwrap().insert(field, raw);
                    }
                }
                Ok(())
            }),
        )?;
        let mut map = std::sync::Arc::try_unwrap(map_m)
            .unwrap()
            .into_inner()
            .unwrap();
        if let Some(delta) = self.hash_deltas.get(key) {
            for (f, change) in &delta.fields {
                match &change.after {
                    Some(v) => {
                        map.insert(f.clone(), v.clone());
                    }
                    None => {
                        map.remove(f);
                    }
                }
            }
        }
        let mut out = Vec::with_capacity(map.len() * 2);
        for (f, v) in map {
            out.push(router::bulk(f));
            out.push(router::bulk(v));
        }
        Ok(RespValue::Array(Some(out)))
    }

    pub(crate) fn hash_delta_count(
        &self,
        key: &[u8],
        storage: &dyn crate::storage::ScriptIo,
    ) -> Result<u32> {
        if let Some(delta) = self.hash_deltas.get(key) {
            return Ok(delta.count);
        }
        let stored = self.get_value(storage, key)?;
        Ok(match stored.as_ref().map(|s| &s.value) {
            Some(ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                count,
            }) => *count,
            _ => 0,
        })
    }

    pub(crate) fn subkey_hset(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        args: &[Bytes],
    ) -> Result<RespValue> {
        let key = &args[0];
        self.ensure_hash_delta(storage, key)?;
        let mut added = 0i64;
        for i in (1..args.len()).step_by(2) {
            let field = args[i].as_ref();
            let value = args[i + 1].to_vec();
            let current = self.point_hash_field(storage, key, field)?;
            if current.is_none() {
                added += 1;
            }
            self.record_field(key, field, current, Some(value));
        }
        if args.len() > 3 {
            Ok(RespValue::SimpleString("OK".into()))
        } else {
            Ok(router::integer(added))
        }
    }

    pub(crate) fn subkey_hset_single(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
        field: &[u8],
        value: &[u8],
    ) -> Result<i64> {
        self.ensure_hash_delta(storage, key)?;
        let current = self.point_hash_field(storage, key, field)?;
        let added = if current.is_none() { 1 } else { 0 };
        self.record_field(key, field, current, Some(value.to_vec()));
        Ok(added)
    }

    pub(crate) fn subkey_hdel_single(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
        field: &[u8],
    ) -> Result<i64> {
        self.ensure_hash_delta(storage, key)?;
        let current = self.point_hash_field(storage, key, field)?;
        let removed = if current.is_some() { 1 } else { 0 };
        if removed > 0 {
            self.record_field(key, field, current, None);
        }
        Ok(removed)
    }

    pub(crate) fn subkey_hsetnx(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        args: &[Bytes],
    ) -> Result<RespValue> {
        let key = &args[0];
        let field = args[1].as_ref();
        let current = self.point_hash_field(storage, key, field)?;
        if current.is_some() {
            return Ok(router::integer(0));
        }
        self.ensure_hash_delta(storage, key)?;
        let value = args[2].to_vec();
        self.record_field(key, field, None, Some(value));
        Ok(router::integer(1))
    }

    pub(crate) fn subkey_hdel(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        args: &[Bytes],
    ) -> Result<RespValue> {
        let key = &args[0];
        self.ensure_hash_delta(storage, key)?;
        let mut removed = 0i64;
        for f in &args[1..] {
            let field = f.as_ref();
            let current = self.point_hash_field(storage, key, field)?;
            if current.is_none() {
                continue;
            }
            removed += 1;
            self.record_field(key, field, current, None);
        }
        Ok(router::integer(removed))
    }

    pub(crate) fn subkey_hincrby(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
        field: &[u8],
        increment: i64,
    ) -> Result<RespValue> {
        self.ensure_hash_delta(storage, key)?;
        let current = self.point_hash_field(storage, key, field)?;
        let old_val = match &current {
            Some(bytes) => parse_i64_arg(&Bytes::copy_from_slice(bytes), "HINCRBY")?,
            None => 0,
        };
        let new_val = old_val.checked_add(increment).ok_or_else(|| {
            crate::error::Error::Command("ERR increment or decrement would overflow".into())
        })?;
        let new_bytes = itoa::Buffer::new().format(new_val).as_bytes().to_vec();
        self.record_field(key, field, current, Some(new_bytes));
        Ok(router::integer(new_val))
    }

    pub(crate) fn subkey_hincrbyfloat(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
        field: &[u8],
        delta: f64,
    ) -> Result<RespValue> {
        self.ensure_hash_delta(storage, key)?;
        let current = self.point_hash_field(storage, key, field)?;
        let current_num = match &current {
            Some(bytes) => crate::command::hash::parse_f64_field_bytes(bytes)?,
            None => 0.0,
        };
        let new_num = current_num + delta;
        if !new_num.is_finite() {
            return Err(crate::error::Error::Command(
                "ERR increment would produce NaN or Infinity".into(),
            ));
        }
        let text = crate::command::hash::format_hash_float(new_num);
        let new_bytes = text.as_bytes().to_vec();
        self.record_field(key, field, current, Some(new_bytes));
        Ok(router::bulk(text.into_bytes()))
    }

    pub(crate) fn materialize_hash_delta(
        &self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
    ) -> Result<HashMap<Vec<u8>, Vec<u8>>> {
        let stored = self.get_value(storage, key)?;
        let mut map = match stored {
            Some(stored) => self.load_hash_from_stored(storage, key, &stored)?,
            None => HashMap::new(),
        };
        if let Some(delta) = self.hash_deltas.get(key) {
            for (field, change) in &delta.fields {
                match &change.after {
                    Some(v) => {
                        map.insert(field.clone(), v.clone());
                    }
                    None => {
                        map.remove(field);
                    }
                }
            }
        }
        Ok(map)
    }

    pub(crate) fn append_hash_deltas(
        &self,
        storage: &dyn crate::storage::ScriptIo,
        ops: &mut Vec<(Vec<u8>, WriteOp)>,
    ) -> Result<()> {
        for (key, delta) in &self.hash_deltas {
            let expires_at = self.resolve_expires_at(storage, key)?;
            push_hash_delta(self.db_index, key, delta, expires_at, ops);
        }
        Ok(())
    }

    fn ensure_hash_delta(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
    ) -> Result<()> {
        if self.hash_deltas.contains_key(key) {
            return Ok(());
        }
        let stored = self.get_value(storage, key)?;
        let count = match stored.as_ref().map(|s| &s.value) {
            Some(ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                count,
            }) => *count,
            _ => 0,
        };
        self.hash_deltas.insert(
            key.to_vec(),
            HashDelta {
                count,
                fields: HashMap::new(),
            },
        );
        Ok(())
    }

    fn record_field(
        &mut self,
        key: &[u8],
        field: &[u8],
        current: Option<Vec<u8>>,
        after: Option<Vec<u8>>,
    ) {
        let delta = self.hash_deltas.get_mut(key).expect("hash delta");
        if let Some(existing) = delta.fields.get_mut(field) {
            let was_present = existing.after.is_some();
            let is_present = after.is_some();
            if !was_present && is_present {
                delta.count = delta.count.saturating_add(1);
            } else if was_present && !is_present {
                delta.count = delta.count.saturating_sub(1);
            }
            existing.after = after;
        } else {
            let was_present = current.is_some();
            let is_present = after.is_some();
            if !was_present && is_present {
                delta.count = delta.count.saturating_add(1);
            } else if was_present && !is_present {
                delta.count = delta.count.saturating_sub(1);
            }
            delta.fields.insert(
                field.to_vec(),
                HashFieldChange {
                    before: current,
                    after,
                },
            );
        }
    }
}

fn point_field(
    in_flight: Option<&Arc<InFlightBuffer>>,
    storage: &dyn crate::storage::ScriptIo,
    db: usize,
    key: &[u8],
    field: &[u8],
) -> Result<Option<Vec<u8>>> {
    let enc_user_key = AiDbEngine::encode_key(db, key);
    let subkey_key = subkey::encode_hash_field_key(&enc_user_key, field);
    if let Some(buf) = in_flight {
        if let Some(res) = buf.get_raw(&subkey_key) {
            return Ok(res);
        }
    }
    storage.raw_subkey_get(db, subkey_key)
}

fn push_hash_delta(
    db: usize,
    key: &[u8],
    delta: &HashDelta,
    expires_at: Option<u64>,
    ops: &mut Vec<(Vec<u8>, WriteOp)>,
) {
    if delta.count == 0 {
        ops.push((key.to_vec(), WriteOp::Delete));
    } else {
        ops.push((
            key.to_vec(),
            WriteOp::put_stored(StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count: delta.count,
                },
                expires_at,
            }),
        ));
    }
    let enc_user_key = AiDbEngine::encode_key(db, key);
    for (field, change) in &delta.fields {
        if change.before == change.after {
            continue;
        }
        let subkey_key = subkey::encode_hash_field_key(&enc_user_key, field);
        if delta.count == 0 {
            ops.push((subkey_key, WriteOp::DeleteEngine));
        } else {
            match &change.after {
                Some(value) => {
                    ops.push((subkey_key, WriteOp::put_engine(value.clone())));
                }
                None => {
                    ops.push((subkey_key, WriteOp::DeleteEngine));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSubkeyIo {
        header_count: u32,
        subkeys: std::sync::Mutex<HashMap<Vec<u8>, Vec<u8>>>,
    }

    impl crate::storage::ScriptIo for MockSubkeyIo {
        fn engine_kind(&self) -> crate::storage::types::StorageEngineKind {
            crate::storage::types::StorageEngineKind::AiDb
        }
        fn get_typed(&self, _db: usize, _key: &[u8]) -> Result<Option<StoredValue>> {
            Ok(Some(StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count: self.header_count,
                },
                expires_at: None,
            }))
        }
        fn ttl(&self, _db: usize, _key: &[u8]) -> Result<Option<i64>> {
            Ok(None)
        }
        fn write_batch(&self, _db: usize, _ops: Vec<(Vec<u8>, WriteOp)>) -> Result<()> {
            Ok(())
        }
        fn raw_prefix_last(&self, _prefix: Vec<u8>) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
            Ok(None)
        }
        fn raw_range_last(
            &self,
            _start: Vec<u8>,
            _end: Vec<u8>,
        ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
            Ok(None)
        }
        fn raw_range_scan(
            &self,
            _start: Vec<u8>,
            _end: Vec<u8>,
            _limit: usize,
            _reverse: bool,
        ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
            Ok(Vec::new())
        }
        fn raw_subkey_get(&self, _db: usize, encoded_key: Vec<u8>) -> Result<Option<Vec<u8>>> {
            Ok(self.subkeys.lock().unwrap().get(&encoded_key).cloned())
        }
        fn raw_subkey_for_each(
            &self,
            _db: usize,
            prefix: Vec<u8>,
            mut f: crate::storage::types::RawPairVisitor,
        ) -> Result<()> {
            let subkeys = self.subkeys.lock().unwrap();
            for (k, v) in subkeys.iter() {
                if k.starts_with(&prefix) {
                    f(k.clone(), v.clone())?;
                }
            }
            Ok(())
        }
    }

    #[test]
    fn test_hash_delta_materializes_for_with_hash() {
        let key = b"my_manifest";
        let enc_user_key = AiDbEngine::encode_key(0, key);
        let field1_key = subkey::encode_hash_field_key(&enc_user_key, b"f1");

        let mut subkeys = HashMap::new();
        subkeys.insert(field1_key, b"val1".to_vec());

        let io = MockSubkeyIo {
            header_count: 1,
            subkeys: std::sync::Mutex::new(subkeys),
        };

        let mut txn = ScriptTransaction::new(0);
        let hset_args = vec![
            Bytes::from_static(key),
            Bytes::from_static(b"f2"),
            Bytes::from_static(b"val2"),
        ];
        txn.subkey_hset(&io, &hset_args).unwrap();

        // with_hash 必须看到在同一脚本内新写入 hash_deltas 的 f2
        let seen_f2 = txn
            .with_hash(&io, key, |h| Ok(h.get(b"f2".as_slice()).cloned()))
            .unwrap()
            .flatten();
        assert_eq!(seen_f2, Some(b"val2".to_vec()));
    }

    #[test]
    fn test_hash_delta_empty_count_reports_key_not_exists() {
        let key = b"my_manifest";
        let enc_user_key = AiDbEngine::encode_key(0, key);
        let field1_key = subkey::encode_hash_field_key(&enc_user_key, b"f1");

        let mut subkeys = HashMap::new();
        subkeys.insert(field1_key, b"val1".to_vec());

        let io = MockSubkeyIo {
            header_count: 1,
            subkeys: std::sync::Mutex::new(subkeys),
        };

        let mut txn = ScriptTransaction::new(0);
        let removed = txn.subkey_hdel_single(&io, key, b"f1").unwrap();
        assert_eq!(removed, 1);
        assert_eq!(txn.hash_delta_count(key, &io).unwrap(), 0);

        // 删空后，同脚本后续 exists 必须感知为不存在
        assert!(!txn.exists(&io, key).unwrap());
        assert!(txn.get_value(&io, key).unwrap().is_none());
    }

    #[test]
    fn test_replace_stored_cleans_hash_deltas() {
        let key = b"my_manifest";
        let io = MockSubkeyIo {
            header_count: 1,
            subkeys: std::sync::Mutex::new(HashMap::new()),
        };

        let mut txn = ScriptTransaction::new(0);
        let hset_args = vec![
            Bytes::from_static(key),
            Bytes::from_static(b"f1"),
            Bytes::from_static(b"val1"),
        ];
        txn.subkey_hset(&io, &hset_args).unwrap();
        assert!(txn.hash_deltas.contains_key(key.as_slice()));

        txn.replace_stored(
            key.to_vec(),
            StoredValue {
                value: ValueType::String(b"overwritten".to_vec()),
                expires_at: None,
            },
        )
        .unwrap();

        assert!(
            !txn.hash_deltas.contains_key(key.as_slice()),
            "replace_stored 必须清理 hash_deltas"
        );
    }

    #[test]
    fn test_subkey_hincrbyfloat() {
        let key = b"my_manifest";
        let io = MockSubkeyIo {
            header_count: 1,
            subkeys: std::sync::Mutex::new(HashMap::new()),
        };

        let mut txn = ScriptTransaction::new(0);
        let res1 = txn.subkey_hincrbyfloat(&io, key, b"score", 10.5).unwrap();
        assert_eq!(res1, router::bulk(b"10.5".to_vec()));

        let res2 = txn.subkey_hincrbyfloat(&io, key, b"score", 2.25).unwrap();
        assert_eq!(res2, router::bulk(b"12.75".to_vec()));

        let read_back = txn.subkey_hget(&io, key, b"score").unwrap();
        assert_eq!(read_back, router::bulk(b"12.75".to_vec()));
    }

    #[test]
    fn test_subkey_hash_point_access_never_scans_all() {
        let key = b"my_manifest";
        let enc_user_key = AiDbEngine::encode_key(0, key);
        let field1_key = subkey::encode_hash_field_key(&enc_user_key, b"f1");

        let mut subkeys = HashMap::new();
        subkeys.insert(field1_key, b"val1".to_vec());

        let io = MockSubkeyIo {
            header_count: 100,
            subkeys: std::sync::Mutex::new(subkeys),
        };

        let mut txn = ScriptTransaction::new(0);
        assert!(txn.uses_subkey_hash(&io, key).unwrap());

        // 1. subkey_hget point read
        let res = txn.subkey_hget(&io, key, b"f1").unwrap();
        assert_eq!(res, router::bulk(b"val1".to_vec()));

        // 2. subkey_hexists
        assert_eq!(
            txn.subkey_hexists(&io, key, b"f1").unwrap(),
            router::integer(1)
        );
        assert_eq!(
            txn.subkey_hexists(&io, key, b"f2").unwrap(),
            router::integer(0)
        );

        // 3. subkey_hset new field f2
        let hset_args = vec![
            Bytes::from_static(key),
            Bytes::from_static(b"f2"),
            Bytes::from_static(b"val2"),
        ];
        txn.subkey_hset(&io, &hset_args).unwrap();
        assert_eq!(txn.hash_delta_count(key, &io).unwrap(), 101);

        // 4. subkey_hget f2 from delta
        assert_eq!(
            txn.subkey_hget(&io, key, b"f2").unwrap(),
            router::bulk(b"val2".to_vec())
        );

        // 5. prepare_batch_ops: writes only field changes
        let ops = txn.prepare_batch_ops_owned(&io).unwrap();
        assert_eq!(ops.len(), 2); // 1 header + 1 subkey write
    }
}
