//! 大 ZSET 的有界范围读: 存储原语 `raw_range_scan` 与命令层结果必须和全量加载排序的结果一致.

use std::sync::Arc;

use aikv::command::CommandRouter;
use aikv::storage::zset_subkey::score_prefix;
use aikv::storage::{AiDbEngine, KvStorage, KvStorageAdapter};
use bytes::Bytes;
use tempfile::TempDir;

fn setup(dir: &TempDir) -> (CommandRouter, Arc<dyn KvStorage>) {
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
    (CommandRouter::new(Arc::clone(&storage)), storage)
}

async fn fill(router: &CommandRouter, key: &str, entries: &[(f64, String)]) {
    let mut db = 0;
    for (score, member) in entries {
        let args = [
            Bytes::copy_from_slice(key.as_bytes()),
            Bytes::from(format!("{score}")),
            Bytes::copy_from_slice(member.as_bytes()),
        ];
        router.execute("ZADD", &args, &mut db).await.unwrap();
    }
}

fn index_bounds(key: &str) -> (Vec<u8>, Vec<u8>) {
    let prefix = score_prefix(&AiDbEngine::encode_key(0, key.as_bytes()));
    let mut end = prefix.clone();
    *end.last_mut().unwrap() += 1;
    (prefix, end)
}

fn member_of(engine_key: &[u8], prefix_len: usize) -> String {
    String::from_utf8(engine_key[prefix_len + 8..].to_vec()).unwrap()
}

#[tokio::test]
async fn raw_range_scan_is_bounded_ordered_and_reversible() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = setup(&dir);
    let entries: Vec<(f64, String)> = (0..100).map(|i| (i as f64, format!("m{i:03}"))).collect();
    fill(&router, "z", &entries).await;
    let (start, end) = index_bounds("z");
    let plen = start.len();

    let head = storage
        .raw_range_scan(start.clone(), end.clone(), 3, false)
        .await
        .unwrap();
    let names: Vec<_> = head.iter().map(|(k, _)| member_of(k, plen)).collect();
    assert_eq!(names, ["m000", "m001", "m002"]);

    let tail = storage
        .raw_range_scan(start.clone(), end.clone(), 2, true)
        .await
        .unwrap();
    let names: Vec<_> = tail.iter().map(|(k, _)| member_of(k, plen)).collect();
    assert_eq!(names, ["m099", "m098"]);

    let all = storage
        .raw_range_scan(start.clone(), end.clone(), usize::MAX, false)
        .await
        .unwrap();
    assert_eq!(all.len(), 100);

    let io = storage.script_io();
    let sync_tail = io.raw_range_scan(start, end, 2, true).unwrap();
    let names: Vec<_> = sync_tail.iter().map(|(k, _)| member_of(k, plen)).collect();
    assert_eq!(names, ["m099", "m098"]);
}

#[tokio::test]
async fn test_tombstone_scan_fast_path() {
    let dir = TempDir::new().unwrap();
    let (router, storage) = setup(&dir);
    let entries: Vec<(f64, String)> = (0..5000).map(|i| (i as f64, format!("m{i:04}"))).collect();
    fill(&router, "z", &entries).await;
    let (start, end) = index_bounds("z");
    let plen = start.len();
    let io = storage.script_io();
    let res = io
        .raw_range_scan(start.clone(), end.clone(), 32, false)
        .unwrap();
    assert_eq!(res.len(), 32);

    let mut db = 0;
    for i in 0..2500 {
        let args = [Bytes::from_static(b"z"), Bytes::from(format!("m{i:04}"))];
        router.execute("ZREM", &args, &mut db).await.unwrap();
    }
    let res = io.raw_range_scan(start, end, 32, false).unwrap();
    assert_eq!(res.len(), 32);
    let first = member_of(&res[0].0, plen);
    assert_eq!(first, "m2500");
}
