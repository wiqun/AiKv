//! 脚本里大 ZSET 的有界范围读: 读 score 索引需要的条数, 再叠加在途覆盖层, 不展开成员全集.
//!
//! 在途覆盖层是前面脚本已经发布、但还没落盘的写. 它们对索引的影响是有限的几条 put / delete,
//! 所以存储侧多取 "区间内在途变更数" 条, 就能保证合并后的前 N 条是正确的.
//! 本脚本自己改过的 ZSET (`ZSetDelta`) 和小集合不走这里, 返回 `None` 由调用方回退全量路径.

use std::collections::HashMap;

use crate::command::script::transaction::ScriptTransaction;
use crate::command::zset::range_scan::{
    decode_page, score_scan_bounds, IndexPlan, ScoreWindow, ScoredMembers,
};
use crate::error::Result;
use crate::storage::zset_subkey::{self, is_zset_header, score_index_entry, score_prefix};
use crate::storage::{AiDbEngine, ScriptIo};

/// 本脚本没改过且已是 header 形式时返回成员数.
fn header_len(storage: &dyn ScriptIo, txn: &ScriptTransaction, key: &[u8]) -> Result<Option<u32>> {
    if txn.zset_delta_count(key).is_some() {
        return Ok(None);
    }
    Ok(txn
        .get_value(storage, key)?
        .as_ref()
        .and_then(is_zset_header))
}

type InflightScanInfo = (HashMap<Vec<u8>, bool>, Option<Vec<u8>>, Option<Vec<u8>>);

fn collect_inflight_updates(
    txn: &ScriptTransaction,
    prefix: &[u8],
    start: &[u8],
    end: &[u8],
) -> InflightScanInfo {
    let raw_updates = txn
        .in_flight
        .as_ref()
        .map(|buf| buf.prefix_raw_updates(prefix))
        .unwrap_or_default();
    let mut updates = HashMap::new();
    let mut min_put: Option<Vec<u8>> = None;
    let mut max_put: Option<Vec<u8>> = None;
    for (key, raw) in raw_updates {
        let is_put = raw.is_some();
        if is_put {
            if min_put.as_ref().is_none_or(|m| key.as_slice() < m.as_slice()) {
                min_put = Some(key.clone());
            }
            if max_put.as_ref().is_none_or(|m| key.as_slice() > m.as_slice()) {
                max_put = Some(key.clone());
            }
        }
        if key.as_slice() >= start && key.as_slice() < end {
            updates.insert(key, is_put);
        }
    }
    (updates, min_put, max_put)
}

fn update_bounds_hint(
    prefix: &[u8],
    bounds: (&[u8], &[u8]),
    hints: (&Option<Vec<u8>>, &Option<Vec<u8>>),
    inflight_puts: (Option<&[u8]>, Option<&[u8]>),
    stored_first: Option<&[u8]>,
    scan_end: &[u8],
    reverse: bool,
) {
    if !reverse {
        let covers_bottom =
            bounds.0 <= prefix || hints.0.as_ref().is_some_and(|h| bounds.0 <= h.as_slice());
        if covers_bottom {
            let floor = stored_first.unwrap_or(scan_end);
            let new_min = match inflight_puts.0 {
                Some(inf) if inf < floor => inf,
                _ => floor,
            };
            zset_subkey::advance_min_score_hint(prefix, new_min.to_vec());
        }
        return;
    }
    let covers_top = AiDbEngine::prefix_end(prefix).is_some_and(|pe| bounds.1 >= pe.as_slice())
        || hints
            .1
            .as_deref()
            .and_then(AiDbEngine::prefix_end)
            .is_some_and(|he| bounds.1 >= he.as_slice());
    if covers_top {
        let new_max = match (stored_first, inflight_puts.1) {
            (Some(s), Some(i)) => Some(if s >= i { s } else { i }),
            (Some(s), None) => Some(s),
            (None, Some(i)) => Some(i),
            (None, None) => None,
        };
        if let Some(m) = new_max {
            zset_subkey::shrink_max_score_hint(prefix, m.to_vec());
        }
    }
}

/// 在途覆盖层叠加存储后, 区间 `[start, end)` 内按 `reverse` 方向的前 `fetch` 个索引 key.
/// `None`: 后端不支持有界扫描.
fn merged_keys(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    prefix: &[u8],
    bounds: (Vec<u8>, Vec<u8>),
    fetch: usize,
    reverse: bool,
) -> Option<Vec<Vec<u8>>> {
    let (start, end) = bounds;
    let _pin = txn.in_flight.as_ref().map(|buf| buf.pin_reads());
    let (min_hint, max_hint) = zset_subkey::get_score_bounds_hint(prefix);
    let scan_start = match &min_hint {
        Some(h) if h.as_slice() > start.as_slice() => h.clone(),
        _ => start.clone(),
    };
    let scan_end = match max_hint.as_deref().and_then(AiDbEngine::prefix_end) {
        Some(he) if he.as_slice() < end.as_slice() => he,
        _ => end.clone(),
    };
    let (updates, min_put, max_put) = collect_inflight_updates(txn, prefix, &start, &end);
    let limit = fetch.saturating_add(updates.len());
    let stored = if scan_start < scan_end {
        storage
            .raw_range_scan(scan_start, scan_end.clone(), limit, reverse)
            .ok()?
    } else {
        Vec::new()
    };
    if fetch > 0 {
        update_bounds_hint(
            prefix,
            (&start, &end),
            (&min_hint, &max_hint),
            (min_put.as_deref(), max_put.as_deref()),
            stored.first().map(|(k, _)| k.as_slice()),
            &scan_end,
            reverse,
        );
    }
    if updates.is_empty() {
        return Some(stored.into_iter().map(|(key, _)| key).collect());
    }
    let mut keys: Vec<Vec<u8>> = stored
        .into_iter()
        .map(|(key, _)| key)
        .filter(|key| !updates.contains_key(key))
        .collect();
    keys.extend(
        updates
            .into_iter()
            .filter(|(_, put)| *put)
            .map(|(key, _)| key),
    );
    keys.sort_unstable();
    if reverse {
        keys.reverse();
    }
    keys.truncate(fetch);
    Some(keys)
}

pub(super) fn top_score_key_without_delta(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    prefix: &[u8],
) -> Option<Vec<u8>> {
    let end = AiDbEngine::prefix_end(prefix)?;
    merged_keys(storage, txn, prefix, (prefix.to_vec(), end), 1, true)?
        .into_iter()
        .next()
}

/// 按 score 范围读 (`ZRANGEBYSCORE` / `ZREVRANGEBYSCORE`). `Ok(None)` 表示不适用.
pub(super) fn by_score(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    key: &[u8],
    window: &ScoreWindow,
) -> Result<Option<ScoredMembers>> {
    if header_len(storage, txn, key)?.is_none() {
        return Ok(None);
    }
    let prefix = score_prefix(&AiDbEngine::encode_key(txn.db_index, key));
    let Some(bounds) = score_scan_bounds(&prefix, &window.min, &window.max) else {
        return Ok(Some(Vec::new()));
    };
    let fetch = window.fetch_limit();
    let Some(keys) = merged_keys(storage, txn, &prefix, bounds, fetch, window.reverse) else {
        return Ok(None);
    };
    let pairs = keys.into_iter().map(|key| (key, Vec::new())).collect();
    Ok(Some(decode_page(
        &prefix,
        pairs,
        window.offset,
        window.count,
    )))
}

/// 按序号读 (`ZRANGE` / `ZREVRANGE`). `Ok(None)` 表示不适用.
pub(super) fn by_index(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    key: &[u8],
    start: i64,
    stop: i64,
    reverse: bool,
) -> Result<Option<ScoredMembers>> {
    let Some(len) = header_len(storage, txn, key)? else {
        return Ok(None);
    };
    let Some(plan) = IndexPlan::new(len as usize, start, stop, reverse) else {
        return Ok(Some(Vec::new()));
    };
    let prefix = score_prefix(&AiDbEngine::encode_key(txn.db_index, key));
    let end = AiDbEngine::prefix_end(&prefix).expect("score prefix ends with a tag byte");
    let bounds = (prefix.clone(), end);
    let Some(keys) = merged_keys(storage, txn, &prefix, bounds, plan.fetch, !plan.from_head) else {
        return Ok(None);
    };
    let entries = keys
        .iter()
        .filter_map(|key| score_index_entry(&prefix, key))
        .collect();
    Ok(Some(plan.apply(entries)))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use bytes::Bytes;
    use tempfile::TempDir;

    use super::*;
    use crate::command::script::in_flight::InFlightBuffer;
    use crate::command::script::transaction::ScriptTransaction;
    use crate::command::zset::helpers::{
        apply_limit, normalize_range, score_in_range, sorted_by_score, ScoreBound,
    };
    use crate::command::zset::range_scan::ScoreWindow;
    use crate::command::CommandRouter;
    use crate::storage::zset_subkey::{self, header, THREAD_LOAD_CALLS};
    use crate::storage::{AiDbEngine, KvStorage, KvStorageAdapter, WriteOp};

    const N: usize = 120;
    /// 独占 key: 成员缓存和加载计数是全局的, 与其他 ZSet 测试共用 key 会互相污染.
    const KEY: &[u8] = b"zscan";

    fn score_of(i: usize) -> f64 {
        ((i * 7) % 50) as f64 - 5.0 + if i.is_multiple_of(4) { 0.25 } else { 0.0 }
    }

    async fn seeded(dir: &TempDir) -> Arc<dyn KvStorage> {
        let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
        let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
        let router = CommandRouter::new(Arc::clone(&storage));
        let mut db = 0;
        for i in 0..N {
            let args = [
                Bytes::from_static(KEY),
                Bytes::from(score_of(i).to_string()),
                Bytes::from(format!("m{i:03}")),
            ];
            router.execute("ZADD", &args, &mut db).await.unwrap();
        }
        storage
    }

    /// 一个脚本还没落盘的写: 删 m000, 把 m001 改成高分, 新增 m999, header 数量不变.
    fn in_flight_edits() -> Arc<InFlightBuffer> {
        let mut ops = zset_subkey::member_change_ops(0, KEY, b"m000", Some(score_of(0)), None);
        ops.extend(zset_subkey::member_change_ops(
            0,
            KEY,
            b"m001",
            Some(score_of(1)),
            Some(1000.0),
        ));
        ops.extend(zset_subkey::member_change_ops(
            0,
            KEY,
            b"m999",
            None,
            Some(20.0),
        ));
        ops.push((KEY.to_vec(), WriteOp::put_stored(header(N as u32, None))));
        let buffer = InFlightBuffer::new();
        buffer.insert_batch(0, &ops);
        Arc::new(buffer)
    }

    fn bound(value: f64, inclusive: bool) -> ScoreBound {
        ScoreBound { value, inclusive }
    }

    fn windows() -> Vec<ScoreWindow> {
        let edges = [
            f64::NEG_INFINITY,
            -5.0,
            0.0,
            10.0,
            20.0,
            44.25,
            1000.0,
            f64::INFINITY,
        ];
        let mut out = Vec::new();
        for &lo in &edges {
            for &hi in &edges {
                for (li, hi_in) in [(true, true), (false, true), (true, false)] {
                    for reverse in [false, true] {
                        for (offset, count) in
                            [(0, None), (0, Some(1)), (3, Some(5)), (40, Some(7))]
                        {
                            out.push(ScoreWindow {
                                min: bound(lo, li),
                                max: bound(hi, hi_in),
                                reverse,
                                offset,
                                count,
                            });
                        }
                    }
                }
            }
        }
        out
    }

    /// 种子数据, 以及 `in_flight_edits` 叠加后的可见成员. 手工推导, 不走会加载全部成员的旧路径.
    fn expected_members(edited: bool) -> BTreeMap<Vec<u8>, f64> {
        let mut map: BTreeMap<Vec<u8>, f64> = (0..N)
            .map(|i| (format!("m{i:03}").into_bytes(), score_of(i)))
            .collect();
        if edited {
            map.remove(b"m000".as_ref());
            map.insert(b"m001".to_vec(), 1000.0);
            map.insert(b"m999".to_vec(), 20.0);
        }
        map
    }

    fn check_equivalence(
        in_flight: Option<Arc<InFlightBuffer>>,
        storage: &Arc<dyn KvStorage>,
        edited: bool,
    ) {
        let io = storage.script_io();
        let txn = ScriptTransaction::with_in_flight(0, in_flight);
        let map = expected_members(edited);

        let loads_before = THREAD_LOAD_CALLS.with(|c| c.get());
        for w in windows() {
            let mut expected: Vec<_> = sorted_by_score(&map, w.reverse)
                .into_iter()
                .filter(|(_, s)| score_in_range(*s, &w.min, &w.max))
                .collect();
            apply_limit(&mut expected, w.offset, w.count);
            let got = by_score(io, &txn, KEY, &w)
                .unwrap()
                .expect("header zset uses the scan path");
            assert_eq!(got, expected, "window {w:?}");
        }
        for reverse in [false, true] {
            for (start, stop) in [
                (0, 0),
                (0, 9),
                (-5, -1),
                (100, 130),
                (-200, 3),
                (50, 49),
                (0, -1),
            ] {
                let sorted = sorted_by_score(&map, reverse);
                let (s, e) = normalize_range(sorted.len(), start, stop);
                let expected: Vec<_> = if s > e {
                    Vec::new()
                } else {
                    sorted[s..=e].to_vec()
                };
                let got = by_index(io, &txn, KEY, start, stop, reverse)
                    .unwrap()
                    .expect("header zset uses the scan path");
                assert_eq!(got, expected, "index {start}..{stop} reverse={reverse}");
            }
        }
        assert_eq!(
            THREAD_LOAD_CALLS.with(|c| c.get()) - loads_before,
            0,
            "有界读不应该加载全部成员"
        );
        assert!(
            zset_subkey::get_cached_members(0, KEY).is_none(),
            "有界读不应该填充成员缓存"
        );
    }

    #[tokio::test]
    async fn scan_matches_full_view_without_in_flight() {
        let dir = TempDir::new().unwrap();
        let storage = seeded(&dir).await;
        check_equivalence(None, &storage, false);
    }

    #[tokio::test]
    async fn scan_applies_in_flight_deletes_puts_and_score_changes() {
        let dir = TempDir::new().unwrap();
        let storage = seeded(&dir).await;
        check_equivalence(Some(in_flight_edits()), &storage, true);
    }

    #[tokio::test]
    async fn scan_falls_back_after_the_script_edited_the_zset() {
        let dir = TempDir::new().unwrap();
        let storage = seeded(&dir).await;
        let io = storage.script_io();
        let mut txn = ScriptTransaction::with_in_flight(0, None);
        txn.subkey_zadd(
            io,
            &[
                Bytes::from_static(KEY),
                Bytes::from_static(b"5"),
                Bytes::from_static(b"fresh"),
            ],
        )
        .unwrap_or_else(|e| panic!("zadd: {e}"));
        let window = ScoreWindow {
            min: bound(f64::NEG_INFINITY, true),
            max: bound(f64::INFINITY, true),
            reverse: false,
            offset: 0,
            count: Some(3),
        };
        assert!(by_score(io, &txn, KEY, &window).unwrap().is_none());
        assert!(by_index(io, &txn, KEY, 0, 2, false).unwrap().is_none());
    }

    #[tokio::test]
    async fn scan_skips_inline_zsets() {
        let dir = TempDir::new().unwrap();
        let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
        let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
        let router = CommandRouter::new(Arc::clone(&storage));
        let mut db = 0;
        let args = [
            Bytes::from_static(KEY),
            Bytes::from_static(b"1"),
            Bytes::from_static(b"a"),
        ];
        router.execute("ZADD", &args, &mut db).await.unwrap();
        let txn = ScriptTransaction::with_in_flight(0, None);
        let window = ScoreWindow {
            min: bound(f64::NEG_INFINITY, true),
            max: bound(f64::INFINITY, true),
            reverse: false,
            offset: 0,
            count: None,
        };
        assert!(by_score(storage.script_io(), &txn, KEY, &window)
            .unwrap()
            .is_none());
    }

    fn lua_call(parts: &[mlua::Value]) -> mlua::MultiValue {
        parts.iter().cloned().collect()
    }

    #[tokio::test]
    async fn fast_zrangebyscore_returns_the_scan_page_as_a_lua_table() {
        let dir = TempDir::new().unwrap();
        let storage = seeded(&dir).await;
        let io = storage.script_io();
        let lua = mlua::Lua::new();
        let mut txn = ScriptTransaction::with_in_flight(0, None);
        let declared_keys = [Bytes::from_static(KEY)];
        let declared = super::super::DeclaredKeys::new(&declared_keys);
        let args = lua_call(&[
            mlua::Value::String(lua.create_string("ZRANGEBYSCORE").unwrap()),
            mlua::Value::String(lua.create_string(KEY).unwrap()),
            mlua::Value::Integer(0),
            mlua::Value::String(lua.create_string("+inf").unwrap()),
            mlua::Value::String(lua.create_string("LIMIT").unwrap()),
            mlua::Value::Integer(0),
            mlua::Value::Integer(3),
        ]);
        let loads = THREAD_LOAD_CALLS.with(|c| c.get());
        let value = super::super::try_fast_path(&lua, io, &mut txn, &declared, &args, true, false)
            .unwrap()
            .expect("header zset is handled");
        let mlua::Value::Table(table) = value else {
            panic!("expected a lua table");
        };
        let mut got = Vec::new();
        for i in 1..=3 {
            let member: mlua::String = table.raw_get(i).unwrap();
            got.push(member.as_bytes().to_vec());
        }
        let window = ScoreWindow {
            min: bound(0.0, true),
            max: bound(f64::INFINITY, true),
            reverse: false,
            offset: 0,
            count: Some(3),
        };
        let expected: Vec<_> = by_score(io, &txn, KEY, &window)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|(member, _)| member)
            .collect();
        assert_eq!(got, expected);
        assert_eq!(THREAD_LOAD_CALLS.with(|c| c.get()), loads);
    }

    #[tokio::test]
    async fn fast_zrangebyscore_leaves_withscores_and_edited_sets() {
        let dir = TempDir::new().unwrap();
        let storage = seeded(&dir).await;
        let io = storage.script_io();
        let lua = mlua::Lua::new();
        let mut txn = ScriptTransaction::with_in_flight(0, None);
        let declared_keys = [Bytes::from_static(KEY)];
        let declared = super::super::DeclaredKeys::new(&declared_keys);
        let withscores = lua_call(&[
            mlua::Value::String(lua.create_string("ZRANGEBYSCORE").unwrap()),
            mlua::Value::String(lua.create_string(KEY).unwrap()),
            mlua::Value::String(lua.create_string("-inf").unwrap()),
            mlua::Value::String(lua.create_string("+inf").unwrap()),
            mlua::Value::String(lua.create_string("WITHSCORES").unwrap()),
        ]);
        assert!(
            super::super::try_fast_path(&lua, io, &mut txn, &declared, &withscores, true, false)
                .unwrap()
                .is_none()
        );
        txn.subkey_zadd(
            io,
            &[
                Bytes::from_static(KEY),
                Bytes::from_static(b"5"),
                Bytes::from_static(b"fresh"),
            ],
        )
        .unwrap();
        let plain = lua_call(&[
            mlua::Value::String(lua.create_string("ZRANGEBYSCORE").unwrap()),
            mlua::Value::String(lua.create_string(KEY).unwrap()),
            mlua::Value::String(lua.create_string("-inf").unwrap()),
            mlua::Value::String(lua.create_string("+inf").unwrap()),
            mlua::Value::String(lua.create_string("LIMIT").unwrap()),
            mlua::Value::String(lua.create_string("0").unwrap()),
            mlua::Value::String(lua.create_string("3").unwrap()),
        ]);
        assert!(
            super::super::try_fast_path(&lua, io, &mut txn, &declared, &plain, true, false)
                .unwrap()
                .is_none()
        );
    }
}
