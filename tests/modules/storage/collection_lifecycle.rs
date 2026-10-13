//! 大集合生命周期: 版本化 subkey 的存储层测试.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use aikv::command::CommandRouter;
use aikv::protocol::RespValue;
use aikv::storage::adapter::{KvStorageAdapter, StorageAdapter};
use aikv::storage::collection::{CollectionWrite, HeaderWrite};
use aikv::storage::subkey::{subkey_db_prefix, SubTag};
use aikv::storage::{CollectionKind, KvStorage, MemoryEngine, StoredValue, ValueType};

use bytes::Bytes;

use super::map_adapter::MapAdapter;

fn adapter() -> (Arc<MapAdapter>, Arc<dyn KvStorage>) {
    let map = Arc::new(MapAdapter::new());
    let storage: Arc<dyn KvStorage> =
        KvStorageAdapter::new(Arc::clone(&map) as Arc<dyn StorageAdapter>);
    (map, storage)
}

fn big_hash(n: usize) -> HashMap<Vec<u8>, Vec<u8>> {
    (0..n)
        .map(|i| (format!("f{i}").into_bytes(), format!("v{i}").into_bytes()))
        .collect()
}

fn header_of(stored: &StoredValue) -> (CollectionKind, u64, u64) {
    match stored.value {
        ValueType::CollectionHeader {
            kind,
            count,
            version,
        } => (kind, count, version),
        ref other => panic!("expected header, got {other:?}"),
    }
}

/// 超过 64 个 field 的 Hash 写入后 header 为版本化布局, 展开后与原值一致.
#[tokio::test]
async fn put_value_converts_large_hash_and_expands_back() {
    let (_map, storage) = adapter();
    let hash = big_hash(100);
    storage
        .put_value(
            0,
            b"h",
            StoredValue {
                value: ValueType::Hash(hash.clone()),
                expires_at: None,
            },
        )
        .await
        .unwrap();
    let raw = storage.get_typed(0, b"h").await.unwrap().unwrap();
    let (kind, count, _) = header_of(&raw);
    assert_eq!((kind, count), (CollectionKind::Hash, 100));
    let expanded = storage.get_expanded(0, b"h").await.unwrap().unwrap();
    assert_eq!(expanded.value, ValueType::Hash(hash));
}

/// 64 个 field 及以下保持内联, 不产生 header.
#[tokio::test]
async fn put_value_keeps_small_hash_inline() {
    let (_map, storage) = adapter();
    let hash = big_hash(64);
    storage
        .put_value(
            0,
            b"h",
            StoredValue {
                value: ValueType::Hash(hash.clone()),
                expires_at: None,
            },
        )
        .await
        .unwrap();
    let raw = storage.get_typed(0, b"h").await.unwrap().unwrap();
    assert_eq!(raw.value, ValueType::Hash(hash));
}

/// 同一版本的 subkey 增删与 header 改写只经一次 `apply_writes`.
#[tokio::test]
async fn commit_collection_writes_in_one_call() {
    let (map, storage) = adapter();
    storage
        .put_value(
            0,
            b"s",
            StoredValue::new_set((0..70).map(|i| vec![i]).collect::<HashSet<_>>()),
        )
        .await
        .unwrap();
    let (kind, count, version) = header_of(&storage.get_typed(0, b"s").await.unwrap().unwrap());
    let before = map.apply_calls();
    storage
        .commit_collection(
            0,
            b"s",
            CollectionWrite {
                header: HeaderWrite::Put(StoredValue {
                    value: ValueType::CollectionHeader {
                        kind,
                        count: count + 2,
                        version,
                    },
                    expires_at: None,
                }),
                version,
                puts: vec![
                    (SubTag::Set, b"x".to_vec(), Vec::new()),
                    (SubTag::Set, b"y".to_vec(), Vec::new()),
                ],
                deletes: vec![(SubTag::Set, vec![0])],
            },
        )
        .await
        .unwrap();
    assert_eq!(map.apply_calls() - before, 1);
    let got = storage
        .subkey_get_many(0, b"s", version, SubTag::Set, &[b"x", &[0u8]])
        .await
        .unwrap();
    assert_eq!(got, vec![Some(Vec::new()), None]);
}

/// FLUSHDB 必须同时清空该 DB 的 subkey 键空间, 不留孤儿 subkey.
#[tokio::test]
async fn flushdb_clears_subkey_space() {
    let (map, storage) = adapter();
    storage
        .put_value(
            0,
            b"h",
            StoredValue {
                value: ValueType::Hash(big_hash(100)),
                expires_at: None,
            },
        )
        .await
        .unwrap();
    assert!(!map.keys_with_prefix(&subkey_db_prefix(0)).is_empty());
    storage.clear(0).await.unwrap();
    assert!(map.keys_with_prefix(&subkey_db_prefix(0)).is_empty());
}

async fn put_big_hash(storage: &Arc<dyn KvStorage>, db: usize, key: &[u8], n: usize) {
    storage
        .put_value(
            db,
            key,
            StoredValue {
                value: ValueType::Hash(big_hash(n)),
                expires_at: None,
            },
        )
        .await
        .unwrap();
}

/// MOVE 大 Hash 后目标库只剩 header 时 field 会丢失.
/// 期望: 目标库可读出全部 field, 源库不存在.
#[tokio::test]
async fn move_large_hash_keeps_fields() {
    let (_map, storage) = adapter();
    put_big_hash(&storage, 0, b"h", 100).await;
    assert!(storage.move_key(0, 1, b"h").await.unwrap());
    assert!(storage.get_typed(0, b"h").await.unwrap().is_none());
    let moved = storage.get_expanded(1, b"h").await.unwrap().unwrap();
    assert_eq!(moved.value, ValueType::Hash(big_hash(100)));
}

/// SWAPDB 只搬 header 时大集合数据会丢失.
/// 期望: 交换后两边的大集合内容完整.
#[tokio::test]
async fn swapdb_keeps_large_collections() {
    let (_map, storage) = adapter();
    put_big_hash(&storage, 0, b"a", 80).await;
    put_big_hash(&storage, 1, b"b", 90).await;
    storage.swap_db(0, 1).await.unwrap();
    let a = storage.get_expanded(1, b"a").await.unwrap().unwrap();
    let b = storage.get_expanded(0, b"b").await.unwrap().unwrap();
    assert_eq!(a.value, ValueType::Hash(big_hash(80)));
    assert_eq!(b.value, ValueType::Hash(big_hash(90)));
    assert!(storage.get_typed(0, b"a").await.unwrap().is_none());
}

/// RENAME 覆盖已有大 Hash 时, 目标旧 field 不得混入.
/// 期望: 目标内容严格等于源内容.
#[tokio::test]
async fn rename_over_existing_large_hash_has_no_stale_fields() {
    let (_map, storage) = adapter();
    put_big_hash(&storage, 0, b"src", 70).await;
    let mut other = big_hash(0);
    for i in 0..100 {
        other.insert(format!("old{i}").into_bytes(), b"x".to_vec());
    }
    storage
        .put_value(
            0,
            b"dst",
            StoredValue {
                value: ValueType::Hash(other),
                expires_at: None,
            },
        )
        .await
        .unwrap();
    storage.rename_key(0, b"src", b"dst").await.unwrap();
    let dst = storage.get_expanded(0, b"dst").await.unwrap().unwrap();
    assert_eq!(dst.value, ValueType::Hash(big_hash(70)));
}

async fn exec(router: &CommandRouter, cmd: &str, args: &[&[u8]]) -> RespValue {
    let args: Vec<Bytes> = args.iter().map(|a| Bytes::copy_from_slice(a)).collect();
    let mut db = 0usize;
    router.execute(cmd, &args, &mut db).await.unwrap()
}

/// DUMP 大 Hash 时只序列化 header, RESTORE 后 field 会丢失.
/// 期望: RESTORE 到新 key 后展开内容与原值一致.
#[tokio::test]
async fn dump_restore_large_hash_roundtrip() {
    let (_map, storage) = adapter();
    put_big_hash(&storage, 0, b"h", 100).await;
    let router = CommandRouter::new(storage.clone());
    let RespValue::BulkString(Some(payload)) = exec(&router, "DUMP", &[b"h"]).await else {
        panic!("dump");
    };
    exec(&router, "RESTORE", &[b"h2", b"0", &payload]).await;
    let restored = storage.get_expanded(0, b"h2").await.unwrap().unwrap();
    assert_eq!(restored.value, ValueType::Hash(big_hash(100)));
}

/// subkey 布局的 Hash 在 OBJECT ENCODING 下应报告 hashtable (与 Redis 大 Hash 一致).
#[tokio::test]
async fn object_encoding_reports_hashtable_for_subkey_layout() {
    let (_map, storage) = adapter();
    put_big_hash(&storage, 0, b"h", 100).await;
    let router = CommandRouter::new(storage);
    assert_eq!(
        exec(&router, "OBJECT", &[b"ENCODING", b"h"]).await,
        RespValue::BulkString(Some(Bytes::from_static(b"hashtable")))
    );
}

fn hset_args(key: &str, n: usize) -> Vec<Bytes> {
    let mut args = vec![Bytes::copy_from_slice(key.as_bytes())];
    for i in 0..n {
        args.push(Bytes::from(format!("f{i}")));
        args.push(Bytes::from(format!("v{i}")));
    }
    args
}

fn engines() -> Vec<(&'static str, Arc<dyn KvStorage>)> {
    let (_map, adapter_storage) = adapter();
    vec![
        ("memory", MemoryEngine::new(16) as Arc<dyn KvStorage>),
        ("adapter", adapter_storage),
    ]
}

async fn run(router: &CommandRouter, cmd: &str, args: Vec<Bytes>) -> RespValue {
    let mut db = 0usize;
    router.execute(cmd, &args, &mut db).await.unwrap()
}

fn b(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

/// 内存引擎在第 65 个 field 处报错 (不支持 raw subkey).
/// 期望: 两种引擎都能写入 100 个 field 且 HLEN=100. Issue #P7
#[tokio::test]
async fn hset_beyond_threshold_works_on_both_engines() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        assert_eq!(
            run(&router, "HSET", hset_args("h", 100)).await,
            RespValue::Integer(100),
            "{name}"
        );
        assert_eq!(
            run(&router, "HLEN", vec![b("h")]).await,
            RespValue::Integer(100),
            "{name}"
        );
        assert_eq!(
            run(&router, "HGET", vec![b("h"), b("f99")]).await,
            RespValue::BulkString(Some(b("v99"))),
            "{name}"
        );
    }
}

/// DEL 只删 header, 重建同名大 Hash 后旧 field 复活.
/// 期望: 重建后只有新 field. Issue #P1
#[tokio::test]
async fn del_then_recreate_large_hash_has_no_stale_fields() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        run(&router, "HSET", hset_args("h", 100)).await;
        run(&router, "DEL", vec![b("h")]).await;
        let mut args = vec![b("h")];
        for i in 0..70 {
            args.push(Bytes::from(format!("n{i}")));
            args.push(b("x"));
        }
        run(&router, "HSET", args).await;
        assert_eq!(
            run(&router, "HGET", vec![b("h"), b("f0")]).await,
            RespValue::BulkString(None),
            "{name}"
        );
        assert_eq!(
            run(&router, "HLEN", vec![b("h")]).await,
            RespValue::Integer(70),
            "{name}"
        );
    }
}

/// 已过期的大 Hash 被同名重建后旧 field 复活.
/// 期望: 过期后重建只看到新 field. Issue #P2
#[tokio::test]
async fn expired_large_hash_does_not_resurrect() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        run(&router, "HSET", hset_args("h", 100)).await;
        run(&router, "PEXPIRE", vec![b("h"), b("1")]).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        run(&router, "HSET", vec![b("h"), b("only"), b("1")]).await;
        assert_eq!(
            run(&router, "HLEN", vec![b("h")]).await,
            RespValue::Integer(1),
            "{name}"
        );
    }
}

/// 多 field HSET 逐 field 读写, 一次命令提交 N 次.
/// 期望: 对已是 subkey 布局的 Hash, 一条 HSET 只调用一次 apply_writes. Issue #P9
#[tokio::test]
async fn multi_field_hset_commits_once() {
    let (map, storage) = adapter();
    let router = CommandRouter::new(storage);
    run(&router, "HSET", hset_args("h", 100)).await;
    let before = map.apply_calls();
    let mut args = vec![b("h")];
    for i in 0..50 {
        args.push(Bytes::from(format!("g{i}")));
        args.push(b("v"));
    }
    assert_eq!(run(&router, "HSET", args).await, RespValue::Integer(50));
    assert_eq!(map.apply_calls() - before, 1);
}

/// 同一条 HSET 中重复 field 只计一次新增, 且以后者为准.
#[tokio::test]
async fn hset_counts_duplicate_fields_once() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        run(&router, "HSET", hset_args("h", 100)).await;
        let got = run(
            &router,
            "HSET",
            vec![b("h"), b("dup"), b("1"), b("dup"), b("2")],
        )
        .await;
        assert_eq!(got, RespValue::Integer(1), "{name}");
        assert_eq!(
            run(&router, "HLEN", vec![b("h")]).await,
            RespValue::Integer(101),
            "{name}"
        );
        assert_eq!(
            run(&router, "HGET", vec![b("h"), b("dup")]).await,
            RespValue::BulkString(Some(b("2"))),
            "{name}"
        );
    }
}

/// 父 key 含 0x01 被拒绝 (旧编码的限制).
/// 期望: 任意字节的 key 都能作为大 Hash 使用. Issue #P12
#[tokio::test]
async fn hash_key_with_separator_byte_is_allowed() {
    let (_map, storage) = adapter();
    let router = CommandRouter::new(storage);
    let mut args = hset_args("x", 100);
    args[0] = Bytes::from_static(b"a\x01b");
    assert_eq!(run(&router, "HSET", args).await, RespValue::Integer(100));
    assert_eq!(
        run(
            &router,
            "HGET",
            vec![Bytes::from_static(b"a\x01b"), b("f1")]
        )
        .await,
        RespValue::BulkString(Some(b("v1")))
    );
}

/// HSCAN 游标超过元素数时切片越界 panic.
/// 期望: 返回游标 0 与空页. Issue #P11
#[tokio::test]
async fn hscan_cursor_past_end_returns_empty_page() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        run(&router, "HSET", hset_args("h", 100)).await;
        let got = run(&router, "HSCAN", vec![b("h"), b("1000")]).await;
        assert_eq!(
            got,
            RespValue::Array(Some(vec![
                RespValue::BulkString(Some(b("0"))),
                RespValue::Array(Some(vec![])),
            ])),
            "{name}"
        );
    }
}

/// HDEL 删空大 Hash 后 key 必须不存在.
#[tokio::test]
async fn hdel_to_empty_removes_large_hash() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        run(&router, "HSET", hset_args("h", 100)).await;
        let mut args = vec![b("h")];
        for i in 0..100 {
            args.push(Bytes::from(format!("f{i}")));
        }
        assert_eq!(
            run(&router, "HDEL", args).await,
            RespValue::Integer(100),
            "{name}"
        );
        assert_eq!(
            run(&router, "EXISTS", vec![b("h")]).await,
            RespValue::Integer(0),
            "{name}"
        );
    }
}

/// 大 Hash 上的 HINCRBY 与 HSETNX 行为与小 Hash 一致.
#[tokio::test]
async fn hincrby_and_hsetnx_on_large_hash() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        run(&router, "HSET", hset_args("h", 100)).await;
        assert_eq!(
            run(&router, "HINCRBY", vec![b("h"), b("n"), b("5")]).await,
            RespValue::Integer(5),
            "{name}"
        );
        assert_eq!(
            run(&router, "HSETNX", vec![b("h"), b("n"), b("9")]).await,
            RespValue::Integer(0),
            "{name}"
        );
        assert_eq!(
            run(&router, "HLEN", vec![b("h")]).await,
            RespValue::Integer(101),
            "{name}"
        );
    }
}

fn sadd_args(key: &str, n: usize) -> Vec<Bytes> {
    let mut args = vec![b(key)];
    for i in 0..n {
        args.push(Bytes::from(format!("m{i}")));
    }
    args
}

/// S*STORE 结果超过阈值时仍须为 subkey 布局; 空结果删除目标.
/// 期望: 大结果 OBJECT ENCODING 为 hashtable, 空结果后目标不存在. Issue #P13
#[tokio::test]
async fn sunionstore_respects_threshold_and_empty_result() {
    let (_map, storage) = adapter();
    let router = CommandRouter::new(storage);
    run(&router, "SADD", sadd_args("a", 50)).await;
    let mut more = vec![b("b")];
    for i in 40..100 {
        more.push(Bytes::from(format!("m{i}")));
    }
    run(&router, "SADD", more).await;
    assert_eq!(
        run(&router, "SUNIONSTORE", vec![b("d"), b("a"), b("b")]).await,
        RespValue::Integer(100)
    );
    assert_eq!(
        run(&router, "OBJECT", vec![b("ENCODING"), b("d")]).await,
        RespValue::BulkString(Some(b("hashtable")))
    );
    assert_eq!(
        run(&router, "SINTERSTORE", vec![b("d"), b("a"), b("missing")]).await,
        RespValue::Integer(0)
    );
    assert_eq!(
        run(&router, "EXISTS", vec![b("d")]).await,
        RespValue::Integer(0)
    );
}

/// SMOVE 使目标超过阈值时必须转为 subkey 布局且成员完整. Issue #P13
#[tokio::test]
async fn smove_into_inline_set_converts_past_threshold() {
    let (_map, storage) = adapter();
    let router = CommandRouter::new(storage);
    run(&router, "SADD", sadd_args("dst", 64)).await;
    run(&router, "SADD", vec![b("src"), b("extra")]).await;
    assert_eq!(
        run(&router, "SMOVE", vec![b("src"), b("dst"), b("extra")]).await,
        RespValue::Integer(1)
    );
    assert_eq!(
        run(&router, "SCARD", vec![b("dst")]).await,
        RespValue::Integer(65)
    );
    assert_eq!(
        run(&router, "OBJECT", vec![b("ENCODING"), b("dst")]).await,
        RespValue::BulkString(Some(b("hashtable")))
    );
}

/// 大 Set 上的 SADD/SREM/SPOP/SSCAN 在两种引擎上行为一致.
#[tokio::test]
async fn set_commands_on_large_set_both_engines() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        assert_eq!(
            run(&router, "SADD", sadd_args("s", 100)).await,
            RespValue::Integer(100),
            "{name}"
        );
        assert_eq!(
            run(&router, "SADD", vec![b("s"), b("m1"), b("new"), b("new")]).await,
            RespValue::Integer(1),
            "{name}"
        );
        assert_eq!(
            run(&router, "SISMEMBER", vec![b("s"), b("m5")]).await,
            RespValue::Integer(1),
            "{name}"
        );
        assert_eq!(
            run(&router, "SREM", vec![b("s"), b("m5"), b("m5"), b("nope")]).await,
            RespValue::Integer(1),
            "{name}"
        );
        assert_eq!(
            run(&router, "SCARD", vec![b("s")]).await,
            RespValue::Integer(100),
            "{name}"
        );
        let RespValue::Array(Some(popped)) = run(&router, "SPOP", vec![b("s"), b("10")]).await
        else {
            panic!("{name}: spop");
        };
        assert_eq!(popped.len(), 10, "{name}");
        assert_eq!(
            run(&router, "SCARD", vec![b("s")]).await,
            RespValue::Integer(90),
            "{name}"
        );
        let got = run(&router, "SSCAN", vec![b("s"), b("5000")]).await;
        assert_eq!(
            got,
            RespValue::Array(Some(vec![
                RespValue::BulkString(Some(b("0"))),
                RespValue::Array(Some(vec![])),
            ])),
            "{name}"
        );
    }
}

/// Lua 写回展开后的集合: 脚本内 HDEL 删除的 field 之后不得复活.
/// 期望: 脚本删除的 f0 之后不可见, HLEN 与内容一致. Issue #P6
#[tokio::test]
async fn lua_rewrite_does_not_resurrect_old_fields() {
    let (_map, storage) = adapter();
    let router = CommandRouter::new(storage);
    run(&router, "HSET", hset_args("h", 100)).await;
    run(
        &router,
        "EVAL",
        vec![
            b("redis.call('HDEL', KEYS[1], 'f0'); return 1"),
            b("1"),
            b("h"),
        ],
    )
    .await;
    run(&router, "HSET", vec![b("h"), b("z"), b("1")]).await;
    assert_eq!(
        run(&router, "HGET", vec![b("h"), b("f0")]).await,
        RespValue::BulkString(None)
    );
    assert_eq!(
        run(&router, "HLEN", vec![b("h")]).await,
        RespValue::Integer(100)
    );
}

/// KEYS/SCAN 不得把 subkey 当作用户 key 返回; 即使 field 值恰好是合法 StoredValue.
/// 期望: 只返回用户 key `h`. Issue #P8
#[tokio::test]
async fn keys_and_scan_never_return_subkeys() {
    let (_map, storage) = adapter();
    let lookalike = postcard::to_allocvec(&StoredValue::string(b"x".to_vec())).unwrap();
    let mut hash = big_hash(100);
    hash.insert(b"evil".to_vec(), lookalike);
    storage
        .put_value(
            0,
            b"h",
            StoredValue {
                value: ValueType::Hash(hash),
                expires_at: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(storage.keys(0, b"*").await.unwrap(), vec![b"h".to_vec()]);
    let page = storage.scan(0, 0, b"*", 1000).await.unwrap();
    assert_eq!(page.keys, vec![b"h".to_vec()]);
}
