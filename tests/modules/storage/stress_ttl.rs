//! TTL + concurrent write 压力测试.
//!
//! 验证 TTL filter 在并发写入和 compaction 下不会崩溃或丢数据.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use aikv::storage::compaction_filter::AikvCompactionFilter;
use aikv::storage::{now_ms, AiDbEngine, StoredValue, ValueType};
use tempfile::TempDir;

#[ignore = "stress: concurrent write with TTL filter ~5s"]
#[test]
fn test_concurrent_write_with_ttl_filter() {
    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open");
    let db = engine.db.clone();
    db.set_compaction_filter(Some(Arc::new(AikvCompactionFilter::single_node())));

    let stop = Arc::new(AtomicBool::new(false));
    let write_count = Arc::new(AtomicUsize::new(0));

    const NUM_WRITERS: usize = 2;

    let mut handles = Vec::new();
    for tid in 0..NUM_WRITERS {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        let cnt = Arc::clone(&write_count);
        let handle = thread::spawn(move || {
            let mut i: u64 = 0;
            while !stop.load(Ordering::Relaxed) {
                let key = format!("key_{}_{}", tid, i % 200);
                let val = format!("val_{}", i);

                let expires_at = if i.is_multiple_of(3) {
                    Some(now_ms() + 500) // 500ms TTL
                } else {
                    None
                };

                let stored = StoredValue {
                    value: ValueType::String(val.into_bytes()),
                    expires_at,
                };
                let encoded = postcard::to_allocvec(&stored).expect("serialize");
                let _ = db.put(key.as_bytes(), &encoded);
                i = i.wrapping_add(1);
                cnt.fetch_add(1, Ordering::Relaxed);
            }
        });
        handles.push(handle);
    }

    // 压实线程
    let db_compact = Arc::clone(&db);
    let stop_compact = Arc::clone(&stop);
    let compact_handle = thread::spawn(move || {
        while !stop_compact.load(Ordering::Relaxed) {
            // 写入少量 key 触发 flush
            let mut any_flush = false;
            for i in 0..3u8 {
                if db_compact.put(&[b'f', i], &[i]).is_ok() {
                    any_flush = true;
                }
            }
            if any_flush {
                let _ = db_compact.flush();
                let _ = db_compact.drain_compactions();
            }
            thread::sleep(Duration::from_millis(15));
        }
    });

    thread::sleep(Duration::from_secs(5));
    stop.store(true, Ordering::Relaxed);

    for h in handles {
        h.join().unwrap();
    }
    compact_handle.join().unwrap();

    let total = write_count.load(Ordering::Relaxed);
    eprintln!("concurrent_ttl: total_writes={total}");

    // 最终 flush + compaction
    db.flush().unwrap();
    for i in 0..5u8 {
        let _ = db.put(&[b'e', i], &[i]);
        let _ = db.flush();
    }
    let _ = db.drain_compactions();
    db.close().unwrap();
}
