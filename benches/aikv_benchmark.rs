//! Performance benchmarks for AiKv
//!
//! These benchmarks measure the performance of various operations in AiKv.
//! Run with: `cargo bench`

use aikv::command::json::JsonCommands;
use aikv::protocol::parser::RespParser;
use aikv::protocol::types::RespValue;
use aikv::StorageEngine;
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
        let storage = StorageEngine::new_memory(16);
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
        let storage = StorageEngine::new_memory(16);
        storage
            .set("benchmark_key".to_string(), Bytes::from("benchmark_value"))
            .unwrap();
        b.iter(|| storage.get(black_box("benchmark_key")).unwrap());
    });

    // Benchmark EXISTS operation
    group.bench_function("exists", |b| {
        let storage = StorageEngine::new_memory(16);
        storage
            .set("benchmark_key".to_string(), Bytes::from("benchmark_value"))
            .unwrap();
        b.iter(|| storage.exists(black_box("benchmark_key")).unwrap());
    });

    // Benchmark DELETE operation
    group.bench_function("delete", |b| {
        b.iter_batched(
            || {
                let storage = StorageEngine::new_memory(16);
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
            let storage = StorageEngine::new_memory(16);

            b.iter(|| {
                for (key, value) in black_box(pairs.clone()) {
                    storage
                        .set_value(0, key, aikv::storage::StoredValue::new_string(value))
                        .unwrap();
                }
            });
        });

        // Benchmark MGET with different key counts
        group.bench_with_input(BenchmarkId::new("mget", size), size, |b, &size| {
            let storage = StorageEngine::new_memory(16);
            let pairs: Vec<(String, Bytes)> = (0..size)
                .map(|i| (format!("key_{}", i), Bytes::from(format!("value_{}", i))))
                .collect();
            let keys: Vec<String> = (0..size).map(|i| format!("key_{}", i)).collect();
            for (key, value) in pairs {
                storage
                    .set_value(0, key, aikv::storage::StoredValue::new_string(value))
                    .unwrap();
            }

            b.iter(|| {
                for key in black_box(&keys) {
                    storage.get_value(0, key).unwrap();
                }
            });
        });
    }

    group.finish();
}

/// Benchmark JSON operations
fn bench_json_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("json_operations");

    // Benchmark JSON.SET with simple object
    group.bench_function("json_set_simple", |b| {
        let json_cmd = JsonCommands::new(StorageEngine::new_memory(16));
        let json_str = r#"{"name":"John","age":30}"#;
        b.iter(|| {
            json_cmd
                .json_set(
                    &[
                        black_box(Bytes::from("user")),
                        black_box(Bytes::from("$")),
                        black_box(Bytes::from(json_str)),
                    ],
                    0,
                )
                .unwrap()
        });
    });

    // Benchmark JSON.SET with nested object
    group.bench_function("json_set_nested", |b| {
        let json_cmd = JsonCommands::new(StorageEngine::new_memory(16));
        let json_str =
            r#"{"user":{"name":"John","age":30,"address":{"city":"NYC","zip":"10001"}}}"#;
        b.iter(|| {
            json_cmd
                .json_set(
                    &[
                        black_box(Bytes::from("data")),
                        black_box(Bytes::from("$")),
                        black_box(Bytes::from(json_str)),
                    ],
                    0,
                )
                .unwrap()
        });
    });

    // Benchmark JSON.GET
    group.bench_function("json_get", |b| {
        let json_cmd = JsonCommands::new(StorageEngine::new_memory(16));
        let json_str = r#"{"name":"John","age":30}"#;
        json_cmd
            .json_set(
                &[Bytes::from("user"), Bytes::from("$"), Bytes::from(json_str)],
                0,
            )
            .unwrap();
        b.iter(|| {
            json_cmd
                .json_get(&[black_box(Bytes::from("user"))], 0)
                .unwrap()
        });
    });

    // Benchmark JSON.GET with path
    group.bench_function("json_get_path", |b| {
        let json_cmd = JsonCommands::new(StorageEngine::new_memory(16));
        let json_str = r#"{"user":{"name":"John","age":30}}"#;
        json_cmd
            .json_set(
                &[Bytes::from("data"), Bytes::from("$"), Bytes::from(json_str)],
                0,
            )
            .unwrap();
        b.iter(|| {
            json_cmd
                .json_get(
                    &[
                        black_box(Bytes::from("data")),
                        black_box(Bytes::from("$.user.name")),
                    ],
                    0,
                )
                .unwrap()
        });
    });

    // Benchmark JSON.TYPE
    group.bench_function("json_type", |b| {
        let json_cmd = JsonCommands::new(StorageEngine::new_memory(16));
        let json_str = r#"{"name":"John","age":30,"active":true}"#;
        json_cmd
            .json_set(
                &[Bytes::from("user"), Bytes::from("$"), Bytes::from(json_str)],
                0,
            )
            .unwrap();
        b.iter(|| {
            json_cmd
                .json_type(
                    &[
                        black_box(Bytes::from("user")),
                        black_box(Bytes::from("$.name")),
                    ],
                    0,
                )
                .unwrap()
        });
    });

    // Benchmark JSON.STRLEN
    group.bench_function("json_strlen", |b| {
        let json_cmd = JsonCommands::new(StorageEngine::new_memory(16));
        let json_str = r#"{"name":"John Doe","age":30}"#;
        json_cmd
            .json_set(
                &[Bytes::from("user"), Bytes::from("$"), Bytes::from(json_str)],
                0,
            )
            .unwrap();
        b.iter(|| {
            json_cmd
                .json_strlen(
                    &[
                        black_box(Bytes::from("user")),
                        black_box(Bytes::from("$.name")),
                    ],
                    0,
                )
                .unwrap()
        });
    });

    // Benchmark JSON.ARRLEN
    group.bench_function("json_arrlen", |b| {
        let json_cmd = JsonCommands::new(StorageEngine::new_memory(16));
        let json_str = "[1,2,3,4,5,6,7,8,9,10]";
        json_cmd
            .json_set(
                &[Bytes::from("arr"), Bytes::from("$"), Bytes::from(json_str)],
                0,
            )
            .unwrap();
        b.iter(|| {
            json_cmd
                .json_arrlen(&[black_box(Bytes::from("arr"))], 0)
                .unwrap()
        });
    });

    // Benchmark JSON.OBJLEN
    group.bench_function("json_objlen", |b| {
        let json_cmd = JsonCommands::new(StorageEngine::new_memory(16));
        let json_str = r#"{"a":1,"b":2,"c":3,"d":4,"e":5}"#;
        json_cmd
            .json_set(
                &[Bytes::from("obj"), Bytes::from("$"), Bytes::from(json_str)],
                0,
            )
            .unwrap();
        b.iter(|| {
            json_cmd
                .json_objlen(&[black_box(Bytes::from("obj"))], 0)
                .unwrap()
        });
    });

    // Benchmark JSON.DEL
    group.bench_function("json_del", |b| {
        b.iter_batched(
            || {
                let json_cmd = JsonCommands::new(StorageEngine::new_memory(16));
                let json_str = r#"{"name":"John","age":30}"#;
                json_cmd
                    .json_set(
                        &[Bytes::from("user"), Bytes::from("$"), Bytes::from(json_str)],
                        0,
                    )
                    .unwrap();
                json_cmd
            },
            |json_cmd| {
                json_cmd
                    .json_del(&[black_box(Bytes::from("user"))], 0)
                    .unwrap()
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark JSON operations with different data sizes
fn bench_json_data_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("json_data_sizes");

    for size in [10, 100, 1000].iter() {
        // Benchmark JSON.SET with arrays of different sizes
        group.bench_with_input(
            BenchmarkId::new("json_set_array", size),
            size,
            |b, &size| {
                let json_cmd = JsonCommands::new(StorageEngine::new_memory(16));
                let array: Vec<i32> = (0..size).collect();
                let json_str = serde_json::to_string(&array).unwrap();
                b.iter(|| {
                    json_cmd
                        .json_set(
                            &[
                                black_box(Bytes::from("arr")),
                                black_box(Bytes::from("$")),
                                black_box(Bytes::from(json_str.clone())),
                            ],
                            0,
                        )
                        .unwrap()
                });
            },
        );

        // Benchmark JSON.GET with arrays of different sizes
        group.bench_with_input(
            BenchmarkId::new("json_get_array", size),
            size,
            |b, &size| {
                let json_cmd = JsonCommands::new(StorageEngine::new_memory(16));
                let array: Vec<i32> = (0..size).collect();
                let json_str = serde_json::to_string(&array).unwrap();
                json_cmd
                    .json_set(
                        &[
                            Bytes::from("arr"),
                            Bytes::from("$"),
                            Bytes::from(json_str.clone()),
                        ],
                        0,
                    )
                    .unwrap();
                b.iter(|| {
                    json_cmd
                        .json_get(&[black_box(Bytes::from("arr"))], 0)
                        .unwrap()
                });
            },
        );

        // Benchmark JSON.SET with objects of different sizes
        group.bench_with_input(
            BenchmarkId::new("json_set_object", size),
            size,
            |b, &size| {
                let json_cmd = JsonCommands::new(StorageEngine::new_memory(16));
                let mut obj = serde_json::Map::new();
                for i in 0..size {
                    obj.insert(
                        format!("key_{}", i),
                        serde_json::json!(format!("value_{}", i)),
                    );
                }
                let json_str = serde_json::to_string(&obj).unwrap();
                b.iter(|| {
                    json_cmd
                        .json_set(
                            &[
                                black_box(Bytes::from("obj")),
                                black_box(Bytes::from("$")),
                                black_box(Bytes::from(json_str.clone())),
                            ],
                            0,
                        )
                        .unwrap()
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_resp_encoding,
    bench_resp_parsing,
    bench_storage_operations,
    bench_multi_key_operations,
    bench_json_operations,
    bench_json_data_sizes
);
criterion_main!(benches);
