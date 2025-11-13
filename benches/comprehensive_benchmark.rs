//! Comprehensive performance benchmarks for AiKv
//!
//! This benchmark suite includes additional tests for:
//! - Concurrent operations
//! - Large data operations
//! - Pipeline operations
//! - Memory efficiency

use aikv::protocol::parser::RespParser;
use aikv::protocol::types::RespValue;
use aikv::storage::StorageAdapter;
use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::sync::Arc;

/// Benchmark concurrent storage operations
fn bench_concurrent_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_operations");

    for num_threads in [2, 4, 8].iter() {
        group.bench_with_input(
            BenchmarkId::new("concurrent_set", num_threads),
            num_threads,
            |b, &num_threads| {
                let storage = Arc::new(StorageAdapter::new());
                b.iter(|| {
                    let handles: Vec<_> = (0..num_threads)
                        .map(|thread_id| {
                            let storage_clone = Arc::clone(&storage);
                            std::thread::spawn(move || {
                                for i in 0..100 {
                                    let key = format!("key_{}_{}", thread_id, i);
                                    let value = Bytes::from(format!("value_{}_{}", thread_id, i));
                                    storage_clone.set(key, value).unwrap();
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("concurrent_get", num_threads),
            num_threads,
            |b, &num_threads| {
                let storage = Arc::new(StorageAdapter::new());
                // Pre-populate
                for thread_id in 0..num_threads {
                    for i in 0..100 {
                        let key = format!("key_{}_{}", thread_id, i);
                        let value = Bytes::from(format!("value_{}_{}", thread_id, i));
                        storage.set(key, value).unwrap();
                    }
                }

                b.iter(|| {
                    let handles: Vec<_> = (0..num_threads)
                        .map(|thread_id| {
                            let storage_clone = Arc::clone(&storage);
                            std::thread::spawn(move || {
                                for i in 0..100 {
                                    let key = format!("key_{}_{}", thread_id, i);
                                    black_box(storage_clone.get(&key).unwrap());
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark large value operations
fn bench_large_values(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_values");

    for size in [1024, 10_240, 102_400, 1_048_576].iter() {
        group.throughput(Throughput::Bytes(*size as u64));

        group.bench_with_input(BenchmarkId::new("set", size), size, |b, &size| {
            let storage = StorageAdapter::new();
            let value = Bytes::from(vec![b'x'; size]);
            b.iter(|| {
                storage
                    .set(black_box("large_key".to_string()), black_box(value.clone()))
                    .unwrap()
            });
        });

        group.bench_with_input(BenchmarkId::new("get", size), size, |b, &size| {
            let storage = StorageAdapter::new();
            let value = Bytes::from(vec![b'x'; size]);
            storage.set("large_key".to_string(), value).unwrap();
            b.iter(|| storage.get(black_box("large_key")).unwrap());
        });
    }

    group.finish();
}

/// Benchmark RESP protocol parsing with various message sizes
fn bench_resp_parsing_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("resp_parsing_sizes");

    for array_size in [1, 10, 100, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::new("array", array_size),
            array_size,
            |b, &array_size| {
                // Build an array command like *N\r\n$3\r\nSET\r\n$5\r\nkey_0\r\n$7\r\nvalue_0\r\n...
                let mut data = format!("*{}\r\n", array_size * 2 + 1);
                data.push_str("$3\r\nSET\r\n");
                for i in 0..array_size {
                    let key = format!("key_{}", i);
                    let val = format!("value_{}", i);
                    data.push_str(&format!("${}\r\n{}\r\n", key.len(), key));
                    data.push_str(&format!("${}\r\n{}\r\n", val.len(), val));
                }
                let data = Bytes::from(data);

                b.iter(|| {
                    let mut parser = RespParser::new(data.len() + 128);
                    parser.feed(black_box(&data));
                    parser.parse().unwrap()
                });
            },
        );
    }

    group.finish();
}

/// Benchmark batch operations with different batch sizes
fn bench_batch_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_sizes");

    for batch_size in [10, 50, 100, 500, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::new("mset", batch_size),
            batch_size,
            |b, &batch_size| {
                let pairs: Vec<(String, Bytes)> = (0..batch_size)
                    .map(|i| {
                        (
                            format!("batch_key_{}", i),
                            Bytes::from(format!("batch_value_{}", i)),
                        )
                    })
                    .collect();
                let storage = StorageAdapter::new();

                b.iter(|| {
                    for (key, value) in black_box(pairs.clone()) {
                        storage
                            .set_value(0, key, aikv::storage::StoredValue::new_string(value))
                            .unwrap();
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("mget", batch_size),
            batch_size,
            |b, &batch_size| {
                let storage = StorageAdapter::new();
                let pairs: Vec<(String, Bytes)> = (0..batch_size)
                    .map(|i| {
                        (
                            format!("batch_key_{}", i),
                            Bytes::from(format!("batch_value_{}", i)),
                        )
                    })
                    .collect();
                for (key, value) in pairs {
                    storage
                        .set_value(0, key, aikv::storage::StoredValue::new_string(value))
                        .unwrap();
                }
                let keys: Vec<String> = (0..batch_size)
                    .map(|i| format!("batch_key_{}", i))
                    .collect();

                b.iter(|| {
                    for key in black_box(&keys) {
                        storage.get_value(0, key).unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark memory allocation patterns
fn bench_memory_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_patterns");

    // Benchmark repeated allocations
    group.bench_function("repeated_alloc_dealloc", |b| {
        b.iter(|| {
            let storage = StorageAdapter::new();
            for i in 0..100 {
                storage
                    .set(format!("key_{}", i), Bytes::from(format!("value_{}", i)))
                    .unwrap();
            }
            for i in 0..100 {
                storage.delete(&format!("key_{}", i)).unwrap();
            }
        });
    });

    // Benchmark growing dataset
    group.bench_function("growing_dataset", |b| {
        b.iter(|| {
            let storage = StorageAdapter::new();
            for i in 0..1000 {
                storage
                    .set(format!("key_{}", i), Bytes::from(format!("value_{}", i)))
                    .unwrap();
            }
        });
    });

    group.finish();
}

/// Benchmark RESP3 types
fn bench_resp3_types(c: &mut Criterion) {
    let mut group = c.benchmark_group("resp3_types");

    // Benchmark null encoding
    group.bench_function("null_encoding", |b| {
        b.iter(|| {
            let value = RespValue::Null;
            black_box(value.serialize())
        });
    });

    // Benchmark boolean encoding
    group.bench_function("boolean_encoding", |b| {
        b.iter(|| {
            let value = RespValue::Boolean(black_box(true));
            black_box(value.serialize())
        });
    });

    // Benchmark double encoding
    group.bench_function("double_encoding", |b| {
        b.iter(|| {
            let value = RespValue::Double(black_box(std::f64::consts::PI));
            black_box(value.serialize())
        });
    });

    // Benchmark map encoding
    group.bench_function("map_encoding", |b| {
        let map = vec![
            (
                RespValue::BulkString(Some(Bytes::from("key1"))),
                RespValue::BulkString(Some(Bytes::from("value1"))),
            ),
            (
                RespValue::BulkString(Some(Bytes::from("key2"))),
                RespValue::BulkString(Some(Bytes::from("value2"))),
            ),
        ];
        let value = RespValue::Map(map);
        b.iter(|| black_box(value.serialize()));
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_concurrent_operations,
    bench_large_values,
    bench_resp_parsing_sizes,
    bench_batch_sizes,
    bench_memory_patterns,
    bench_resp3_types,
);
criterion_main!(benches);
