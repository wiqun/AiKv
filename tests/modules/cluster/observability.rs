//! 集群可观测性 (Phase 3).

#![cfg(feature = "cluster")]

use aikv::server::metrics::ServerMetrics;

#[test]
fn gossip_refresh_increments_cluster_message_stats() {
    let metrics = ServerMetrics::default();
    metrics.on_gossip_refresh(3);
    assert_eq!(metrics.cluster_messages_sent(), 1);
    assert_eq!(metrics.cluster_messages_received(), 3);

    metrics.on_gossip_refresh(5);
    assert_eq!(metrics.cluster_messages_sent(), 2);
    assert_eq!(metrics.cluster_messages_received(), 8);
}

#[test]
fn gossip_refresh_with_zero_nodes_only_counts_sent() {
    let metrics = ServerMetrics::default();
    metrics.on_gossip_refresh(0);
    assert_eq!(metrics.cluster_messages_sent(), 1);
    assert_eq!(metrics.cluster_messages_received(), 0);
}

#[test]
fn blocked_clients_defaults_to_zero() {
    let metrics = ServerMetrics::default();
    assert_eq!(metrics.blocked_clients(), 0);
}

#[cfg(feature = "monitoring")]
#[test]
fn sync_stats_gauges_exports_blocked_clients() {
    use aikv::server::otel_metrics::testutil;

    let (exporter, otel) = testutil::init_in_memory();
    let metrics = ServerMetrics::default().with_otel(otel.clone());
    metrics.on_client_blocked();
    otel.sync_stats_gauges(&metrics);
    assert_eq!(
        testutil::gauge_value(&exporter, "aikv_blocked_clients"),
        1.0
    );
}

/// gossip 拓扑刷新经命令统计差分后镜像到 `aikv_gossip_messages_total` (T8).
#[cfg(feature = "monitoring")]
#[test]
fn sync_counters_mirrors_gossip_messages_total() {
    use aikv::server::otel_metrics::testutil;

    let (exporter, otel) = testutil::init_in_memory();
    let metrics = ServerMetrics::default().with_otel(otel.clone());
    metrics.on_gossip_refresh(3);
    otel.sync_counters(&metrics);
    assert_eq!(
        testutil::counter_sum(&exporter, "aikv_gossip_messages_total"),
        1
    );

    // 第二次 refresh 产生增量, 累计 2.
    metrics.on_gossip_refresh(5);
    otel.sync_counters(&metrics);
    assert_eq!(
        testutil::counter_sum(&exporter, "aikv_gossip_messages_total"),
        2
    );
}

/// failover 事件镜像到 `aikv_failover_total` (T8).
#[cfg(feature = "monitoring")]
#[test]
fn sync_counters_mirrors_failover_total() {
    use aikv::server::otel_metrics::testutil;

    let (exporter, otel) = testutil::init_in_memory();
    let metrics = ServerMetrics::default().with_otel(otel.clone());
    metrics.on_failover();
    metrics.on_failover();
    otel.sync_counters(&metrics);
    assert_eq!(testutil::counter_sum(&exporter, "aikv_failover_total"), 2);
}

/// 重定向只进导出快照 `aikv_redirects_total`. commandstats 不再回放成另一条计数.
#[cfg(feature = "monitoring")]
#[test]
fn redirects_export_from_counters_not_commandstats() {
    use aikv::server::otel_metrics::testutil;

    let (exporter, otel) = testutil::init_in_memory();
    let metrics = ServerMetrics::default().with_otel(otel.clone());
    metrics.on_cluster_redirect("moved");
    metrics.on_cluster_redirect("moved");
    metrics.on_cluster_redirect("ask");
    otel.sync_commandstats(&metrics);
    assert_eq!(
        testutil::counter_sum(&exporter, "aikv_cluster_redirects_total"),
        0
    );
    let snap = aikv::server::export::collect_export_snapshot(&metrics, 0, &[], None, &[]);
    let value = |kind: &str| {
        snap.counters
            .iter()
            .find(|sample| {
                sample.name == "aikv_redirects_total"
                    && sample
                        .labels
                        .iter()
                        .any(|(key, value)| key == "type" && value == kind)
            })
            .map(|sample| sample.value)
    };
    assert_eq!(value("moved"), Some(2));
    assert_eq!(value("ask"), Some(1));
}
