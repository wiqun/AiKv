//! ZSET 子键布局 (2a) 测试: ZADD/ZREM/ZSCORE/ZCARD, 内联 ↔ 子键迁移与重启, 基于真实 AiDb.

use std::path::Path;
use std::sync::Arc;

use aikv::command::CommandRouter;
use aikv::protocol::RespValue;
use aikv::storage::subkey::{decode_zset_score, zset_score_sub, SubTag};
use aikv::storage::{
    is_wrongtype, server_db_options, AiDbEngine, CollectionKind, KvStorage, KvStorageAdapter,
    StoredValue, ValueType,
};
use bytes::Bytes;
use tempfile::TempDir;

use super::helpers::{assert_int, b};

#[derive(Debug, PartialEq, Eq)]
enum Layout {
    Missing,
    Inline,
    Subkey { count: u64 },
}

async fn open_at(path: &Path, sync_wal: bool) -> (CommandRouter, Arc<dyn KvStorage>) {
    let engine = AiDbEngine::open_with_options(path, server_db_options(sync_wal)).unwrap();
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
    (CommandRouter::new(Arc::clone(&storage)), storage)
}

async fn layout_of(storage: &Arc<dyn KvStorage>, key: &str) -> Layout {
    match storage.get_typed(0, key.as_bytes()).await.unwrap() {
        None => Layout::Missing,
        Some(StoredValue {
            value: ValueType::ZSet(_),
            ..
        }) => Layout::Inline,
        Some(StoredValue {
            value:
                ValueType::CollectionHeader {
                    kind: CollectionKind::ZSet,
                    count,
                    ..
                },
            ..
        }) => Layout::Subkey { count },
        Some(other) => panic!("unexpected stored value: {other:?}"),
    }
}

/// 子键布局下检查: Z 行数 = header.count = R 行数, 且每个 Z 都有对应的分数索引 R.
async fn assert_index_consistent(storage: &Arc<dyn KvStorage>, key: &str) {
    let (count, version) = match storage.get_typed(0, key.as_bytes()).await.unwrap() {
        Some(StoredValue {
            value: ValueType::CollectionHeader { count, version, .. },
            ..
        }) => (count, version),
        other => panic!("expected subkey layout, got {other:?}"),
    };
    let zs = storage
        .subkey_scan(0, key.as_bytes(), version, SubTag::ZMember)
        .await
        .unwrap();
    let rs = storage
        .subkey_scan(0, key.as_bytes(), version, SubTag::ZScore)
        .await
        .unwrap();
    assert_eq!(zs.len() as u64, count, "Z 行数必须等于 header.count");
    assert_eq!(rs.len(), zs.len(), "R 行数必须等于 Z 行数");
    for (member, raw) in zs {
        let score = decode_zset_score(&raw).expect("score decodes");
        let expected = zset_score_sub(score, &member);
        assert!(
            rs.iter().any(|(sub, _)| *sub == expected),
            "member {member:?} 缺少分数索引"
        );
    }
}

fn bulk(resp: RespValue) -> Vec<u8> {
    match resp {
        RespValue::BulkString(Some(bytes)) => bytes.to_vec(),
        other => panic!("expected bulk string, got {other:?}"),
    }
}

fn array_len(resp: &RespValue) -> usize {
    match resp {
        RespValue::Array(Some(items)) => items.len(),
        other => panic!("expected array, got {other:?}"),
    }
}

fn args(parts: &[&str]) -> Vec<Bytes> {
    parts.iter().map(|p| b(p)).collect()
}

/// 构造 `ZADD key score0 m0 score1 m1 ...`, 成员为 `m{start}..m{end}`.
fn zadd_args(key: &str, start: usize, end: usize, score_offset: f64) -> Vec<Bytes> {
    let mut out = vec![b(key)];
    for i in start..end {
        out.push(b(&format!("{}", i as f64 + score_offset)));
        out.push(b(&format!("m{i:03}")));
    }
    out
}

async fn exec(router: &CommandRouter, cmd: &str, argv: &[Bytes]) -> RespValue {
    let mut db = 0usize;
    router.execute(cmd, argv, &mut db).await.unwrap()
}

/// 阈值: 64 成员保持内联, 第 65 个成员转为子键布局.
#[tokio::test]
async fn zadd_promotes_to_subkey_above_64_members() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = open_at(dir.path(), false).await;
    exec(&router, "ZADD", &zadd_args("z", 0, 64, 0.0)).await;
    assert_eq!(layout_of(&storage, "z").await, Layout::Inline);

    exec(&router, "ZADD", &zadd_args("z", 64, 65, 0.0)).await;
    assert_eq!(layout_of(&storage, "z").await, Layout::Subkey { count: 65 });
    assert_index_consistent(&storage, "z").await;

    assert_int(exec(&router, "ZCARD", &args(&["z"])).await, 65);
    let all = exec(&router, "ZRANGE", &args(&["z", "0", "-1", "WITHSCORES"])).await;
    assert_eq!(array_len(&all), 130, "非 2a 命令必须看到完整集合");
}

/// 同名成员更新分数不增加计数; 新成员计数; 同一命令内重复成员以后者为准.
#[tokio::test]
async fn zadd_counts_only_new_members_and_keeps_index() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = open_at(dir.path(), false).await;
    exec(&router, "ZADD", &zadd_args("z", 0, 70, 0.0)).await;

    assert_int(exec(&router, "ZADD", &args(&["z", "500", "m001"])).await, 0);
    assert_eq!(
        bulk(exec(&router, "ZSCORE", &args(&["z", "m001"])).await),
        b"500".to_vec()
    );
    assert_int(exec(&router, "ZADD", &args(&["z", "1", "new"])).await, 1);
    assert_int(
        exec(&router, "ZADD", &args(&["z", "2", "dup", "3", "dup"])).await,
        1,
    );
    assert_eq!(
        bulk(exec(&router, "ZSCORE", &args(&["z", "dup"])).await),
        b"3".to_vec()
    );
    assert_int(exec(&router, "ZADD", &args(&["z", "500", "m001"])).await, 0);
    assert_eq!(layout_of(&storage, "z").await, Layout::Subkey { count: 72 });
    assert_index_consistent(&storage, "z").await;
}

/// 子键布局下 ZSCORE 对不存在的成员返回 nil; ZCARD 与 header 计数一致.
#[tokio::test]
async fn zscore_and_zcard_on_subkey_layout() {
    let dir = TempDir::new().unwrap();
    let (router, _storage) = open_at(dir.path(), false).await;
    exec(&router, "ZADD", &zadd_args("z", 0, 100, 0.5)).await;
    assert_int(exec(&router, "ZCARD", &args(&["z"])).await, 100);
    assert_eq!(
        bulk(exec(&router, "ZSCORE", &args(&["z", "m042"])).await),
        b"42.5".to_vec()
    );
    assert_eq!(
        exec(&router, "ZSCORE", &args(&["z", "missing"])).await,
        RespValue::BulkString(None)
    );
    assert_int(exec(&router, "ZCARD", &args(&["nokey"])).await, 0);
}

/// ZREM 删除成员并维持索引; 剩余 33 仍为子键, 剩余 32 回落为内联 (迟滞, 避免边界抖动).
#[tokio::test]
async fn zrem_demotes_to_inline_only_at_32_members() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = open_at(dir.path(), false).await;
    exec(&router, "ZADD", &zadd_args("z", 0, 70, 0.0)).await;

    let first: Vec<Bytes> = std::iter::once(b("z"))
        .chain((0..37).map(|i| b(&format!("m{i:03}"))))
        .collect();
    assert_int(exec(&router, "ZREM", &first).await, 37);
    assert_eq!(layout_of(&storage, "z").await, Layout::Subkey { count: 33 });
    assert_index_consistent(&storage, "z").await;

    assert_int(exec(&router, "ZREM", &args(&["z", "m037"])).await, 1);
    assert_eq!(layout_of(&storage, "z").await, Layout::Inline);
    assert_int(exec(&router, "ZCARD", &args(&["z"])).await, 32);
    assert_eq!(
        bulk(exec(&router, "ZSCORE", &args(&["z", "m050"])).await),
        b"50".to_vec()
    );
    let members = exec(&router, "ZRANGE", &args(&["z", "0", "-1"])).await;
    assert_eq!(array_len(&members), 32);
}

/// 子键化集合删空后整键消失, 且 EXISTS 为 0.
#[tokio::test]
async fn zrem_all_members_deletes_key() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = open_at(dir.path(), false).await;
    exec(&router, "ZADD", &zadd_args("z", 0, 70, 0.0)).await;
    let all: Vec<Bytes> = std::iter::once(b("z"))
        .chain((0..70).map(|i| b(&format!("m{i:03}"))))
        .collect();
    assert_int(exec(&router, "ZREM", &all).await, 70);
    assert_eq!(layout_of(&storage, "z").await, Layout::Missing);
    assert_int(exec(&router, "EXISTS", &args(&["z"])).await, 0);
}

/// 重启后 (drop 引擎再打开) 子键布局的成员、分数与计数均可读.
#[tokio::test]
async fn subkey_zset_survives_restart() {
    let dir = TempDir::new().unwrap();
    {
        let (router, storage) = open_at(dir.path(), false).await;
        exec(&router, "ZADD", &zadd_args("z", 0, 100, 0.0)).await;
        exec(&router, "ZADD", &args(&["z", "777", "m010"])).await;
        storage.flush_engine().await.unwrap();
        storage.close_engine().await.unwrap();
    }
    let (router, storage) = open_at(dir.path(), false).await;
    assert_eq!(
        layout_of(&storage, "z").await,
        Layout::Subkey { count: 100 }
    );
    assert_int(exec(&router, "ZCARD", &args(&["z"])).await, 100);
    assert_eq!(
        bulk(exec(&router, "ZSCORE", &args(&["z", "m010"])).await),
        b"777".to_vec()
    );
    assert_index_consistent(&storage, "z").await;
}

/// 非 2a 命令 (ZINCRBY/ZPOPMIN/ZRANGEBYSCORE) 作用于子键 key 时看到完整集合, 写回后布局与索引一致.
#[tokio::test]
async fn non_2a_commands_see_full_subkey_zset() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = open_at(dir.path(), false).await;
    exec(&router, "ZADD", &zadd_args("z", 0, 100, 0.0)).await;

    assert_eq!(
        bulk(exec(&router, "ZINCRBY", &args(&["z", "1000", "m005"])).await),
        b"1005".to_vec()
    );
    let popped = exec(&router, "ZPOPMIN", &args(&["z", "3"])).await;
    assert_eq!(array_len(&popped), 6);
    assert_int(exec(&router, "ZCARD", &args(&["z"])).await, 97);
    assert_eq!(
        exec(&router, "ZSCORE", &args(&["z", "m000"])).await,
        RespValue::BulkString(None)
    );
    let range = exec(
        &router,
        "ZRANGEBYSCORE",
        &args(&["z", "50", "+inf", "LIMIT", "0", "2"]),
    )
    .await;
    assert_eq!(array_len(&range), 2);
    assert_index_consistent(&storage, "z").await;
}

/// RENAME 子键化的 ZSET 必须复制全部 Z/R 子键, 目标 key 的 ZCARD/ZSCORE 完整.
#[tokio::test]
async fn rename_keeps_subkey_zset_complete() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = open_at(dir.path(), false).await;
    exec(&router, "ZADD", &zadd_args("src", 0, 100, 0.0)).await;
    exec(&router, "RENAME", &args(&["src", "dst"])).await;

    assert_eq!(layout_of(&storage, "src").await, Layout::Missing);
    assert_eq!(
        layout_of(&storage, "dst").await,
        Layout::Subkey { count: 100 }
    );
    assert_int(exec(&router, "ZCARD", &args(&["dst"])).await, 100);
    assert_eq!(
        bulk(exec(&router, "ZSCORE", &args(&["dst", "m099"])).await),
        b"99".to_vec()
    );
    assert_index_consistent(&storage, "dst").await;
}

/// 子键写入保留 header 上的 TTL; 删除成员后 TTL 仍在.
#[tokio::test]
async fn ttl_survives_subkey_writes() {
    let dir = TempDir::new().unwrap();
    let (router, _storage) = open_at(dir.path(), false).await;
    exec(&router, "ZADD", &zadd_args("z", 0, 100, 0.0)).await;
    assert_int(exec(&router, "EXPIRE", &args(&["z", "1000"])).await, 1);
    exec(&router, "ZADD", &args(&["z", "1", "fresh"])).await;
    exec(&router, "ZREM", &args(&["z", "m001"])).await;
    let ttl = match exec(&router, "TTL", &args(&["z"])).await {
        RespValue::Integer(v) => v,
        other => panic!("expected integer, got {other:?}"),
    };
    assert!(ttl > 0 && ttl <= 1000, "ttl = {ttl}");
}

/// 类型不符仍返回 WRONGTYPE, 不得因 header 形态被误判.
#[tokio::test]
async fn wrongtype_is_preserved_for_non_zset_keys() {
    let dir = TempDir::new().unwrap();
    let (router, _storage) = open_at(dir.path(), false).await;
    exec(&router, "SET", &args(&["s", "x"])).await;
    let mut db = 0usize;
    let err = router
        .execute("ZADD", &args(&["s", "1", "m"]), &mut db)
        .await
        .unwrap_err();
    assert!(is_wrongtype(&err), "{err:?}");
    let err = router
        .execute("ZCARD", &args(&["s"]), &mut db)
        .await
        .unwrap_err();
    assert!(is_wrongtype(&err), "{err:?}");
}

/// `sync_wal` 开启时同步写快路径关闭, 写入回落 async 路径, 结果仍须正确.
#[tokio::test]
async fn sync_wal_falls_back_to_async_writes() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = open_at(dir.path(), true).await;
    exec(&router, "ZADD", &zadd_args("z", 0, 100, 0.0)).await;
    exec(&router, "ZADD", &args(&["z", "9", "m001"])).await;
    assert_int(exec(&router, "ZCARD", &args(&["z"])).await, 100);
    assert_eq!(
        bulk(exec(&router, "ZSCORE", &args(&["z", "m001"])).await),
        b"9".to_vec()
    );
    assert_index_consistent(&storage, "z").await;
}

/// 子键布局的写入必须推进被 WATCH 的 key 版本 (sync 快路径同样生效).
#[tokio::test]
async fn watch_version_advances_on_subkey_zadd() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = open_at(dir.path(), false).await;
    exec(&router, "ZADD", &zadd_args("z", 0, 100, 0.0)).await;
    storage.watch_registry().watch(0, b"z");
    let before = storage.get_watch_version(0, b"z").await.unwrap();
    exec(&router, "ZADD", &args(&["z", "1", "new"])).await;
    let after = storage.get_watch_version(0, b"z").await.unwrap();
    assert!(after > before, "{before} -> {after}");
}

// ---------------------------------------------------------------------------
// 2b: 范围读取 / 增量写入 / 脚本 / 重启 / 兼容命令 与内存模型逐条比对.
// ---------------------------------------------------------------------------

type Model = std::collections::BTreeMap<String, f64>;

/// 模型: 按 (分数, 成员) 升序.
fn model_sorted(model: &Model) -> Vec<(String, f64)> {
    let mut v: Vec<(String, f64)> = model.iter().map(|(m, s)| (m.clone(), *s)).collect();
    v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap().then_with(|| a.0.cmp(&b.0)));
    v
}

/// 模型: ZRANGE/ZREVRANGE 的闭区间名次语义 (负数从尾部计, 越界裁剪).
fn model_range(model: &Model, start: i64, stop: i64, reverse: bool) -> Vec<(String, f64)> {
    let mut v = model_sorted(model);
    if reverse {
        v.reverse();
    }
    let len = v.len() as i64;
    let s = if start < 0 {
        (len + start).max(0)
    } else {
        start
    };
    let e = if stop < 0 {
        len + stop
    } else {
        stop.min(len - 1)
    };
    if len == 0 || s > e || s >= len {
        return Vec::new();
    }
    v[s as usize..=e as usize].to_vec()
}

#[derive(Clone, Copy)]
struct Bound {
    v: f64,
    incl: bool,
}

fn parse_bound(s: &str) -> Bound {
    match s.strip_prefix('(') {
        Some(rest) => Bound {
            v: rest.parse().unwrap(),
            incl: false,
        },
        None => Bound {
            v: s.parse().unwrap(),
            incl: true,
        },
    }
}

fn in_bounds(score: f64, lo: Bound, hi: Bound) -> bool {
    let above = if lo.incl { score >= lo.v } else { score > lo.v };
    let below = if hi.incl { score <= hi.v } else { score < hi.v };
    above && below
}

/// 模型: `lo`/`hi` 为升序边界; `reverse` 时结果按分数降序, 边界不变.
fn model_by_score(
    model: &Model,
    lo: &str,
    hi: &str,
    reverse: bool,
    offset: usize,
    count: Option<usize>,
) -> Vec<(String, f64)> {
    let (lo, hi) = (parse_bound(lo), parse_bound(hi));
    let mut v: Vec<(String, f64)> = model_sorted(model)
        .into_iter()
        .filter(|(_, s)| in_bounds(*s, lo, hi))
        .collect();
    if reverse {
        v.reverse();
    }
    let rest = v.into_iter().skip(offset);
    match count {
        Some(n) => rest.take(n).collect(),
        None => rest.collect(),
    }
}

fn pairs_of(resp: RespValue) -> Vec<(String, f64)> {
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        other => panic!("expected array, got {other:?}"),
    };
    items
        .chunks(2)
        .map(|c| {
            let m = String::from_utf8(bulk(c[0].clone())).unwrap();
            let s: f64 = String::from_utf8(bulk(c[1].clone()))
                .unwrap()
                .parse()
                .unwrap();
            (m, s)
        })
        .collect()
}

fn zadd_model(model: &mut Model, pairs: &[(f64, String)]) -> i64 {
    let mut added = 0;
    for (s, m) in pairs {
        if model.insert(m.clone(), *s).is_none() {
            added += 1;
        }
    }
    added
}

fn zadd_cmd(key: &str, pairs: &[(f64, String)]) -> Vec<Bytes> {
    let mut out = vec![b(key)];
    for (s, m) in pairs {
        out.push(b(&format!("{s}")));
        out.push(b(m));
    }
    out
}

/// 逐条比对范围读取: ZRANGE/ZREVRANGE、ZRANGEBYSCORE/ZREVRANGEBYSCORE (含 LIMIT)、ZCOUNT、ZRANK、ZCARD.
async fn assert_reads_match(router: &CommandRouter, key: &str, model: &Model) {
    let len = model.len() as u64;
    assert_int(exec(router, "ZCARD", &args(&[key])).await, len as i64);
    for (start, stop) in [
        (0i64, -1i64),
        (0, 2),
        (-3, -1),
        (5, 5),
        (10, 4),
        (-200, 3),
        (90, 500),
        (0, 0),
    ] {
        for reverse in [false, true] {
            let cmd = if reverse { "ZREVRANGE" } else { "ZRANGE" };
            let (s, e) = (start.to_string(), stop.to_string());
            let resp = exec(router, cmd, &args(&[key, &s, &e, "WITHSCORES"])).await;
            assert_eq!(
                pairs_of(resp),
                model_range(model, start, stop, reverse),
                "{cmd} {start} {stop}"
            );
        }
    }
    let bounds = [
        ("-inf", "+inf"),
        ("(10", "20"),
        ("3", "(3"),
        ("0", "9"),
        ("15", "12"),
        ("5", "5"),
    ];
    for (lo, hi) in bounds {
        for (offset, count) in [
            (0usize, None),
            (1, Some(4usize)),
            (7, Some(0)),
            (3, Some(1000)),
        ] {
            let mut fwd = args(&[key, lo, hi, "WITHSCORES"]);
            let mut rev = args(&[key, hi, lo, "WITHSCORES"]);
            if let Some(n) = count {
                let limit = [b("LIMIT"), b(&offset.to_string()), b(&n.to_string())];
                fwd.extend(limit.iter().cloned());
                rev.extend(limit.iter().cloned());
            }
            let got = pairs_of(exec(router, "ZRANGEBYSCORE", &fwd).await);
            assert_eq!(
                got,
                model_by_score(model, lo, hi, false, offset, count),
                "ZRANGEBYSCORE {lo} {hi} {offset} {count:?}"
            );
            let got = pairs_of(exec(router, "ZREVRANGEBYSCORE", &rev).await);
            assert_eq!(
                got,
                model_by_score(model, lo, hi, true, offset, count),
                "ZREVRANGEBYSCORE {hi} {lo} {offset} {count:?}"
            );
        }
        let n = model_by_score(model, lo, hi, false, 0, None).len() as i64;
        assert_int(exec(router, "ZCOUNT", &args(&[key, lo, hi])).await, n);
    }
    let sorted = model_sorted(model);
    for (rank, (m, _)) in sorted.iter().enumerate().take(4) {
        assert_int(exec(router, "ZRANK", &args(&[key, m])).await, rank as i64);
    }
}

/// 同一组数据 (120 成员, 分数大量并列) 的范围读取与模型一致; 重启后结果不变.
#[tokio::test]
async fn subkey_range_commands_match_model_across_restart() {
    let dir = TempDir::new().unwrap();
    let mut model = Model::new();
    let pairs: Vec<(f64, String)> = (0..120)
        .map(|i| ((i % 10) as f64, format!("m{i:03}")))
        .collect();
    zadd_model(&mut model, &pairs);
    {
        let (router, storage) = open_at(dir.path(), false).await;
        exec(&router, "ZADD", &zadd_cmd("z", &pairs)).await;
        assert_eq!(
            layout_of(&storage, "z").await,
            Layout::Subkey { count: 120 }
        );
        assert_reads_match(&router, "z", &model).await;
        storage.flush_engine().await.unwrap();
        storage.close_engine().await.unwrap();
    }
    let (router, storage) = open_at(dir.path(), false).await;
    assert_eq!(
        layout_of(&storage, "z").await,
        Layout::Subkey { count: 120 }
    );
    assert_reads_match(&router, "z", &model).await;
    assert_index_consistent(&storage, "z").await;
}

/// ZINCRBY/ZPOPMIN/ZPOPMAX/ZREM 在子键布局上增量完成, 每步与模型一致;
/// 剩余成员降到 ≤32 时经 ZPOPMIN 回落为内联, 之后仍正确; 重启后一致.
#[tokio::test]
async fn subkey_incremental_writes_match_model() {
    let dir = TempDir::new().unwrap();
    let mut model = Model::new();
    let pairs: Vec<(f64, String)> = (0..100).map(|i| (i as f64, format!("m{i:03}"))).collect();
    zadd_model(&mut model, &pairs);
    {
        let (router, storage) = open_at(dir.path(), false).await;
        exec(&router, "ZADD", &zadd_cmd("z", &pairs)).await;

        let got = bulk(exec(&router, "ZINCRBY", &args(&["z", "0.5", "m010"])).await);
        *model.get_mut("m010").unwrap() += 0.5;
        assert_eq!(got, format_f64(model["m010"]).into_bytes());
        exec(&router, "ZINCRBY", &args(&["z", "1", "m050"])).await;
        model.insert("extra".into(), 100.0);
        exec(&router, "ZADD", &args(&["z", "100", "extra"])).await;
        *model.get_mut("m050").unwrap() += 1.0;
        assert_reads_match(&router, "z", &model).await;

        let popped = pairs_of(exec(&router, "ZPOPMIN", &args(&["z", "3"])).await);
        let expect: Vec<_> = model_sorted(&model).into_iter().take(3).collect();
        assert_eq!(popped, expect);
        for (m, _) in &expect {
            model.remove(m);
        }
        let popped = pairs_of(exec(&router, "ZPOPMAX", &args(&["z", "2"])).await);
        let expect: Vec<_> = model_sorted(&model).into_iter().rev().take(2).collect();
        assert_eq!(popped, expect);
        for (m, _) in &expect {
            model.remove(m);
        }
        assert_reads_match(&router, "z", &model).await;
        assert_eq!(layout_of(&storage, "z").await, Layout::Subkey { count: 96 });

        let rem: Vec<Bytes> = std::iter::once(b("z"))
            .chain((20..80).map(|i| b(&format!("m{i:03}"))))
            .collect();
        assert_int(exec(&router, "ZREM", &rem).await, 60);
        for i in 20..80 {
            model.remove(&format!("m{i:03}"));
        }
        assert_eq!(layout_of(&storage, "z").await, Layout::Subkey { count: 36 });
        assert_index_consistent(&storage, "z").await;
        assert_reads_match(&router, "z", &model).await;

        let popped = pairs_of(exec(&router, "ZPOPMIN", &args(&["z", "10"])).await);
        assert_eq!(
            popped,
            model_sorted(&model)
                .into_iter()
                .take(10)
                .collect::<Vec<_>>()
        );
        for (m, _) in &popped {
            model.remove(m);
        }
        assert_eq!(layout_of(&storage, "z").await, Layout::Inline);
        assert_reads_match(&router, "z", &model).await;
        storage.flush_engine().await.unwrap();
        storage.close_engine().await.unwrap();
    }
    let (router, storage) = open_at(dir.path(), false).await;
    assert_eq!(layout_of(&storage, "z").await, Layout::Inline);
    assert_reads_match(&router, "z", &model).await;
}

fn format_f64(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// 脚本内 ZADD/ZREM/ZRANGE/ZREVRANGEBYSCORE/ZCARD/ZRANK/ZSCORE 直接走子键视图, 结果与命令路径一致; 提交后与重启后可见.
#[tokio::test]
async fn script_on_subkey_zset_matches_commands_and_restart() {
    let dir = TempDir::new().unwrap();
    let script = "redis.call('ZADD', KEYS[1], 1000, 'extra'); \
                  redis.call('ZREM', KEYS[1], 'm000', 'm001'); \
                  local a = redis.call('ZRANGE', KEYS[1], 0, 2); \
                  local b = redis.call('ZREVRANGEBYSCORE', KEYS[1], '50', '0', 'LIMIT', 0, 2); \
                  local c = redis.call('ZCARD', KEYS[1]); \
                  local r = redis.call('ZRANK', KEYS[1], 'm010'); \
                  local s = redis.call('ZSCORE', KEYS[1], 'extra'); \
                  return {c, r, s, a[1], a[2], a[3], b[1], b[2]};";
    let expected = RespValue::Array(Some(vec![
        RespValue::Integer(99),
        RespValue::Integer(8),
        RespValue::BulkString(Some(Bytes::from_static(b"1000"))),
        RespValue::BulkString(Some(b("m002"))),
        RespValue::BulkString(Some(b("m003"))),
        RespValue::BulkString(Some(b("m004"))),
        RespValue::BulkString(Some(b("m050"))),
        RespValue::BulkString(Some(b("m049"))),
    ]));
    let zadd: Vec<(f64, String)> = (0..100).map(|i| (i as f64, format!("m{i:03}"))).collect();
    {
        let (router, storage) = open_at(dir.path(), false).await;
        exec(&router, "ZADD", &zadd_cmd("z", &zadd)).await;
        let got = exec(&router, "EVAL", &[b(script), b("1"), b("z")]).await;
        assert_eq!(got, expected);
        assert_eq!(layout_of(&storage, "z").await, Layout::Subkey { count: 99 });
        assert_index_consistent(&storage, "z").await;
        storage.flush_engine().await.unwrap();
        storage.close_engine().await.unwrap();
    }
    let (router, storage) = open_at(dir.path(), false).await;
    assert_eq!(layout_of(&storage, "z").await, Layout::Subkey { count: 99 });
    assert_int(exec(&router, "ZCARD", &args(&["z"])).await, 99);
    assert_eq!(
        exec(&router, "ZSCORE", &args(&["z", "m000"])).await,
        RespValue::BulkString(None)
    );
    assert_eq!(
        bulk(exec(&router, "ZSCORE", &args(&["z", "extra"])).await),
        b"1000".to_vec()
    );
    assert_index_consistent(&storage, "z").await;
}

/// 兼容命令: DUMP/RESTORE 与 SWAPDB 对子键布局的 ZSET 保持完整; KEYS 只看到用户 key, 不泄漏子键.
#[tokio::test]
async fn dump_restore_swapdb_and_keys_on_subkey_zset() {
    let dir = TempDir::new().unwrap();
    let (router, _storage) = open_at(dir.path(), false).await;
    exec(&router, "ZADD", &zadd_args("z", 0, 100, 0.0)).await;

    let payload = bulk(exec(&router, "DUMP", &args(&["z"])).await);
    exec(&router, "DEL", &args(&["z"])).await;
    let mut db = 0usize;
    router
        .execute("RESTORE", &[b("z"), b("0"), Bytes::from(payload)], &mut db)
        .await
        .unwrap();
    assert_int(exec(&router, "ZCARD", &args(&["z"])).await, 100);
    assert_eq!(
        bulk(exec(&router, "ZSCORE", &args(&["z", "m042"])).await),
        b"42".to_vec()
    );

    let keys = exec(&router, "KEYS", &args(&["*"])).await;
    assert_eq!(array_len(&keys), 1, "KEYS 不得返回子键: {keys:?}");

    exec(&router, "SWAPDB", &args(&["0", "1"])).await;
    assert_int(exec(&router, "ZCARD", &args(&["z"])).await, 0);
    let mut db1 = 1usize;
    let card = router
        .execute("ZCARD", &args(&["z"]), &mut db1)
        .await
        .unwrap();
    assert_int(card, 100);
}

/// COPY / MOVE 复制或迁移子键布局的 ZSET 时必须携带全部 Z/R 子键 (与 RENAME 同一 restage 路径).
#[tokio::test]
async fn copy_and_move_keep_subkey_zset_complete() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = open_at(dir.path(), false).await;
    exec(&router, "ZADD", &zadd_args("z", 0, 100, 0.0)).await;

    assert_int(exec(&router, "COPY", &args(&["z", "zc"])).await, 1);
    assert_int(exec(&router, "ZCARD", &args(&["zc"])).await, 100);
    assert_eq!(
        bulk(exec(&router, "ZSCORE", &args(&["zc", "m077"])).await),
        b"77".to_vec()
    );
    assert_index_consistent(&storage, "zc").await;

    let mut db0 = 0usize;
    let moved = router
        .execute("MOVE", &args(&["z", "1"]), &mut db0)
        .await
        .unwrap();
    assert_int(moved, 1);
    assert_int(exec(&router, "ZCARD", &args(&["z"])).await, 0);
    let mut db1 = 1usize;
    let card = router
        .execute("ZCARD", &args(&["z"]), &mut db1)
        .await
        .unwrap();
    assert_int(card, 100);
    let range = router
        .execute("ZRANGEBYSCORE", &args(&["z", "95", "+inf"]), &mut db1)
        .await
        .unwrap();
    assert_eq!(array_len(&range), 5);
}
