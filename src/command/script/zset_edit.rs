//! 脚本里的大 ZSET 增量. 避免把 header 展开成整包后再写回.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;

use crate::command::router;
use crate::command::script::transaction::{
    parse_f64_arg, ScriptTransaction, ZSetDelta, ZSetMemberChange,
};
use crate::error::Result;
use crate::protocol::RespValue;
use crate::storage::zset_subkey::{self, member_key};
use crate::storage::{AiDbEngine, KvStorage, WriteOp};

use super::in_flight::InFlightBuffer;

impl ScriptTransaction {
    pub(crate) async fn uses_subkey_zset(
        &self,
        storage: &dyn KvStorage,
        key: &[u8],
    ) -> Result<bool> {
        if self.zset_deltas.contains_key(key) {
            return Ok(true);
        }
        let stored = self.get_value(storage, key).await?;
        Ok(stored
            .as_ref()
            .and_then(zset_subkey::is_zset_header)
            .is_some())
    }

    pub(crate) fn zset_delta_count(&self, key: &[u8]) -> Option<u32> {
        self.zset_deltas.get(key).map(|delta| delta.count)
    }

    pub(crate) async fn subkey_zadd(
        &mut self,
        storage: &dyn KvStorage,
        args: &[Bytes],
    ) -> Result<RespValue> {
        let key = &args[0];
        self.ensure_zset_delta(storage, key).await?;
        let mut added = 0i64;
        for i in (1..args.len()).step_by(2) {
            let score = parse_f64_arg(&args[i], "score")?;
            let member = args[i + 1].as_ref();
            let current = self.current_member_score(storage, key, member).await?;
            if current.is_none() {
                added += 1;
            }
            self.record_member(key, member, current, Some(score));
        }
        Ok(router::integer(added))
    }

    pub(crate) async fn subkey_zrem(
        &mut self,
        storage: &dyn KvStorage,
        args: &[Bytes],
    ) -> Result<RespValue> {
        let key = &args[0];
        self.ensure_zset_delta(storage, key).await?;
        let mut removed = 0i64;
        for member in &args[1..] {
            let current = self
                .current_member_score(storage, key, member.as_ref())
                .await?;
            if current.is_none() {
                continue;
            }
            removed += 1;
            self.record_member(key, member.as_ref(), current, None);
        }
        Ok(router::integer(removed))
    }

    pub(crate) async fn materialize_zset_delta(
        &self,
        storage: &dyn KvStorage,
        key: &[u8],
    ) -> Result<BTreeMap<Vec<u8>, f64>> {
        let mut map = self.visible_subkey_members(storage, key).await?;
        let delta = &self.zset_deltas[key];
        for (member, change) in &delta.members {
            match change.after {
                Some(score) => {
                    map.insert(member.clone(), score);
                }
                None => {
                    map.remove(member);
                }
            }
        }
        Ok(map)
    }

    pub(crate) async fn append_zset_deltas(
        &self,
        storage: &dyn KvStorage,
        ops: &mut Vec<(Vec<u8>, WriteOp)>,
    ) -> Result<()> {
        for (key, delta) in &self.zset_deltas {
            let expires_at = self.resolve_expires_at(storage, key).await?;
            push_delta(self.db_index, key, delta, expires_at, ops);
        }
        Ok(())
    }

    pub(crate) async fn visible_subkey_members(
        &self,
        storage: &dyn KvStorage,
        key: &[u8],
    ) -> Result<BTreeMap<Vec<u8>, f64>> {
        let _pin = self.in_flight.as_ref().map(|buf| buf.pin_reads());
        let mut map = zset_subkey::load_members(storage, self.db_index, key).await?;
        if let Some(buffer) = &self.in_flight {
            overlay_inflight_members(&mut map, buffer, self.db_index, key);
        }
        Ok(map)
    }

    async fn ensure_zset_delta(&mut self, storage: &dyn KvStorage, key: &[u8]) -> Result<()> {
        if self.zset_deltas.contains_key(key) {
            return Ok(());
        }
        let stored = self.get_value(storage, key).await?;
        let count = match stored {
            Some(stored) => zset_subkey::is_zset_header(&stored).unwrap_or(0),
            None => 0,
        };
        self.zset_deltas.insert(
            key.to_vec(),
            ZSetDelta {
                count,
                members: Default::default(),
            },
        );
        Ok(())
    }

    pub(crate) async fn point_member_score(
        &self,
        storage: &dyn KvStorage,
        key: &[u8],
        member: &[u8],
    ) -> Result<Option<f64>> {
        if let Some(change) = self
            .zset_deltas
            .get(key)
            .and_then(|delta| delta.members.get(member))
        {
            return Ok(change.after);
        }
        point_score(
            self.in_flight.as_ref(),
            storage,
            self.db_index,
            key,
            member,
        )
        .await
    }

    async fn current_member_score(
        &self,
        storage: &dyn KvStorage,
        key: &[u8],
        member: &[u8],
    ) -> Result<Option<f64>> {
        self.point_member_score(storage, key, member).await
    }

    fn record_member(&mut self, key: &[u8], member: &[u8], current: Option<f64>, after: Option<f64>) {
        let delta = self.zset_deltas.get_mut(key).expect("zset delta");
        if delta.members.contains_key(member) {
            let was_present = delta.members[member].after.is_some();
            delta.members.get_mut(member).expect("member change").after = after;
            adjust_count(delta, was_present, after.is_some());
            return;
        }
        adjust_count(delta, current.is_some(), after.is_some());
        delta.members.insert(
            member.to_vec(),
            ZSetMemberChange {
                before: current,
                after,
            },
        );
    }
}

fn overlay_inflight_members(
    map: &mut BTreeMap<Vec<u8>, f64>,
    inflight: &InFlightBuffer,
    db: usize,
    user_key: &[u8],
) {
    let prefix = zset_subkey::member_prefix(&AiDbEngine::encode_key(db, user_key));
    let prefix_len = prefix.len();
    for (engine_key, raw) in inflight.prefix_raw_updates(&prefix) {
        let Some(member) = zset_subkey::member_from_encoded(&engine_key, prefix_len) else {
            continue;
        };
        match raw.as_deref().and_then(zset_subkey::decode_score) {
            Some(score) => {
                map.insert(member, score);
            }
            None => {
                map.remove(&member);
            }
        }
    }
}

fn adjust_count(delta: &mut ZSetDelta, was_present: bool, now_present: bool) {
    match (was_present, now_present) {
        (false, true) => delta.count = delta.count.saturating_add(1),
        (true, false) => delta.count = delta.count.saturating_sub(1),
        _ => {}
    }
}

fn push_delta(
    db: usize,
    key: &[u8],
    delta: &ZSetDelta,
    expires_at: Option<u64>,
    ops: &mut Vec<(Vec<u8>, WriteOp)>,
) {
    if delta.count == 0 {
        ops.push((key.to_vec(), WriteOp::Delete));
    } else {
        ops.push((
            key.to_vec(),
            WriteOp::PutStored(zset_subkey::header(delta.count, expires_at)),
        ));
    }
    for (member, change) in &delta.members {
        if change.before == change.after {
            continue;
        }
        let after = if delta.count == 0 { None } else { change.after };
        ops.extend(zset_subkey::member_change_ops(
            db,
            key,
            member,
            change.before,
            after,
        ));
    }
}

async fn point_score(
    in_flight: Option<&Arc<InFlightBuffer>>,
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    member: &[u8],
) -> Result<Option<f64>> {
    let engine = member_key(&AiDbEngine::encode_key(db, key), member);
    let _pin = in_flight.map(|buf| buf.pin_reads());
    if let Some(buffer) = in_flight {
        if let Some(raw) = buffer.get_raw(&engine) {
            return Ok(raw.as_deref().and_then(zset_subkey::decode_score));
        }
    }
    zset_subkey::read_member_score(storage, db, key, member).await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use tempfile::TempDir;

    use crate::command::CommandRouter;
    use crate::command::script::transaction::ScriptTransaction;
    use crate::storage::zset_subkey::{self, header};
    use crate::storage::{AiDbEngine, KvStorage, KvStorageAdapter, WriteOp};

    use super::super::in_flight::InFlightBuffer;

    #[tokio::test]
    async fn range_read_hides_inflight_member_delete() {
        let dir = TempDir::new().unwrap();
        let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
        let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
        let router = CommandRouter::new(Arc::clone(&storage));
        let mut db = 0;
        for i in 0..65 {
            router
                .execute(
                    "ZADD",
                    &[
                        Bytes::from_static(b"z"),
                        Bytes::from(format!("{}", 100 - i)),
                        Bytes::from(format!("m{i:02}")),
                    ],
                    &mut db,
                )
                .await
                .unwrap();
        }

        let mut ops = zset_subkey::member_change_ops(0, b"z", b"m00", Some(100.0), None);
        let added = zset_subkey::member_key(&AiDbEngine::encode_key(0, b"z"), b"m99");
        ops.push((
            added,
            WriteOp::PutEngine {
                value: zset_subkey::encode_score(7.0),
            },
        ));
        ops.push((b"z".to_vec(), WriteOp::PutStored(header(64, None))));
        let inflight = InFlightBuffer::new();
        inflight.insert_batch(0, &ops);
        let txn = ScriptTransaction::with_in_flight(0, Some(Arc::new(inflight)));
        let (removed_visible, added_visible) = txn
            .with_zset(storage.as_ref(), b"z", |zset| {
                Ok((
                    zset.contains_key(b"m00".as_ref()),
                    zset.get(b"m99".as_ref()).copied() == Some(7.0),
                ))
            })
            .await
            .unwrap()
            .unwrap();
        assert!(
            !removed_visible,
            "in-flight ZREM still visible to the next range read"
        );
        assert!(added_visible, "in-flight member put is missing from the range read");
    }

    #[tokio::test]
    async fn script_zscore_reads_one_member_without_scanning_the_segment() {
        use std::sync::atomic::Ordering;

        use crate::protocol::RespValue;

        let dir = TempDir::new().unwrap();
        let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
        let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
        let router = CommandRouter::new(Arc::clone(&storage));
        let mut db = 0;
        for i in 0..65 {
            router
                .execute(
                    "ZADD",
                    &[
                        Bytes::from_static(b"z"),
                        Bytes::from(format!("{}", 100 - i)),
                        Bytes::from(format!("m{i:02}")),
                    ],
                    &mut db,
                )
                .await
                .unwrap();
        }
        zset_subkey::LOAD_MEMBERS_CALLS.store(0, Ordering::Relaxed);
        let script = Bytes::from(
            "local n = 0\n\
             for i = 0, 64 do\n\
               local score = redis.call('ZSCORE', KEYS[1], string.format('m%02d', i))\n\
               if score then n = n + 1 end\n\
             end\n\
             redis.call('ZREM', KEYS[1], 'm00')\n\
             local gone = redis.call('ZSCORE', KEYS[1], 'm00')\n\
             local kept = redis.call('ZSCORE', KEYS[1], 'm01')\n\
             return {n, gone and 1 or 0, kept}\n",
        );
        let resp = router
            .execute(
                "EVAL",
                &[script, Bytes::from_static(b"1"), Bytes::from_static(b"z")],
                &mut db,
            )
            .await
            .unwrap();
        let RespValue::Array(Some(items)) = resp else {
            panic!("expected array");
        };
        assert_eq!(items[0], RespValue::Integer(65));
        assert_eq!(items[1], RespValue::Integer(0));
        let RespValue::BulkString(Some(kept)) = &items[2] else {
            panic!("missing kept score");
        };
        assert_eq!(kept.as_ref(), b"99");
        let scans = zset_subkey::LOAD_MEMBERS_CALLS.load(Ordering::Relaxed);
        assert_eq!(scans, 0, "ZSCORE scanned the member prefix {scans} times");
    }
}
