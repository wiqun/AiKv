//! 可观测性测试

use std::time::Duration;

use super::helpers::{connect, start_server, write_cmd};
#[cfg(feature = "monitoring")]
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn test_connection_metrics() {
    let storage = aikv::storage::MemoryEngine::new(16);
    let state = aikv::server::ServerSharedState::new(
        aikv::server::ConnectionConfig {
            read_timeout: None,
            idle_timeout: None,
            max_clients: 0,
            script_worker_threads: 2,
        },
        storage,
        0,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state_clone = std::sync::Arc::clone(&state);
    tokio::spawn(async move {
        let _ = aikv::server::Server::run_with_listener(listener, state_clone).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(state.metrics().connections_total(), 0);
    let mut stream = connect(addr).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(state.metrics().connections_total(), 1);
    assert_eq!(state.metrics().connected_clients(), 1);

    write_cmd(&mut stream, &[b"QUIT"]).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(stream);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(state.metrics().connections_total(), 1);
    assert_eq!(state.metrics().connected_clients(), 0);
}

#[tokio::test]
async fn test_connection_lifecycle_logs() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"PING"]).await;
    let _ = super::helpers::read_response(&mut stream).await;
    drop(stream);
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn test_command_metrics_recorded() {
    use bytes::Bytes;
    use std::sync::Arc;

    use aikv::server::{ConnectionConfig, ServerSharedState};
    use aikv::storage::{KvStorage, MemoryEngine};

    fn b(s: &str) -> Bytes {
        Bytes::copy_from_slice(s.as_bytes())
    }

    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    let shared = ServerSharedState::new(ConnectionConfig::default(), storage, 6379);
    let router = shared.router();
    let mut db = 0;

    router
        .execute("GET", &[b("missing")], &mut db)
        .await
        .unwrap();
    router
        .execute("SET", &[b("k"), b("v")], &mut db)
        .await
        .unwrap();
    router.execute("GET", &[b("k")], &mut db).await.unwrap();
    router
        .execute("INFO", &[b("stats")], &mut db)
        .await
        .unwrap();

    assert_eq!(shared.metrics().keyspace_hits(), 1);
    assert_eq!(shared.metrics().keyspace_misses(), 1);
}

/// 验证 Metrics HTTP 端点 /health 可用; 生产指标经 OTLP 出口, 不暴露 /metrics.
#[cfg(feature = "monitoring")]
#[tokio::test]
async fn test_metrics_health_endpoint() {
    use aikv::server::MetricsServer;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind metrics server");
    let addr = listener.local_addr().unwrap();
    let svc = MetricsServer::from_listener(listener);
    tokio::spawn(async move {
        svc.run().await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect metrics server");
    let request = b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    stream.write_all(request).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let body = String::from_utf8_lossy(&response);
    assert!(body.contains("200 OK"), "health endpoint failed: {body}");
    assert!(body.contains("OK"), "health body unexpected: {body}");

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect metrics server");
    let request = b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    stream.write_all(request).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let body = String::from_utf8_lossy(&response);
    assert!(
        body.contains("404 Not Found"),
        "expected /metrics removed: {body}"
    );
}
/// 验证关键 tracing span 名称与文档规范一致.
#[test]
fn test_span_naming() {
    // 项目根目录 (cargo test 的 CARGO_MANIFEST_DIR 指向 AiKv)
    let kv_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let db_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("AiDb")
        .join("src");

    // 期望存在的 span 名称: AiDb 引擎核心路径
    let expected_db_spans: &[&str] = &["db_flush", "cmp_run", "raft_propose"];

    // 期望存在的 span 名称: AiKv 只保留命令入口.
    let expected_kv_spans: &[&str] = &["kv_command"];

    let mut missing = Vec::new();

    for name in expected_db_spans {
        if db_src.exists() {
            let pattern = format!("optional_span(\"{name}\")");
            let found = walk_grep(&db_src, &pattern).unwrap_or(false);
            if !found {
                missing.push(format!("DbSpan::{name}"));
            }
        }
    }

    for name in expected_kv_spans {
        let pattern = format!("optional_span(\"{name}\")");
        let found = walk_grep(&kv_src, &pattern).unwrap_or(false);
        if !found {
            missing.push(format!("KvSpan::{name}"));
        }
    }

    assert!(
        missing.is_empty(),
        "missing expected span names: {}",
        missing.join(", ")
    );
}

/// 只允许命令入口 span `kv_command`, 且不得再有 `#[instrument]`.
#[test]
fn test_hot_path_span_level_debug() {
    let kv_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    walk(&kv_src, &mut files);
    let mut violations = Vec::new();
    for path in files {
        let content = std::fs::read_to_string(&path).unwrap();
        let rel = path.strip_prefix(&kv_src).unwrap().display().to_string();
        if content.contains("#[instrument") || content.contains("#[tracing::instrument") {
            violations.push(format!("{rel} still has an instrument attribute"));
        }
        let mut rest = content.as_str();
        while let Some(index) = rest.find("optional_span(\"") {
            let after = &rest[index + "optional_span(\"".len()..];
            let end = after.find('"').unwrap();
            let name = &after[..end];
            if name != "kv_command" {
                violations.push(format!("{rel} optional_span {name}"));
            }
            rest = &after[end..];
        }
    }
    let connection = std::fs::read_to_string(kv_src.join("server/connection/mod.rs")).unwrap();
    assert!(!connection.contains("kv_connection"));
    assert!(
        violations.is_empty(),
        "disallowed spans:\n{}",
        violations.join("\n")
    );
}

/// 采样率解析契约: `AIKV_OTEL_SAMPLE_RATIO` 合法范围 0.0-1.0, 非法输入回退 0.0.
#[cfg(feature = "monitoring")]
#[test]
fn test_sample_ratio_parse() {
    use aikv::server::otel::parse_sample_ratio;

    assert_eq!(parse_sample_ratio("1"), 1.0);
    assert_eq!(parse_sample_ratio("0.1"), 0.1);
    assert_eq!(parse_sample_ratio("0"), 0.0);
    assert_eq!(parse_sample_ratio("0.5"), 0.5);
    assert_eq!(parse_sample_ratio(" 0.5 "), 0.5);
    assert_eq!(parse_sample_ratio("+0.5"), 0.5);
    // 非法/越界 → 回退 0.0
    assert_eq!(parse_sample_ratio(""), 0.0);
    assert_eq!(parse_sample_ratio("abc"), 0.0);
    assert_eq!(parse_sample_ratio("-0.5"), 0.0);
    assert_eq!(parse_sample_ratio("1.5"), 0.0);
    assert_eq!(parse_sample_ratio("2"), 0.0);
    assert_eq!(parse_sample_ratio("NaN"), 0.0);
    assert_eq!(parse_sample_ratio("inf"), 0.0);
}

/// 递归搜索文件中是否包含指定字符串.
fn walk_grep(dir: &std::path::Path, pattern: &str) -> Result<bool, std::io::Error> {
    if !dir.is_dir() {
        return Ok(false);
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        for entry in std::fs::read_dir(&cur)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                let content = std::fs::read_to_string(&path)?;
                if content.contains(pattern) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

#[cfg(feature = "monitoring")]
#[tokio::test]
async fn test_otel_metrics_integration() {
    use aikv::server::otel_metrics::testutil;

    let (exporter, otel) = testutil::init_in_memory();
    let server_metrics = aikv::server::ServerMetrics::default().with_otel(otel);
    let before = testutil::counter_sum(&exporter, "aikv_connections_total");

    server_metrics.on_connect();
    server_metrics.on_command("SET", true);
    server_metrics.on_command("GET", false);

    aikv::server::info_catalog::sync_otel_from_server_metrics(&server_metrics);

    let after = testutil::counter_sum(&exporter, "aikv_connections_total");
    assert!(after > before, "connections_total should increase");
    assert!(testutil::metric_exists(&exporter, "aikv_commands_total"));
}

#[cfg(feature = "monitoring")]
#[tokio::test]
async fn test_maxclients_rejection_metrics() {
    use std::sync::Arc;

    use aikv::server::otel_metrics::testutil;
    use aikv::server::{ConnectionConfig, ServerSharedState};
    use aikv::storage::{KvStorage, MemoryEngine};

    let (exporter, _otel) = testutil::init_in_memory();
    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    let state = ServerSharedState::new(
        ConnectionConfig {
            read_timeout: None,
            idle_timeout: None,
            max_clients: 1,
            script_worker_threads: 2,
        },
        storage,
        6379,
    );

    assert!(state.try_register_connection());
    assert!(!state.try_register_connection());

    aikv::server::info_catalog::sync_otel_from_server_metrics(state.metrics());

    assert_eq!(
        testutil::counter_sum(&exporter, "aikv_rejected_connections_total"),
        1
    );
}

#[cfg(feature = "monitoring")]
#[tokio::test]
async fn test_runtime_metrics_refresh() {
    use std::sync::Arc;

    use aikv::server::otel_metrics::testutil;
    use aikv::server::{ConnectionConfig, ServerSharedState};
    use aikv::storage::{KvStorage, MemoryEngine, StorageEngineKind, StorageObservation};
    use bytes::Bytes;

    fn b(s: &str) -> Bytes {
        Bytes::copy_from_slice(s.as_bytes())
    }

    let (exporter, _otel) = testutil::init_in_memory();
    let observation = StorageObservation::new();
    let storage: Arc<dyn KvStorage> =
        MemoryEngine::with_observation(16, Some(Arc::clone(&observation)));
    let state = ServerSharedState::new_with_backup_dir(
        ConnectionConfig::default(),
        storage,
        6379,
        StorageEngineKind::Memory,
        None,
        None,
        9191,
        "127.0.0.1".into(),
        Some(observation),
    );

    let router = state.router();
    let mut db = 0;
    router
        .execute("SET", &[b("alive1"), b("v")], &mut db)
        .await
        .unwrap();
    router
        .execute("SET", &[b("alive2"), b("v")], &mut db)
        .await
        .unwrap();
    router
        .execute("SET", &[b("expired"), b("x"), b("PX"), b("1")], &mut db)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    router
        .execute("GET", &[b("expired")], &mut db)
        .await
        .unwrap();

    state.refresh_runtime_metrics().await;

    assert!(
        state.metrics().expired_keys() >= 1,
        "expected expired key counter on ServerMetrics"
    );
    assert!(
        state.metrics().used_memory_bytes() > 0,
        "expected non-zero memory on ServerMetrics"
    );
    assert_eq!(state.metrics().blocked_clients(), 0);
    let _ = exporter;
}

#[cfg(all(feature = "monitoring", target_os = "linux"))]
#[tokio::test]
async fn process_resident_memory_bytes_in_registry() {
    use std::sync::Arc;

    use aikv::server::otel_metrics::testutil;
    use aikv::server::ServerMetrics;

    let (exporter, otel) = testutil::init_in_memory();
    let server_metrics = ServerMetrics::default().with_otel(Arc::clone(&otel));
    server_metrics.refresh_process_metrics();
    server_metrics.refresh_process_metrics();
    otel.add_process_cpu_delta(0.001, 0.001);

    assert!(
        testutil::metric_exists(&exporter, "aikv_process_resident_memory_bytes"),
        "expected aikv_process_resident_memory_bytes registered"
    );
    assert!(
        testutil::gauge_value(&exporter, "aikv_process_resident_memory_bytes") > 0.0,
        "expected RSS gauge > 0"
    );
    assert!(
        testutil::metric_exists(&exporter, "process.cpu.time"),
        "expected OTel process.cpu.time counter"
    );
}

#[cfg(feature = "monitoring")]
#[tokio::test]
async fn test_net_bytes_metrics() {
    use std::sync::Arc;

    use aikv::server::otel_metrics::testutil;
    use aikv::server::{ConnectionConfig, Server, ServerSharedState};
    use aikv::storage::{KvStorage, MemoryEngine};

    let (exporter, _otel) = testutil::init_in_memory();
    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    let state = ServerSharedState::new(
        ConnectionConfig {
            read_timeout: None,
            idle_timeout: None,
            max_clients: 0,
            script_worker_threads: 2,
        },
        storage,
        0,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state_clone = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = Server::run_with_listener(listener, state_clone).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"PING"]).await;
    let _ = super::helpers::read_response(&mut stream).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    state.refresh_runtime_metrics().await;

    assert!(
        state.metrics().net_input_bytes() > 0,
        "expected net input bytes on ServerMetrics"
    );
    assert!(
        state.metrics().net_output_bytes() > 0,
        "expected net output bytes on ServerMetrics"
    );

    write_cmd(&mut stream, &[b"INFO", b"stats"]).await;
    let info_resp = super::helpers::read_response(&mut stream).await;
    let info_text = String::from_utf8_lossy(&info_resp);
    assert!(
        info_text.contains("total_net_input_bytes:"),
        "INFO stats missing total_net_input_bytes: {info_text}"
    );
    assert!(
        info_text.contains("total_net_output_bytes:"),
        "INFO stats missing total_net_output_bytes: {info_text}"
    );

    assert!(
        testutil::counter_sum(&exporter, "aikv_net_input_bytes_total") > 0,
        "expected net input bytes in otel"
    );
    assert!(
        testutil::counter_sum(&exporter, "aikv_net_output_bytes_total") > 0,
        "expected net output bytes in otel"
    );
}

#[cfg(feature = "monitoring")]
fn info_field_u64(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        (k == key).then(|| v.parse().ok())?
    })
}

/// 不变式 I1/I2: INFO 与 ServerMetrics atomics 数值一致.
#[cfg(feature = "monitoring")]
#[tokio::test]
async fn info_metrics_consistency_after_commands() {
    use std::sync::Arc;

    use aikv::server::otel_metrics::testutil;
    use aikv::server::{ConnectionConfig, ServerSharedState};
    use aikv::storage::{KvStorage, MemoryEngine, StorageEngineKind, StorageObservation};
    use bytes::Bytes;

    let (exporter, _otel) = testutil::init_in_memory();
    let pre_hits = testutil::counter_sum(&exporter, "aikv_keyspace_hits_total");
    let pre_misses = testutil::counter_sum(&exporter, "aikv_keyspace_misses_total");
    let pre_cmds = testutil::counter_sum(&exporter, "aikv_commands_total");

    fn b(s: &str) -> Bytes {
        Bytes::copy_from_slice(s.as_bytes())
    }

    let observation = StorageObservation::new();
    let storage: Arc<dyn KvStorage> =
        MemoryEngine::with_observation(16, Some(Arc::clone(&observation)));
    let state = ServerSharedState::new_with_backup_dir(
        ConnectionConfig::default(),
        storage,
        6379,
        StorageEngineKind::Memory,
        None,
        None,
        9191,
        "127.0.0.1".into(),
        Some(observation),
    );

    let router = state.router();
    let mut db = 0;
    router
        .execute("GET", &[b("missing")], &mut db)
        .await
        .unwrap();
    router
        .execute("SET", &[b("k"), b("v")], &mut db)
        .await
        .unwrap();
    router.execute("GET", &[b("k")], &mut db).await.unwrap();

    state.refresh_runtime_metrics().await;

    let memory_text = {
        let resp = router
            .execute("INFO", &[b("memory")], &mut db)
            .await
            .unwrap();
        let aikv::protocol::RespValue::BulkString(Some(text)) = resp else {
            panic!("expected bulk string");
        };
        String::from_utf8_lossy(&text).into_owned()
    };
    let stats_text = {
        let resp = router
            .execute("INFO", &[b("stats")], &mut db)
            .await
            .unwrap();
        let aikv::protocol::RespValue::BulkString(Some(text)) = resp else {
            panic!("expected bulk string");
        };
        String::from_utf8_lossy(&text).into_owned()
    };
    let clients_text = {
        let resp = router
            .execute("INFO", &[b("clients")], &mut db)
            .await
            .unwrap();
        let aikv::protocol::RespValue::BulkString(Some(text)) = resp else {
            panic!("expected bulk string");
        };
        String::from_utf8_lossy(&text).into_owned()
    };

    let used_memory = state.metrics().used_memory_bytes();
    assert_eq!(
        info_field_u64(&memory_text, "used_memory"),
        Some(used_memory)
    );

    let hits = state.metrics().keyspace_hits();
    let misses = state.metrics().keyspace_misses();
    assert_eq!(info_field_u64(&stats_text, "keyspace_hits"), Some(hits));
    assert_eq!(info_field_u64(&stats_text, "keyspace_misses"), Some(misses));

    let blocked = state.metrics().blocked_clients() as u64;
    assert_eq!(
        info_field_u64(&clients_text, "blocked_clients"),
        Some(blocked)
    );

    let ops = state.metrics().instantaneous_ops_per_sec();
    assert_eq!(
        info_field_u64(&stats_text, "instantaneous_ops_per_sec"),
        Some(ops)
    );

    state.refresh_runtime_metrics().await;

    assert_eq!(
        testutil::counter_sum(&exporter, "aikv_keyspace_hits_total") - pre_hits,
        hits
    );
    assert_eq!(
        testutil::counter_sum(&exporter, "aikv_keyspace_misses_total") - pre_misses,
        misses
    );
    assert_eq!(
        testutil::counter_sum(&exporter, "aikv_commands_total") - pre_cmds,
        state.metrics().total_commands_processed()
    );
}

/// 热路径不写 OTel. refresh 不再把命令计数推进旧 meter. 导出快照才带这些数.
#[cfg(feature = "monitoring")]
#[tokio::test]
async fn otel_counters_sync_only_on_refresh() {
    use std::sync::Arc;

    use aikv::server::otel_metrics::testutil;
    use aikv::server::{ConnectionConfig, ServerSharedState};
    use aikv::storage::{KvStorage, MemoryEngine};
    use bytes::Bytes;

    let (exporter, _otel) = testutil::init_in_memory();
    let pre_cmds = testutil::counter_sum(&exporter, "aikv_commands_total");
    let pre_hits = testutil::counter_sum(&exporter, "aikv_keyspace_hits_total");

    fn b(s: &str) -> Bytes {
        Bytes::copy_from_slice(s.as_bytes())
    }

    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    let state = ServerSharedState::new(ConnectionConfig::default(), storage, 6379);
    let router = state.router();
    let mut db = 0;

    router
        .execute("SET", &[b("k"), b("v")], &mut db)
        .await
        .unwrap();
    router.execute("GET", &[b("k")], &mut db).await.unwrap();

    assert_eq!(
        testutil::counter_sum(&exporter, "aikv_commands_total"),
        pre_cmds,
        "OTel commands_total must not change before refresh sync"
    );
    assert_eq!(
        testutil::counter_sum(&exporter, "aikv_keyspace_hits_total"),
        pre_hits,
        "OTel keyspace_hits must not change before refresh sync"
    );

    state.refresh_runtime_metrics().await;

    assert_eq!(
        testutil::counter_sum(&exporter, "aikv_commands_total") - pre_cmds,
        0,
        "refresh must not push the old command counter"
    );
    let snap = aikv::server::export::collect_export_snapshot(state.metrics(), 0, &[], None, &[]);
    let commands: u64 = snap
        .counters
        .iter()
        .filter(|sample| sample.name == "aikv_commands_total")
        .map(|sample| sample.value)
        .sum();
    assert_eq!(commands, state.metrics().total_commands_processed());
}

#[tokio::test]
async fn blocked_clients_increments_during_blpop() {
    use bytes::Bytes;
    use std::sync::Arc;

    use aikv::server::{ConnectionConfig, ServerSharedState};
    use aikv::storage::{KvStorage, MemoryEngine};

    fn b(s: &str) -> Bytes {
        Bytes::copy_from_slice(s.as_bytes())
    }

    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    let shared = Arc::new(ServerSharedState::new(
        ConnectionConfig::default(),
        storage,
        6379,
    ));
    let metrics = shared.metrics();
    let router = shared.router();

    assert_eq!(metrics.blocked_clients(), 0);

    let router_wait = shared.router();
    let wait = tokio::spawn(async move {
        let mut db = 0;
        router_wait
            .execute("BLPOP", &[b("q"), b("5")], &mut db)
            .await
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(metrics.blocked_clients(), 1);

    let mut db = 0;
    router
        .execute("LPUSH", &[b("q"), b("v")], &mut db)
        .await
        .unwrap();

    wait.await.unwrap().unwrap();
    assert_eq!(metrics.blocked_clients(), 0);
}

#[tokio::test]
async fn blocked_clients_multi_key_blpop_counts_one_client() {
    use bytes::Bytes;
    use std::sync::Arc;

    use aikv::server::{ConnectionConfig, ServerSharedState};
    use aikv::storage::{KvStorage, MemoryEngine};

    fn b(s: &str) -> Bytes {
        Bytes::copy_from_slice(s.as_bytes())
    }

    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    let shared = Arc::new(ServerSharedState::new(
        ConnectionConfig::default(),
        storage,
        6379,
    ));
    let metrics = shared.metrics();
    let router_wait = shared.router();
    let wait = tokio::spawn(async move {
        let mut db = 0;
        router_wait
            .execute("BLPOP", &[b("a"), b("b"), b("c"), b("5")], &mut db)
            .await
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(metrics.blocked_clients(), 1);

    let router = shared.router();
    let mut db = 0;
    router
        .execute("LPUSH", &[b("b"), b("v")], &mut db)
        .await
        .unwrap();

    wait.await.unwrap().unwrap();
    assert_eq!(metrics.blocked_clients(), 0);
}

#[tokio::test]
async fn blocked_clients_short_timeout_unblocks() {
    use bytes::Bytes;
    use std::sync::Arc;

    use aikv::server::{ConnectionConfig, ServerSharedState};
    use aikv::storage::{KvStorage, MemoryEngine};

    fn b(s: &str) -> Bytes {
        Bytes::copy_from_slice(s.as_bytes())
    }

    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    let shared = ServerSharedState::new(ConnectionConfig::default(), storage, 6379);
    let router = shared.router();
    let mut db = 0;

    router
        .execute("BLPOP", &[b("missing"), b("0.05")], &mut db)
        .await
        .unwrap();
    assert_eq!(shared.metrics().blocked_clients(), 0);
}

/// 契约: 核心 aikv_* 指标名与 observability-reference 一致.
#[cfg(feature = "monitoring")]
#[test]
fn test_metric_catalog_contract() {
    use aikv::server::metrics::ServerMetrics;
    use aikv::server::otel_metrics::{testutil, AIKV_METRIC_NAMES};

    let (exporter, otel) = testutil::init_in_memory();
    let server_metrics = ServerMetrics::default().with_otel(otel);
    server_metrics.on_connect();
    server_metrics.on_command("PING", true);
    server_metrics.on_command_duration("PING", 1000, true);
    server_metrics.on_keyspace_hit();
    server_metrics.set_uptime_secs(1);
    server_metrics.set_db_key_count(0, 0);

    aikv::server::info_catalog::sync_otel_from_server_metrics(&server_metrics);

    for name in AIKV_METRIC_NAMES {
        assert!(
            testutil::metric_exists(&exporter, name),
            "missing otel metric: {name}"
        );
    }
}

struct SpanCapture {
    spans: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    info_events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    fields: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

struct FieldGrab(Vec<String>);

impl tracing::field::Visit for FieldGrab {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{}={value:?}", field.name()));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push(format!("{}={value}", field.name()));
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SpanCapture {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.spans
            .lock()
            .unwrap()
            .push(attrs.metadata().name().to_string());
        let mut grab = FieldGrab(Vec::new());
        attrs.record(&mut grab);
        self.fields.lock().unwrap().extend(grab.0);
    }

    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if *event.metadata().level() == tracing::Level::INFO {
            let mut grab = FieldGrab(Vec::new());
            event.record(&mut grab);
            self.info_events
                .lock()
                .unwrap()
                .push(event.metadata().name().to_string());
            self.fields.lock().unwrap().extend(grab.0);
        }
    }
}

#[allow(clippy::type_complexity)]
fn info_capture() -> (
    SpanCapture,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    let spans = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let info_events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let fields = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    (
        SpanCapture {
            spans: std::sync::Arc::clone(&spans),
            info_events: std::sync::Arc::clone(&info_events),
            fields: std::sync::Arc::clone(&fields),
        },
        spans,
        info_events,
        fields,
    )
}

#[tokio::test]
async fn default_get_and_set_emit_no_info_and_no_span() {
    use aikv::command::CommandRouter;
    use aikv::storage::{KvStorage, MemoryEngine};
    use bytes::Bytes;
    use std::sync::Arc;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::Registry;

    aidb::observability::set_traces_enabled(false);
    let (layer, spans, info_events, _fields) = info_capture();
    let subscriber =
        Registry::default().with(layer.with_filter(tracing_subscriber::filter::LevelFilter::INFO));
    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    let router = CommandRouter::new(storage);
    let mut db = 0;
    let key = Bytes::from_static(b"user-key-should-stay-out");
    let _guard = tracing::subscriber::set_default(subscriber);
    router
        .execute("SET", &[key.clone(), Bytes::from_static(b"v")], &mut db)
        .await
        .unwrap();
    router.execute("GET", &[key], &mut db).await.unwrap();
    drop(_guard);
    assert!(info_events.lock().unwrap().is_empty());
    let spans = spans.lock().unwrap();
    assert!(!spans
        .iter()
        .any(|name| name == "kv_command" || name == "kv_connection"));
    aidb::observability::set_traces_enabled(false);
}

#[tokio::test]
async fn command_span_exists_only_when_traces_are_enabled() {
    use aikv::command::CommandRouter;
    use aikv::storage::{KvStorage, MemoryEngine};
    use bytes::Bytes;
    use std::sync::Arc;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::Registry;

    aidb::observability::set_traces_enabled(true);
    let (layer, spans, _info_events, fields) = info_capture();
    let subscriber =
        Registry::default().with(layer.with_filter(tracing_subscriber::filter::LevelFilter::INFO));
    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    let router = CommandRouter::new(storage);
    let mut db = 0;
    let key = Bytes::from_static(b"user-key-should-stay-out");
    let _guard = tracing::subscriber::set_default(subscriber);
    router.execute("GET", &[key], &mut db).await.unwrap();
    drop(_guard);
    let spans = spans.lock().unwrap();
    assert_eq!(spans.iter().filter(|name| *name == "kv_command").count(), 1);
    let dumped = fields.lock().unwrap().join(" ");
    assert!(!dumped.contains("user-key-should-stay-out"));
    aidb::observability::set_traces_enabled(false);
}
