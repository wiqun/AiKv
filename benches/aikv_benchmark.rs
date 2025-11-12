//! Performance benchmarks for AiKv
//!
//! These benchmarks measure the performance of various operations in AiKv.
//! Run with: `cargo bench`

use aikv::protocol::parser::RespParser;
use aikv::protocol::types::RespValue;
use aikv::storage::StorageAdapter;
use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

/// Benchmark RESP protocol encoding
fn bench_resp_encoding(c: &mut Criterion) {
    let mut group = c.benchmark_group("resp_encoding");

    // Benchmark simple string encoding
    group.bench_function("simple_string", |b| {
        b.iter(|| {
            let value = RespValue::SimpleString(black_box("OK".to_string()));
            value.serialize()
        });
    });

    // Benchmark integer encoding
    group.bench_function("integer", |b| {
        b.iter(|| {
            let value = RespValue::Integer(black_box(12345));
            value.serialize()
        });
    });

    // Benchmark bulk string encoding
    group.bench_function("bulk_string", |b| {
        b.iter(|| {
            let value = RespValue::BulkString(Some(black_box(Bytes::from("hello world"))));
            value.serialize()
        });
    });

    // Benchmark array encoding
    group.bench_function("array", |b| {
        b.iter(|| {
            let value = RespValue::Array(Some(vec![
                RespValue::SimpleString("OK".to_string()),
                RespValue::Integer(123),
                RespValue::BulkString(Some(Bytes::from("test"))),
            ]));
            black_box(value.serialize())
        });
    });

    group.finish();
}

/// Benchmark RESP protocol parsing
fn bench_resp_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("resp_parsing");

    // Benchmark simple string parsing
    group.bench_function("simple_string", |b| {
        let data = Bytes::from("+OK\r\n");
        b.iter(|| {
            let mut parser = RespParser::new(64);
            parser.feed(black_box(&data));
            parser.parse().unwrap()
        });
    });

    // Benchmark integer parsing
    group.bench_function("integer", |b| {
        let data = Bytes::from(":12345\r\n");
        b.iter(|| {
            let mut parser = RespParser::new(64);
            parser.feed(black_box(&data));
            parser.parse().unwrap()
        });
    });

    // Benchmark bulk string parsing
    group.bench_function("bulk_string", |b| {
        let data = Bytes::from("$11\r\nhello world\r\n");
        b.iter(|| {
            let mut parser = RespParser::new(64);
            parser.feed(black_box(&data));
            parser.parse().unwrap()
        });
    });

    // Benchmark array parsing
    group.bench_function("array", |b| {
        let data = Bytes::from("*3\r\n+OK\r\n:123\r\n$4\r\ntest\r\n");
        b.iter(|| {
            let mut parser = RespParser::new(128);
            parser.feed(black_box(&data));
            parser.parse().unwrap()
        });
    });

    group.finish();
}

/// Benchmark storage operations
fn bench_storage_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage_operations");

    // Benchmark SET operation
    group.bench_function("set", |b| {
        let storage = StorageAdapter::new();
        b.iter(|| {
            storage
                .set(
                    black_box("benchmark_key".to_string()),
                    black_box(Bytes::from("benchmark_value")),
                )
                .unwrap()
        });
    });

    // Benchmark GET operation
    group.bench_function("get", |b| {
        let storage = StorageAdapter::new();
        storage
            .set("benchmark_key".to_string(), Bytes::from("benchmark_value"))
            .unwrap();
        b.iter(|| storage.get(black_box("benchmark_key")).unwrap());
    });

    // Benchmark EXISTS operation
    group.bench_function("exists", |b| {
        let storage = StorageAdapter::new();
        storage
            .set("benchmark_key".to_string(), Bytes::from("benchmark_value"))
            .unwrap();
        b.iter(|| storage.exists(black_box("benchmark_key")).unwrap());
    });

    // Benchmark DELETE operation
    group.bench_function("delete", |b| {
        b.iter_batched(
            || {
                let storage = StorageAdapter::new();
                storage
                    .set("benchmark_key".to_string(), Bytes::from("benchmark_value"))
                    .unwrap();
                storage
            },
            |storage| storage.delete(black_box("benchmark_key")).unwrap(),
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark multiple key operations
fn bench_multi_key_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi_key_operations");

    for size in [10, 100, 1000].iter() {
        // Benchmark MSET with different key counts
        group.bench_with_input(BenchmarkId::new("mset", size), size, |b, &size| {
            let pairs: Vec<(String, Bytes)> = (0..size)
                .map(|i| (format!("key_{}", i), Bytes::from(format!("value_{}", i))))
                .collect();
            let storage = StorageAdapter::new();

            b.iter(|| storage.mset(black_box(pairs.clone())).unwrap());
        });

        // Benchmark MGET with different key counts
        group.bench_with_input(BenchmarkId::new("mget", size), size, |b, &size| {
            let storage = StorageAdapter::new();
            let pairs: Vec<(String, Bytes)> = (0..size)
                .map(|i| (format!("key_{}", i), Bytes::from(format!("value_{}", i))))
                .collect();
            let keys: Vec<String> = (0..size).map(|i| format!("key_{}", i)).collect();
            storage.mset(pairs).unwrap();

            b.iter(|| storage.mget(black_box(&keys)).unwrap());
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_resp_encoding,
    bench_resp_parsing,
    bench_storage_operations,
    bench_multi_key_operations
);
criterion_main!(benches);
