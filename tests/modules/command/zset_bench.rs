//! ZSET 单命令与脚本耗时基准 (真实 AiDb, release 构建). 默认 ignore, 手动运行:
//! `cargo test --release --test commands zset_single_command_bench -- --ignored --nocapture --test-threads=1`
//!
//! 口径: 单进程, 单连接串行调用 `CommandRouter::execute`, 不经网络; 每项先预热再计时,
//! 结果为平均每次调用耗时 (us/op). 数据: 1000 成员的单个 ZSET.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aikv::command::CommandRouter;
use aikv::protocol::RespValue;
use aikv::storage::{AiDbEngine, KvStorage, KvStorageAdapter};
use bytes::Bytes;
use tempfile::TempDir;

const N: usize = 1000;
const ITERS: usize = 2000;
const KEY: &str = "bench";

/// tl.hauler 风格脚本: 取最高分一条, 删除后以新分数写回, 返回基数.
const HAULER_SCRIPT: &str = "\
local k = KEYS[1]\n\
local top = redis.call('ZREVRANGEBYSCORE', k, '+inf', '-inf', 'LIMIT', 0, 1)\n\
local m = top[1]\n\
redis.call('ZREM', k, m)\n\
redis.call('ZADD', k, ARGV[1], m)\n\
return redis.call('ZCARD', k)";

fn b(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

fn member(i: usize) -> Bytes {
    b(&format!("member:{i:05}"))
}

fn report(label: &str, total: Duration, n: usize) {
    println!(
        "BENCH {label:<34} {:>9.2} us/op ({n} ops)",
        total.as_secs_f64() * 1e6 / n as f64
    );
}

#[tokio::test]
#[ignore]
async fn zset_single_command_bench() {
    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open(dir.path()).unwrap();
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
    let router = CommandRouter::new(storage);
    let mut db = 0usize;

    let mut args = vec![b(KEY)];
    for i in 0..N {
        args.push(b(&format!("{i}")));
        args.push(member(i));
    }
    router.execute("ZADD", &args, &mut db).await.unwrap();

    for it in 0..500 {
        let i = (it * 7919) % N;
        let args = [b(KEY), b(&format!("{}.5", it + 1_000_000)), member(i)];
        router.execute("ZADD", &args, &mut db).await.unwrap();
    }

    let t0 = Instant::now();
    for it in 0..ITERS {
        let i = (it * 7919) % N;
        let args = [b(KEY), b(&format!("{}.25", it + 2_000_000)), member(i)];
        router.execute("ZADD", &args, &mut db).await.unwrap();
    }
    report("ZADD change score", t0.elapsed(), ITERS);

    let t0 = Instant::now();
    for it in 0..ITERS {
        let i = (it * 7919) % N;
        let args = [b(KEY), b("1"), member(i)];
        std::hint::black_box(router.execute("ZINCRBY", &args, &mut db).await.unwrap());
    }
    report("ZINCRBY", t0.elapsed(), ITERS);

    let t0 = Instant::now();
    for _ in 0..ITERS {
        let args = [b(KEY), b("+inf"), b("-inf"), b("LIMIT"), b("0"), b("1")];
        std::hint::black_box(
            router
                .execute("ZREVRANGEBYSCORE", &args, &mut db)
                .await
                .unwrap(),
        );
    }
    report("ZREVRANGEBYSCORE LIMIT 0 1", t0.elapsed(), ITERS);

    let t0 = Instant::now();
    for _ in 0..ITERS {
        let args = [b(KEY), b("0"), b("9")];
        std::hint::black_box(router.execute("ZRANGE", &args, &mut db).await.unwrap());
    }
    report("ZRANGE 0 9", t0.elapsed(), ITERS);

    let t0 = Instant::now();
    for it in 0..ITERS {
        let i = (it * 7919) % N;
        let args = [b(KEY), member(i)];
        std::hint::black_box(router.execute("ZSCORE", &args, &mut db).await.unwrap());
    }
    report("ZSCORE", t0.elapsed(), ITERS);

    let t0 = Instant::now();
    for _ in 0..ITERS {
        let args = [b(KEY)];
        std::hint::black_box(router.execute("ZCARD", &args, &mut db).await.unwrap());
    }
    report("ZCARD", t0.elapsed(), ITERS);

    let t0 = Instant::now();
    for _ in 0..ITERS {
        let args = [b(KEY), b("1")];
        let popped = router.execute("ZPOPMIN", &args, &mut db).await.unwrap();
        let RespValue::Array(Some(items)) = popped else {
            panic!("ZPOPMIN returned no member");
        };
        let (RespValue::BulkString(Some(m)), RespValue::BulkString(Some(s))) =
            (&items[0], &items[1])
        else {
            panic!("unexpected ZPOPMIN reply");
        };
        let args = [b(KEY), s.clone(), m.clone()];
        router.execute("ZADD", &args, &mut db).await.unwrap();
    }
    report("ZPOPMIN + re-ZADD pair", t0.elapsed(), ITERS);
}

/// 脚本 (EVAL) 单次耗时: 与单命令基准分开运行, 避免 ZINCRBY 等慢项拖长.
#[tokio::test]
#[ignore]
async fn zset_script_bench() {
    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open(dir.path()).unwrap();
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
    let router = CommandRouter::new(storage);
    let mut db = 0usize;

    let mut args = vec![b(KEY)];
    for i in 0..N {
        args.push(b(&format!("{i}")));
        args.push(member(i));
    }
    router.execute("ZADD", &args, &mut db).await.unwrap();

    let probe = [b(HAULER_SCRIPT), b("1"), b(KEY), b("5000000")];
    let first = router.execute("EVAL", &probe, &mut db).await;
    println!("PROBE first EVAL result: {first:?}");
    first.unwrap();

    let script_n = 1000;
    let t0 = Instant::now();
    for it in 0..script_n {
        let args = [
            b(HAULER_SCRIPT),
            b("1"),
            b(KEY),
            b(&format!("{}", 3_000_000 + it)),
        ];
        std::hint::black_box(router.execute("EVAL", &args, &mut db).await.unwrap());
    }
    report(
        "EVAL hauler script (ZREVRANGEBYSCORE+ZREM+ZADD+ZCARD)",
        t0.elapsed(),
        script_n,
    );
}
