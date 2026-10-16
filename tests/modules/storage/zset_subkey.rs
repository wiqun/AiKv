//! 大 ZSET 拆成 member subkey + score 索引后, 顺序和脚本写入仍然正确.

use std::sync::{Arc, Mutex};

use aikv::command::CommandRouter;
use aikv::protocol::RespValue;
use aikv::storage::{AiDbEngine, CollectionKind, KvStorage, KvStorageAdapter, ValueType};
use bytes::Bytes;
use tempfile::TempDir;

fn router(dir: &TempDir) -> (CommandRouter, Arc<dyn KvStorage>) {
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
    let router = CommandRouter::new(Arc::clone(&storage));
    (router, storage)
}

fn bulk(bytes: &[u8]) -> Bytes {
    Bytes::copy_from_slice(bytes)
}

fn assert_int(value: RespValue, expect: i64) {
    assert_eq!(value, RespValue::Integer(expect));
}

fn members(value: RespValue) -> Vec<String> {
    let RespValue::Array(Some(items)) = value else {
        panic!("expected array, got {value:?}");
    };
    items
        .into_iter()
        .map(|item| {
            let RespValue::BulkString(Some(raw)) = item else {
                panic!("expected bulk");
            };
            String::from_utf8(raw.to_vec()).unwrap()
        })
        .collect()
}

async fn zadd(router: &CommandRouter, score: &str, member: &str) {
    let mut db = 0;
    let resp = router
        .execute(
            "ZADD",
            &[bulk(b"z"), bulk(score.as_bytes()), bulk(member.as_bytes())],
            &mut db,
        )
        .await
        .unwrap();
    let _ = resp;
}

#[tokio::test]
async fn large_zset_splits_and_keeps_score_order() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = router(&dir);
    for i in 0..65 {
        let score = format!("{}", 100 - i);
        let member = format!("m{i:02}");
        zadd(&router, &score, &member).await;
    }

    let stored = storage.get_typed(0, b"z").await.unwrap().unwrap();
    match stored.value {
        ValueType::CollectionHeader {
            kind: CollectionKind::ZSet,
            count: 65,
        } => {}
        other => panic!("expected zset subkey header, got {other:?}"),
    }

    let mut db = 0;
    let lowest = members(
        router
            .execute("ZRANGE", &[bulk(b"z"), bulk(b"0"), bulk(b"0")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(lowest, vec!["m64".to_string()]);

    zadd(&router, "1000", "m64").await;
    let indexed = score_index_members(storage.as_ref()).await;
    assert_eq!(indexed.len(), 65, "old score index entry still present");
    assert_eq!(
        indexed
            .iter()
            .filter(|member| member.as_slice() == b"m64")
            .count(),
        1
    );
    let lowest = members(
        router
            .execute("ZRANGE", &[bulk(b"z"), bulk(b"0"), bulk(b"0")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(lowest, vec!["m63".to_string()]);

    assert_int(
        router
            .execute("ZREM", &[bulk(b"z"), bulk(b"m00")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_int(
        router
            .execute("ZCARD", &[bulk(b"z")], &mut db)
            .await
            .unwrap(),
        64,
    );

    assert_int(
        router.execute("DEL", &[bulk(b"z")], &mut db).await.unwrap(),
        1,
    );
    for i in 0..65 {
        let score = format!("{i}");
        let member = format!("n{i:02}");
        zadd(&router, &score, &member).await;
    }
    assert_int(
        router
            .execute("ZCARD", &[bulk(b"z")], &mut db)
            .await
            .unwrap(),
        65,
    );
    let all = members(
        router
            .execute("ZRANGE", &[bulk(b"z"), bulk(b"0"), bulk(b"-1")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(all.len(), 65);
    assert_eq!(all[0], "n00");
    assert_eq!(all[64], "n64");
}

#[tokio::test]
async fn script_zadd_on_large_zset_appends_one_member() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = router(&dir);
    for i in 0..65 {
        zadd(&router, &format!("{i}"), &format!("m{i:02}")).await;
    }
    let mut db = 0;
    let script = Bytes::from_static(
        b"redis.call('ZADD', KEYS[1], '1000', 'extra'); return redis.call('ZCARD', KEYS[1])",
    );
    let resp = router
        .execute("EVAL", &[script, bulk(b"1"), bulk(b"z")], &mut db)
        .await
        .unwrap();
    assert_int(resp, 66);
    let stored = storage.get_typed(0, b"z").await.unwrap().unwrap();
    match stored.value {
        ValueType::CollectionHeader {
            kind: CollectionKind::ZSet,
            count: 66,
        } => {}
        other => panic!("script write collapsed zset, got {other:?}"),
    }
    let top = members(
        router
            .execute(
                "ZREVRANGEBYSCORE",
                &[
                    bulk(b"z"),
                    bulk(b"+inf"),
                    bulk(b"-inf"),
                    bulk(b"LIMIT"),
                    bulk(b"0"),
                    bulk(b"1"),
                ],
                &mut db,
            )
            .await
            .unwrap(),
    );
    assert_eq!(top, vec!["extra".to_string()]);
    let top_script = Bytes::from_static(b"return redis.call('ZREVRANGE', KEYS[1], '0', '0')");
    let top_resp = router
        .execute("EVAL", &[top_script, bulk(b"1"), bulk(b"z")], &mut db)
        .await
        .unwrap();
    assert_eq!(members(top_resp), vec!["extra".to_string()]);
    let indexed = score_index_members(storage.as_ref()).await;
    assert_eq!(indexed.len(), 66);
    assert_eq!(
        indexed
            .iter()
            .filter(|member| member.as_slice() == b"extra")
            .count(),
        1
    );
}

#[tokio::test]
async fn script_zrevrangebyscore_limit_0_1_on_large_zset_locates_by_score() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = router(&dir);
    for i in 0..70 {
        zadd(&router, &format!("{i}"), &format!("m{i:02}")).await;
    }
    let stored = storage.get_typed(0, b"z").await.unwrap().unwrap();
    assert!(aikv::storage::zset_subkey::is_zset_header(&stored).is_some());

    let mut db = 0;
    // 1. 精确上界 (inclusive 50) -> 定位到 m50
    let script = Bytes::from_static(
        b"return redis.call('ZREVRANGEBYSCORE', KEYS[1], '50', '0', 'LIMIT', '0', '1')",
    );
    let resp = router
        .execute("EVAL", &[script, bulk(b"1"), bulk(b"z")], &mut db)
        .await
        .unwrap();
    assert_eq!(members(resp), vec!["m50".to_string()]);

    // 2. 开区间上界 ((50) -> 定位到 m49
    let script_ex = Bytes::from_static(
        b"return redis.call('ZREVRANGEBYSCORE', KEYS[1], '(50', '0', 'LIMIT', '0', '1')",
    );
    let resp_ex = router
        .execute("EVAL", &[script_ex, bulk(b"1"), bulk(b"z")], &mut db)
        .await
        .unwrap();
    assert_eq!(members(resp_ex), vec!["m49".to_string()]);

    // 3. 超出范围 (-1) -> 返回空
    let script_empty = Bytes::from_static(
        b"return redis.call('ZREVRANGEBYSCORE', KEYS[1], '-1', '-10', 'LIMIT', '0', '1')",
    );
    let resp_empty = router
        .execute("EVAL", &[script_empty, bulk(b"1"), bulk(b"z")], &mut db)
        .await
        .unwrap();
    assert_eq!(members(resp_empty), Vec::<String>::new());

    // 4. 事务内修改后读取触发安全回退
    let script_delta = Bytes::from_static(
        b"redis.call('ZADD', KEYS[1], '55.5', 'm_new'); return redis.call('ZREVRANGEBYSCORE', KEYS[1], '55.8', '0', 'LIMIT', '0', '1')",
    );
    let resp_delta = router
        .execute("EVAL", &[script_delta, bulk(b"1"), bulk(b"z")], &mut db)
        .await
        .unwrap();
    assert_eq!(members(resp_delta), vec!["m_new".to_string()]);
}

async fn score_index_members(storage: &dyn KvStorage) -> Vec<Vec<u8>> {
    let encoded = AiDbEngine::encode_key(0, b"z");
    let prefix = aikv::storage::zset_subkey::score_prefix(&encoded);
    let rows = Arc::new(Mutex::new(Vec::new()));
    let rows_c = Arc::clone(&rows);
    let encoded_c = encoded.clone();
    storage
        .raw_subkey_for_each(
            0,
            prefix,
            Box::new(move |key, _| {
                if let Some(member) =
                    aikv::storage::zset_subkey::member_from_score_key(&encoded_c, &key)
                {
                    rows_c.lock().unwrap().push(member);
                }
                Ok(())
            }),
        )
        .await
        .unwrap();
    Arc::try_unwrap(rows).unwrap().into_inner().unwrap()
}
