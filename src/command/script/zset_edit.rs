//! 脚本里的大 ZSET 增量. 避免把 header 展开成整包后再写回.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;

use crate::command::router;
use crate::command::script::transaction::{
    ScriptTransaction, ZSetDelta, ZSetMemberChange,
};
use crate::error::Result;
use crate::protocol::RespValue;
use crate::storage::zset_subkey::{self, member_key};
use crate::storage::{AiDbEngine, WriteOp};

use super::in_flight::InFlightBuffer;

impl ScriptTransaction {
    pub(crate) fn uses_subkey_zset(
        &self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
    ) -> Result<bool> {
        if self.zset_deltas.contains_key(key) {
            return Ok(true);
        }
        let stored = self.get_value(storage, key)?;
        Ok(stored
            .as_ref()
            .and_then(zset_subkey::is_zset_header)
            .is_some())
    }

    pub(crate) fn zset_delta_count(&self, key: &[u8]) -> Option<u32> {
        self.zset_deltas.get(key).map(|delta| delta.count)
    }

    #[cfg(test)]
    pub(crate) fn subkey_zadd(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        args: &[Bytes],
    ) -> Result<RespValue> {
        use crate::command::script::transaction::parse_f64_arg;
        let (flags, idx) = crate::command::zset::helpers::parse_zadd_flags(args)?;
        let mut pairs = Vec::with_capacity((args.len() - idx) / 2);
        for i in (idx..args.len()).step_by(2) {
            let score = parse_f64_arg(&args[i], "score")?;
            pairs.push((score, args[i + 1].to_vec()));
        }
        self.subkey_zadd_with_flags(storage, &args[0], &pairs, flags)
    }

    pub(crate) fn subkey_zadd_with_flags(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
        pairs: &[(f64, Vec<u8>)],
        flags: crate::command::zset::helpers::ZAddFlags,
    ) -> Result<RespValue> {
        self.ensure_zset_delta(storage, key)?;
        if flags.incr {
            let (incr_score, member) = &pairs[0];
            let current = self.current_member_score(storage, key, member)?;
            if current.is_none() && flags.xx {
                return Ok(RespValue::Null);
            }
            if current.is_some() && flags.nx {
                return Ok(RespValue::Null);
            }
            let old_score = current.unwrap_or(0.0);
            let new_score = old_score + incr_score;
            if new_score.is_nan() {
                return Err(crate::error::Error::Command(
                    "ERR resulting score is not a number (NaN)".into(),
                ));
            }
            if let Some(old) = current {
                if flags.gt && new_score <= old {
                    return Ok(RespValue::Null);
                }
                if flags.lt && new_score >= old {
                    return Ok(RespValue::Null);
                }
            }
            self.record_member(key, member, current, Some(new_score));
            return Ok(router::bulk(
                crate::command::zset::helpers::format_score(new_score).into_bytes(),
            ));
        }

        let mut added = 0i64;
        let mut changed = 0i64;
        for (score, member) in pairs {
            let current = self.current_member_score(storage, key, member)?;
            match current {
                None => {
                    if flags.xx {
                        continue;
                    }
                    added += 1;
                    changed += 1;
                    self.record_member(key, member, None, Some(*score));
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
                    self.record_member(key, member, Some(old_score), Some(*score));
                }
            }
        }
        if flags.ch {
            Ok(router::integer(changed))
        } else {
            Ok(router::integer(added))
        }
    }

    pub(crate) fn subkey_zrem(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        args: &[Bytes],
    ) -> Result<RespValue> {
        let key = &args[0];
        self.ensure_zset_delta(storage, key)?;
        let mut removed = 0i64;
        for member in &args[1..] {
            let current = self.current_member_score(storage, key, member.as_ref())?;
            if current.is_none() {
                continue;
            }
            removed += 1;
            self.record_member(key, member.as_ref(), current, None);
        }
        Ok(router::integer(removed))
    }

    pub(crate) fn subkey_zrem_single(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
        member: &[u8],
    ) -> Result<i64> {
        self.ensure_zset_delta(storage, key)?;
        let current = self.current_member_score(storage, key, member)?;
        if current.is_none() {
            return Ok(0);
        }
        self.record_member(key, member, current, None);
        Ok(1)
    }

    pub(crate) fn materialize_zset_delta(
        &self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
    ) -> Result<BTreeMap<Vec<u8>, f64>> {
        if let Some(view) = self.zset_views.lock().get(key) {
            return Ok(view.clone());
        }
        let mut map = self.visible_subkey_members(storage, key)?;
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
        self.zset_views.lock().insert(key.to_vec(), map.clone());
        Ok(map)
    }

    pub(crate) fn append_zset_deltas(
        &self,
        storage: &dyn crate::storage::ScriptIo,
        ops: &mut Vec<(Vec<u8>, WriteOp)>,
    ) -> Result<()> {
        for (key, delta) in &self.zset_deltas {
            let expires_at = self.resolve_expires_at(storage, key)?;
            push_delta(self.db_index, key, delta, expires_at, ops);
            if delta.count == 0 {
                zset_subkey::invalidate_score_bounds_hint(self.db_index, key);
            }
        }
        Ok(())
    }

    pub(crate) fn visible_subkey_members(
        &self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
    ) -> Result<BTreeMap<Vec<u8>, f64>> {
        let _pin = self.in_flight.as_ref().map(|buf| buf.pin_reads());
        let mut loaded = zset_subkey::load_members_sync(storage, self.db_index, key)?;
        if let Some(buffer) = &self.in_flight {
            overlay_inflight_members(&mut loaded, buffer, self.db_index, key);
        }
        Ok(loaded)
    }

    fn ensure_zset_delta(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
    ) -> Result<()> {
        if self.zset_deltas.contains_key(key) {
            return Ok(());
        }
        let stored = self.get_value(storage, key)?;
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

    pub(crate) fn point_member_score(
        &self,
        storage: &dyn crate::storage::ScriptIo,
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
        if let Some(view) = self.zset_views.lock().get(key) {
            return Ok(view.get(member).copied());
        }
        point_score(self.in_flight.as_ref(), storage, self.db_index, key, member)
    }

    fn current_member_score(
        &self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
        member: &[u8],
    ) -> Result<Option<f64>> {
        self.point_member_score(storage, key, member)
    }

    fn record_member(
        &mut self,
        key: &[u8],
        member: &[u8],
        current: Option<f64>,
        after: Option<f64>,
    ) {
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
        if let Some(view) = self.zset_views.lock().get_mut(key) {
            match after {
                Some(score) => {
                    view.insert(member.to_vec(), score);
                }
                None => {
                    view.remove(member);
                }
            }
        }
    }

    pub(crate) fn subkey_zincrby(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
        increment: f64,
        member: &[u8],
    ) -> Result<RespValue> {
        self.ensure_zset_delta(storage, key)?;
        let current = self.current_member_score(storage, key, member)?;
        let new_score = current.unwrap_or(0.0) + increment;
        if !new_score.is_finite() {
            return Err(crate::error::Error::Command(
                "ERR increment would produce NaN or Infinity".into(),
            ));
        }
        self.record_member(key, member, current, Some(new_score));
        Ok(router::bulk(
            crate::command::zset::helpers::format_score(new_score).into_bytes(),
        ))
    }

    pub(crate) fn subkey_zpop(
        &mut self,
        storage: &dyn crate::storage::ScriptIo,
        key: &[u8],
        count: usize,
        max: bool,
    ) -> Result<RespValue> {
        self.ensure_zset_delta(storage, key)?;
        let map = self.materialize_zset_delta(storage, key)?;
        let sorted = crate::command::zset::helpers::sorted_by_score(&map, max);
        let n = count.min(sorted.len());
        let mut items = Vec::with_capacity(n * 2);
        for (member, score) in sorted.into_iter().take(n) {
            items.push(router::bulk(member.clone()));
            items.push(router::bulk(
                crate::command::zset::helpers::format_score(score).into_bytes(),
            ));
            self.record_member(key, &member, Some(score), None);
        }
        Ok(RespValue::Array(Some(items)))
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
            WriteOp::put_stored(zset_subkey::header(delta.count, expires_at)),
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

fn point_score(
    in_flight: Option<&Arc<InFlightBuffer>>,
    storage: &dyn crate::storage::ScriptIo,
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
    zset_subkey::read_member_score_sync(storage, db, key, member)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use tempfile::TempDir;

    use crate::command::script::transaction::ScriptTransaction;
    use crate::command::CommandRouter;
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
        ops.push((added, WriteOp::put_engine(zset_subkey::encode_score(7.0))));
        ops.push((b"z".to_vec(), WriteOp::put_stored(header(64, None))));
        let inflight = InFlightBuffer::new();
        inflight.insert_batch(0, &ops);
        let txn = ScriptTransaction::with_in_flight(0, Some(Arc::new(inflight)));
        let (removed_visible, added_visible) = txn
            .with_zset(storage.script_io(), b"z", |zset| {
                Ok((
                    zset.contains_key(b"m00".as_ref()),
                    zset.get(b"m99".as_ref()).copied() == Some(7.0),
                ))
            })
            .unwrap()
            .unwrap();
        assert!(
            !removed_visible,
            "in-flight ZREM still visible to the next range read"
        );
        assert!(
            added_visible,
            "in-flight member put is missing from the range read"
        );
    }

    #[tokio::test]
    async fn script_zscore_reads_one_member_without_scanning_the_segment() {
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
        zset_subkey::THREAD_LOAD_CALLS.with(|c| c.set(0));
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
        let scans = zset_subkey::THREAD_LOAD_CALLS.with(|c| c.get());
        assert_eq!(scans, 0, "ZSCORE scanned the member prefix {scans} times");
    }

    #[tokio::test]
    async fn visible_subkey_members_overlays_inflight_without_cross_txn_leak() {
        let dir = TempDir::new().unwrap();
        let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
        let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
        let router = CommandRouter::new(Arc::clone(&storage));
        let mut db = 0;
        for i in 0..70 {
            router
                .execute(
                    "ZADD",
                    &[
                        Bytes::from_static(b"kz"),
                        Bytes::from(format!("{}", i * 10)),
                        Bytes::from(format!("m{i}")),
                    ],
                    &mut db,
                )
                .await
                .unwrap();
        }
        let mut ops = zset_subkey::member_change_ops(0, b"kz", b"m0", Some(0.0), None);
        let added = zset_subkey::member_key(&AiDbEngine::encode_key(0, b"kz"), b"m99");
        ops.push((added, WriteOp::put_engine(zset_subkey::encode_score(99.0))));
        ops.push((b"kz".to_vec(), WriteOp::put_stored(header(70, None))));
        let inflight = Arc::new(InFlightBuffer::new());
        inflight.insert_batch(0, &ops);

        let txn1 = ScriptTransaction::with_in_flight(0, Some(Arc::clone(&inflight)));
        let members1 = txn1
            .visible_subkey_members(storage.script_io(), b"kz")
            .unwrap();
        assert!(
            !members1.contains_key(b"m0".as_ref()),
            "m0 should be deleted by in_flight"
        );
        assert_eq!(
            members1.get(b"m99".as_ref()).copied(),
            Some(99.0),
            "m99 should be visible"
        );
    }

    #[tokio::test]
    async fn revrange_top_with_interleaved_zadd_does_not_load_all_members() {
        use crate::protocol::RespValue;

        let dir = TempDir::new().unwrap();
        let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
        let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
        let router = CommandRouter::new(Arc::clone(&storage));
        let mut db = 0;
        for i in 0..70 {
            router
                .execute(
                    "ZADD",
                    &[
                        Bytes::from_static(b"ztop"),
                        Bytes::from(format!("{}", i + 1)),
                        Bytes::from(format!("m{i:02}")),
                    ],
                    &mut db,
                )
                .await
                .unwrap();
        }
        zset_subkey::clear_score_bounds_hint();
        zset_subkey::THREAD_LOAD_CALLS.with(|c| c.set(0));
        let script = Bytes::from(
            "local top1 = redis.call('ZREVRANGE', KEYS[1], 0, 0, 'WITHSCORES')\n\
             redis.call('ZADD', KEYS[1], 500, 'm00')\n\
             local top2 = redis.call('ZREVRANGE', KEYS[1], 0, 0, 'WITHSCORES')\n\
             redis.call('ZADD', KEYS[1], 600, 'm01')\n\
             local top3 = redis.call('ZREVRANGE', KEYS[1], 0, 0, 'WITHSCORES')\n\
             return {top1[1], top1[2], top2[1], top2[2], top3[1], top3[2]}\n",
        );
        let resp = router
            .execute(
                "EVAL",
                &[
                    script,
                    Bytes::from_static(b"1"),
                    Bytes::from_static(b"ztop"),
                ],
                &mut db,
            )
            .await
            .unwrap();
        let RespValue::Array(Some(items)) = resp else {
            panic!("expected array");
        };
        assert_eq!(
            items[0],
            RespValue::BulkString(Some(Bytes::from_static(b"m69")))
        );
        assert_eq!(
            items[1],
            RespValue::BulkString(Some(Bytes::from_static(b"70")))
        );
        assert_eq!(
            items[2],
            RespValue::BulkString(Some(Bytes::from_static(b"m00")))
        );
        assert_eq!(
            items[3],
            RespValue::BulkString(Some(Bytes::from_static(b"500")))
        );
        assert_eq!(
            items[4],
            RespValue::BulkString(Some(Bytes::from_static(b"m01")))
        );
        assert_eq!(
            items[5],
            RespValue::BulkString(Some(Bytes::from_static(b"600")))
        );
        let scans = zset_subkey::THREAD_LOAD_CALLS.with(|c| c.get());
        assert_eq!(scans, 0, "ZREVRANGE 0 0 loaded full ZSet {scans} times");
    }
}
