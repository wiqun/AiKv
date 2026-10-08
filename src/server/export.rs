//! 同一份累计快照同时渲染 `/metrics` 并按增量推送.
//! 计数器每次只加差值一次. 直方图一次交出整份累计快照, 不按桶中点重放.

use std::sync::RwLock;

use aidb::statistics::{render_histogram, LatencySnapshot};

static LATEST_METRICS: RwLock<String> = RwLock::new(String::new());

#[derive(Debug, Clone, PartialEq)]
pub struct CounterSample {
    pub name: String,
    pub labels: Vec<(String, String)>,
    pub value: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GaugeSample {
    pub name: String,
    pub labels: Vec<(String, String)>,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistogramSample {
    pub name: String,
    pub labels: Vec<(String, String)>,
    pub snap: LatencySnapshot,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportSnapshot {
    pub counters: Vec<CounterSample>,
    pub gauges: Vec<GaugeSample>,
    pub histograms: Vec<HistogramSample>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportConfig {
    pub otlp_enabled: bool,
}

/// 端点为空则不初始化 OTLP. `/metrics` 仍然由调用方提供.
pub fn export_config(endpoint: Option<&str>) -> ExportConfig {
    ExportConfig {
        otlp_enabled: endpoint.is_some_and(|s| !s.is_empty()),
    }
}

pub trait ExportSink {
    fn add_counter(&mut self, name: &str, labels: &[(&str, &str)], delta: u64);
    fn set_gauge(&mut self, name: &str, labels: &[(&str, &str)], value: f64);
    fn set_histogram(&mut self, name: &str, labels: &[(&str, &str)], snap: &LatencySnapshot);
}

/// 用这一轮已经读完的快照渲染文本, 并按相对上一轮的差值推送.
pub fn publish_round(
    current: &ExportSnapshot,
    baseline: &ExportSnapshot,
    sink: &mut dyn ExportSink,
) {
    publish_metrics_text(render_prometheus(current));
    push_deltas(current, baseline, sink);
}

pub fn publish_metrics_text(text: String) {
    *LATEST_METRICS.write().unwrap_or_else(|e| e.into_inner()) = text;
}

pub fn metrics_text() -> String {
    LATEST_METRICS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub fn render_prometheus(snapshot: &ExportSnapshot) -> String {
    let mut out = String::new();
    for counter in &snapshot.counters {
        out.push_str(&format!(
            "{}{} {}\n",
            counter.name,
            format_labels(&counter.labels),
            counter.value
        ));
    }
    for gauge in &snapshot.gauges {
        out.push_str(&format!(
            "{}{} {}\n",
            gauge.name,
            format_labels(&gauge.labels),
            gauge.value
        ));
    }
    for histogram in &snapshot.histograms {
        let labels = labels_inside(&histogram.labels);
        out.push_str(&render_histogram(&histogram.name, &labels, &histogram.snap));
    }
    out
}

pub fn push_deltas(current: &ExportSnapshot, baseline: &ExportSnapshot, sink: &mut dyn ExportSink) {
    for counter in &current.counters {
        let previous = baseline
            .counters
            .iter()
            .find(|old| old.name == counter.name && old.labels == counter.labels)
            .map(|old| old.value)
            .unwrap_or(0);
        let delta = counter.value.saturating_sub(previous);
        if delta == 0 {
            continue;
        }
        let labels = label_refs(&counter.labels);
        sink.add_counter(&counter.name, &labels, delta);
    }
    for gauge in &current.gauges {
        let labels = label_refs(&gauge.labels);
        sink.set_gauge(&gauge.name, &labels, gauge.value);
    }
    for histogram in &current.histograms {
        let labels = label_refs(&histogram.labels);
        sink.set_histogram(&histogram.name, &labels, &histogram.snap);
    }
}

/// 连接, 进程和引擎样本. 不扫描数据库, 也不带 `instantaneous_ops`, `evicted_keys`,
/// `used_memory_lua`, `avg_ttl`. 未启用 Raft 时样本没有 `group`.
pub fn collect_export_snapshot(
    metrics: &crate::server::metrics::ServerMetrics,
    keys_estimate: u64,
    expires: &[(usize, u64)],
    engine: Option<&aidb::Statistics>,
    raft: &[(String, RaftExportView)],
) -> ExportSnapshot {
    let mut snapshot = ExportSnapshot::default();
    append_process(&mut snapshot, metrics, keys_estimate, expires);
    append_commands(&mut snapshot, metrics);
    append_redirects(&mut snapshot, metrics);
    for (group, view) in raft {
        let part = raft_samples(group, view);
        snapshot.gauges.extend(part.gauges);
        snapshot.counters.extend(part.counters);
        snapshot.histograms.extend(part.histograms);
    }
    if let Some(stats) = engine {
        append_engine(&mut snapshot, stats);
    }
    snapshot
}

fn append_process(
    snapshot: &mut ExportSnapshot,
    metrics: &crate::server::metrics::ServerMetrics,
    keys_estimate: u64,
    expires: &[(usize, u64)],
) {
    snapshot.gauges.push(GaugeSample {
        name: "aikv_connections_current".into(),
        labels: vec![],
        value: metrics.connected_clients() as f64,
    });
    push_counter(
        snapshot,
        "aikv_connections_rejected_total",
        vec![],
        metrics.rejected_connections(),
    );
    push_counter(
        snapshot,
        "aikv_net_input_bytes_total",
        vec![],
        metrics.net_input_bytes(),
    );
    push_counter(
        snapshot,
        "aikv_net_output_bytes_total",
        vec![],
        metrics.net_output_bytes(),
    );
    push_counter(
        snapshot,
        "aikv_scan_entries_total",
        vec![],
        metrics.scan_entries(),
    );
    push_counter(
        snapshot,
        "aikv_expired_keys_total",
        vec![],
        metrics.expired_keys(),
    );
    push_counter(
        snapshot,
        "aikv_slowlog_total",
        vec![],
        metrics.slowlog_commands_count(),
    );
    push_counter(
        snapshot,
        "aikv_point_reads_total",
        vec![("result".into(), "hit".into())],
        metrics.point_read_count(true),
    );
    push_counter(
        snapshot,
        "aikv_point_reads_total",
        vec![("result".into(), "miss".into())],
        metrics.point_read_count(false),
    );
    snapshot.gauges.push(GaugeSample {
        name: "aikv_process_up".into(),
        labels: vec![],
        value: 1.0,
    });
    snapshot.gauges.push(GaugeSample {
        name: "aikv_rss_bytes".into(),
        labels: vec![],
        value: metrics.cached_rss_bytes() as f64,
    });
    snapshot.gauges.push(GaugeSample {
        name: "aikv_keys_estimate".into(),
        labels: vec![],
        value: keys_estimate as f64,
    });
    for (db, count) in expires {
        snapshot.gauges.push(GaugeSample {
            name: "aikv_expires".into(),
            labels: vec![("db".into(), db.to_string())],
            value: *count as f64,
        });
    }
    for (reason, count) in metrics.connection_close_rows() {
        push_counter(
            snapshot,
            "aikv_connection_closes_total",
            vec![("reason".into(), reason)],
            count,
        );
    }
    for (errno, count) in metrics.accept_error_rows() {
        push_counter(
            snapshot,
            "aikv_accept_errors_total",
            vec![("errno".into(), errno)],
            count,
        );
    }
}

fn append_commands(snapshot: &mut ExportSnapshot, metrics: &crate::server::metrics::ServerMetrics) {
    for (command, ok, err) in metrics.command_counter_rows() {
        push_counter(
            snapshot,
            "aikv_commands_total",
            vec![
                ("command".into(), command.clone()),
                ("status".into(), "ok".into()),
            ],
            ok,
        );
        push_counter(
            snapshot,
            "aikv_commands_total",
            vec![
                ("command".into(), command),
                ("status".into(), "error".into()),
            ],
            err,
        );
    }
    for (command, ok, snap) in metrics.command_duration_rows() {
        snapshot.histograms.push(HistogramSample {
            name: "aikv_command_duration_seconds".into(),
            labels: vec![
                ("command".into(), command),
                ("status".into(), if ok { "ok" } else { "error" }.into()),
            ],
            snap,
        });
    }
}

fn append_redirects(
    snapshot: &mut ExportSnapshot,
    metrics: &crate::server::metrics::ServerMetrics,
) {
    use crate::server::metrics::RedirectKind;
    push_counter(
        snapshot,
        "aikv_redirects_total",
        vec![("type".into(), "moved".into())],
        metrics.redirect_count(RedirectKind::Moved),
    );
    push_counter(
        snapshot,
        "aikv_redirects_total",
        vec![("type".into(), "ask".into())],
        metrics.redirect_count(RedirectKind::Ask),
    );
    for (group, snap) in metrics.propose_rows() {
        snapshot.histograms.push(HistogramSample {
            name: "aidb_propose_duration_seconds".into(),
            labels: vec![("group".into(), group.to_string())],
            snap,
        });
    }
}

fn push_counter(
    snapshot: &mut ExportSnapshot,
    name: &str,
    labels: Vec<(String, String)>,
    value: u64,
) {
    snapshot.counters.push(CounterSample {
        name: name.into(),
        labels,
        value,
    });
}

fn append_engine(snapshot: &mut ExportSnapshot, stats: &aidb::Statistics) {
    use std::sync::atomic::Ordering;
    let current = stats.snapshot();
    snapshot
        .gauges
        .push(gauge("aidb_wal_size_bytes", vec![], current.wal_size_bytes));
    snapshot.gauges.push(gauge(
        "aidb_memtable_active_bytes",
        vec![],
        current.memtable_size_bytes[0],
    ));
    snapshot.gauges.push(gauge(
        "aidb_memtable_frozen_bytes",
        vec![],
        current.memtable_size_bytes[1],
    ));
    snapshot.gauges.push(gauge(
        "aidb_memtable_limit_bytes",
        vec![],
        stats.memtable_limit_bytes.load(Ordering::Relaxed),
    ));
    snapshot.gauges.push(gauge(
        "aidb_l0_files",
        vec![],
        current.sstable_count.first().copied().unwrap_or(0),
    ));
    snapshot.gauges.push(gauge(
        "aidb_l0_compaction_trigger",
        vec![],
        stats.l0_compaction_trigger.load(Ordering::Relaxed),
    ));
    snapshot.gauges.push(gauge(
        "aidb_l0_slowdown_writes_trigger",
        vec![],
        stats.l0_slowdown_writes_trigger.load(Ordering::Relaxed),
    ));
    snapshot.gauges.push(gauge(
        "aidb_l0_stop_writes_trigger",
        vec![],
        stats.l0_stop_writes_trigger.load(Ordering::Relaxed),
    ));
    for (level, size) in current.sstable_size_bytes.iter().enumerate() {
        snapshot.gauges.push(GaugeSample {
            name: "aidb_sstable_size_bytes".into(),
            labels: vec![("level".into(), level.to_string())],
            value: *size as f64,
        });
    }
    append_engine_counters(snapshot, stats, &current);
}

fn append_engine_counters(
    snapshot: &mut ExportSnapshot,
    stats: &aidb::Statistics,
    current: &aidb::statistics::StatsSnapshot,
) {
    use std::sync::atomic::Ordering;
    push_counter(
        snapshot,
        "aidb_wal_written_bytes_total",
        vec![],
        current.wal_written_bytes,
    );
    push_counter(
        snapshot,
        "aidb_flush_written_bytes_total",
        vec![],
        current.flush_written_bytes,
    );
    push_counter(
        snapshot,
        "aidb_compaction_written_bytes_total",
        vec![],
        current.compaction_written_bytes,
    );
    push_counter(
        snapshot,
        "aidb_compaction_read_bytes_total",
        vec![],
        current.compaction_read_bytes,
    );
    push_counter(
        snapshot,
        "aidb_block_read_bytes_total",
        vec![],
        current.block_read_bytes,
    );
    push_counter(
        snapshot,
        "aidb_block_seek_count_total",
        vec![],
        current.block_seek_count,
    );
    push_counter(
        snapshot,
        "aidb_logical_read_count_total",
        vec![],
        current.logical_read_count,
    );
    push_counter(
        snapshot,
        "aidb_logical_read_bytes_total",
        vec![],
        current.logical_read_bytes,
    );
    push_counter(
        snapshot,
        "aidb_block_cache_hits_total",
        vec![],
        current.block_cache_hits,
    );
    push_counter(
        snapshot,
        "aidb_block_cache_misses_total",
        vec![],
        current.block_cache_misses,
    );
    push_counter(
        snapshot,
        "aidb_bloom_false_positive_total",
        vec![],
        current.bloom_false_positive,
    );
    push_counter(
        snapshot,
        "aidb_bloom_useful_total",
        vec![],
        current.bloom_useful,
    );
    snapshot.gauges.push(gauge(
        "aidb_block_cache_size_bytes",
        vec![],
        current.block_cache_size,
    ));
    snapshot.gauges.push(gauge(
        "aidb_block_cache_capacity_bytes",
        vec![],
        current.block_cache_capacity,
    ));
    snapshot.gauges.push(gauge(
        "aidb_compaction_pending_bytes",
        vec![],
        current.compaction_pending_bytes,
    ));
    snapshot.gauges.push(GaugeSample {
        name: "aidb_flush_last_duration_seconds".into(),
        labels: vec![],
        value: stats.flush_last_duration_us.load(Ordering::Relaxed) as f64 / 1_000_000.0,
    });
    snapshot.gauges.push(GaugeSample {
        name: "aidb_compaction_last_duration_seconds".into(),
        labels: vec![],
        value: stats.compaction_last_duration_us.load(Ordering::Relaxed) as f64 / 1_000_000.0,
    });
    snapshot.gauges.push(GaugeSample {
        name: "aidb_write_stall_window_max_us".into(),
        labels: vec![],
        value: stats.take_write_stall_window_max_us() as f64,
    });
    snapshot.histograms.push(HistogramSample {
        name: "aidb_engine_write_duration_seconds".into(),
        labels: vec![],
        snap: stats.engine_write_duration.snapshot(),
    });
    snapshot.gauges.push(GaugeSample {
        name: "aidb_recovery_wal_replay_duration_seconds".into(),
        labels: vec![],
        value: current.recovery_wal_replay_duration_us as f64 / 1_000_000.0,
    });
    for kind in aidb::statistics::WriteStallKind::ALL {
        let (cause, stall_kind) = kind.cause_and_type();
        push_counter(
            snapshot,
            "aidb_write_stall_total",
            vec![
                ("cause".into(), cause.into()),
                ("kind".into(), stall_kind.into()),
            ],
            current.write_stall_requests[kind as usize],
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RaftExportView {
    pub commit_index: u64,
    pub applied_index: u64,
    pub last_log_index: u64,
    pub min_voter_matched: u64,
    pub is_leader: bool,
    pub leader_contact_age_secs: f64,
    pub role: &'static str,
}

/// leader 的接触年龄是 0. follower 用已经听到的年龄, 没听到过就用调用方给的未接触秒数.
pub fn contact_age_for_export(is_leader: bool, heard: Option<f64>, unheard_secs: f64) -> f64 {
    if is_leader {
        0.0
    } else {
        heard.unwrap_or(unheard_secs)
    }
}

/// 导出时用这份视图算滞后. 不在请求路径上做减法.
pub fn raft_samples(group: &str, view: &RaftExportView) -> ExportSnapshot {
    let mut snapshot = ExportSnapshot::default();
    let group_label = vec![("group".into(), group.into())];
    snapshot.gauges.push(GaugeSample {
        name: "aidb_raft_apply_lag_entries".into(),
        labels: group_label.clone(),
        value: view.commit_index.saturating_sub(view.applied_index) as f64,
    });
    let replication = if view.is_leader {
        view.last_log_index.saturating_sub(view.min_voter_matched)
    } else {
        0
    };
    snapshot.gauges.push(GaugeSample {
        name: "aidb_raft_replication_lag_entries".into(),
        labels: group_label.clone(),
        value: replication as f64,
    });
    let contact = if view.is_leader {
        0.0
    } else {
        view.leader_contact_age_secs
    };
    snapshot.gauges.push(GaugeSample {
        name: "aidb_raft_leader_contact_age_seconds".into(),
        labels: group_label.clone(),
        value: contact,
    });
    let mut role_labels = group_label;
    role_labels.push(("role".into(), view.role.into()));
    snapshot.gauges.push(GaugeSample {
        name: "aidb_raft_role".into(),
        labels: role_labels,
        value: 1.0,
    });
    snapshot
}

fn gauge(name: &str, labels: Vec<(String, String)>, value: u64) -> GaugeSample {
    GaugeSample {
        name: name.into(),
        labels,
        value: value as f64,
    }
}

fn format_labels(labels: &[(String, String)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    format!("{{{}}}", labels_inside(labels))
}

fn labels_inside(labels: &[(String, String)]) -> String {
    labels
        .iter()
        .map(|(k, v)| format!("{k}=\"{v}\""))
        .collect::<Vec<_>>()
        .join(",")
}

/// monitoring 下计数器一次 `add(delta)`. 直方图一次交出累计桶, 不循环 `record`.
#[cfg(feature = "monitoring")]
#[derive(Default)]
pub struct OtelSink {
    histograms: Vec<HistogramSample>,
}

#[cfg(feature = "monitoring")]
impl OtelSink {
    pub async fn flush_histograms(&self) {
        export_histogram_batch(&self.histograms).await;
    }
}

#[cfg(feature = "monitoring")]
impl ExportSink for OtelSink {
    fn add_counter(&mut self, name: &str, labels: &[(&str, &str)], delta: u64) {
        let meter = opentelemetry::global::meter("aikv-export");
        let counter = meter.u64_counter(name.to_string()).build();
        counter.add(delta, &key_values(labels));
    }

    fn set_gauge(&mut self, name: &str, labels: &[(&str, &str)], value: f64) {
        let meter = opentelemetry::global::meter("aikv-export");
        let gauge = meter.f64_gauge(name.to_string()).build();
        gauge.record(value, &key_values(labels));
    }

    fn set_histogram(&mut self, name: &str, labels: &[(&str, &str)], snap: &LatencySnapshot) {
        if snap.buckets.iter().all(|count| *count == 0) {
            return;
        }
        self.histograms.push(HistogramSample {
            name: name.to_string(),
            labels: labels
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
            snap: *snap,
        });
    }
}

#[cfg(feature = "monitoring")]
fn export_attributes(
    labels: &[(String, String)],
) -> Vec<opentelemetry_proto::tonic::common::v1::KeyValue> {
    labels
        .iter()
        .map(|(key, value)| proto_string_kv(key, value))
        .collect()
}

#[cfg(feature = "monitoring")]
fn proto_string_kv(key: &str, value: &str) -> opentelemetry_proto::tonic::common::v1::KeyValue {
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(Value::StringValue(value.to_string())),
        }),
        key_strindex: 0,
    }
}

#[cfg(feature = "monitoring")]
fn unix_nanos(time: std::time::SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// 一条累计直方图. `bucket_counts` 是各桶自己的次数, 不是前缀和.
#[cfg(feature = "monitoring")]
pub(crate) fn histogram_data_point(
    labels: &[(String, String)],
    snap: &LatencySnapshot,
    start: std::time::SystemTime,
    now: std::time::SystemTime,
) -> Option<opentelemetry_proto::tonic::metrics::v1::HistogramDataPoint> {
    use aidb::statistics::COMMAND_DURATION_BOUNDS_US;
    let bucket_counts = snap.buckets.to_vec();
    let count: u64 = bucket_counts.iter().copied().sum();
    if count == 0 {
        return None;
    }
    let explicit_bounds = COMMAND_DURATION_BOUNDS_US
        .iter()
        .map(|bound_us| *bound_us as f64 / 1_000_000.0)
        .collect();
    Some(
        opentelemetry_proto::tonic::metrics::v1::HistogramDataPoint {
            attributes: export_attributes(labels),
            start_time_unix_nano: unix_nanos(start),
            time_unix_nano: unix_nanos(now),
            count,
            sum: Some(snap.sum_us as f64 / 1_000_000.0),
            bucket_counts,
            explicit_bounds,
            exemplars: Vec::new(),
            flags: 0,
            min: None,
            max: None,
        },
    )
}

#[cfg(feature = "monitoring")]
fn histogram_request(
    resource: &opentelemetry_sdk::Resource,
    samples: &[HistogramSample],
    start: std::time::SystemTime,
    now: std::time::SystemTime,
) -> opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest {
    use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
    use opentelemetry_proto::tonic::common::v1::InstrumentationScope;
    use opentelemetry_proto::tonic::metrics::v1::metric::Data;
    use opentelemetry_proto::tonic::metrics::v1::{
        AggregationTemporality, Histogram, Metric, ResourceMetrics, ScopeMetrics,
    };
    use opentelemetry_proto::tonic::resource::v1::Resource as ProtoResource;
    use std::collections::BTreeMap;

    let mut by_name: BTreeMap<
        &str,
        Vec<opentelemetry_proto::tonic::metrics::v1::HistogramDataPoint>,
    > = BTreeMap::new();
    for sample in samples {
        if let Some(point) = histogram_data_point(&sample.labels, &sample.snap, start, now) {
            by_name.entry(sample.name.as_str()).or_default().push(point);
        }
    }
    let metrics = by_name
        .into_iter()
        .map(|(name, data_points)| Metric {
            name: name.to_string(),
            description: String::new(),
            unit: String::new(),
            metadata: Vec::new(),
            data: Some(Data::Histogram(Histogram {
                data_points,
                aggregation_temporality: AggregationTemporality::Cumulative as i32,
            })),
        })
        .collect();
    let attributes = resource
        .iter()
        .map(|(key, value)| proto_string_kv(key.as_str(), &value.to_string()))
        .collect();
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Some(ProtoResource {
                attributes,
                dropped_attributes_count: 0,
                entity_refs: Vec::new(),
            }),
            scope_metrics: vec![ScopeMetrics {
                scope: Some(InstrumentationScope {
                    name: "aikv-export".to_string(),
                    version: String::new(),
                    attributes: Vec::new(),
                    dropped_attributes_count: 0,
                }),
                metrics,
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    }
}

#[cfg(feature = "monitoring")]
async fn export_histogram_batch(samples: &[HistogramSample]) {
    use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_client::MetricsServiceClient;
    use std::time::SystemTime;
    use tonic::transport::Channel;

    if samples.is_empty() {
        return;
    }
    let Some((endpoint, resource)) = crate::server::otel::histogram_export_context() else {
        return;
    };
    static STARTED: std::sync::OnceLock<SystemTime> = std::sync::OnceLock::new();
    let start = *STARTED.get_or_init(SystemTime::now);
    let request = histogram_request(resource, samples, start, SystemTime::now());
    let channel = match Channel::from_shared(endpoint.to_string()) {
        Ok(endpoint) => endpoint
            .connect_timeout(std::time::Duration::from_secs(5))
            .connect_lazy(),
        Err(err) => {
            eprintln!("warn: histogram export channel failed: {err}");
            return;
        }
    };
    if let Err(err) = MetricsServiceClient::new(channel).export(request).await {
        eprintln!("warn: histogram export failed: {err}");
    }
}

#[cfg(feature = "monitoring")]
fn key_values(labels: &[(&str, &str)]) -> Vec<opentelemetry::KeyValue> {
    labels
        .iter()
        .map(|(k, v)| opentelemetry::KeyValue::new(k.to_string(), v.to_string()))
        .collect()
}

fn label_refs(labels: &[(String, String)]) -> Vec<(&str, &str)> {
    labels
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{export_config, push_deltas, CounterSample, ExportSnapshot, HistogramSample};
    use aidb::statistics::LatencyHistogram;
    use aidb::statistics::LatencySnapshot;
    use std::cell::RefCell;

    struct RecordingSink {
        adds: RefCell<Vec<u64>>,
        histogram_sets: RefCell<u64>,
        record_calls: u64,
    }

    impl super::ExportSink for RecordingSink {
        fn add_counter(&mut self, _name: &str, _labels: &[(&str, &str)], delta: u64) {
            self.adds.borrow_mut().push(delta);
        }
        fn set_gauge(&mut self, _name: &str, _labels: &[(&str, &str)], _value: f64) {}
        fn set_histogram(
            &mut self,
            _name: &str,
            _labels: &[(&str, &str)],
            _snap: &LatencySnapshot,
        ) {
            *self.histogram_sets.borrow_mut() += 1;
        }
    }

    fn gauge_value(snap: &super::ExportSnapshot, name: &str) -> f64 {
        snap.gauges
            .iter()
            .find(|g| g.name == name)
            .map(|g| g.value)
            .unwrap_or(f64::NAN)
    }

    #[test]
    fn contact_age_for_export_is_zero_on_the_leader() {
        use super::contact_age_for_export;
        assert_eq!(contact_age_for_export(true, Some(4.0), 9.0), 0.0);
        assert_eq!(contact_age_for_export(false, None, 9.0), 9.0);
        assert_eq!(contact_age_for_export(false, Some(2.5), 9.0), 2.5);
    }

    #[test]
    fn raft_lag_uses_the_view() {
        use super::{raft_samples, RaftExportView};
        let leader = RaftExportView {
            commit_index: 10,
            applied_index: 7,
            last_log_index: 10,
            min_voter_matched: 6,
            is_leader: true,
            leader_contact_age_secs: 4.0,
            role: "leader",
        };
        let snap = raft_samples("g1", &leader);
        assert_eq!(gauge_value(&snap, "aidb_raft_apply_lag_entries"), 3.0);
        assert_eq!(gauge_value(&snap, "aidb_raft_replication_lag_entries"), 4.0);
        assert_eq!(
            gauge_value(&snap, "aidb_raft_leader_contact_age_seconds"),
            0.0
        );
        let role = snap
            .gauges
            .iter()
            .find(|g| g.name == "aidb_raft_role")
            .unwrap();
        assert_eq!(role.value, 1.0);
        assert!(role
            .labels
            .iter()
            .any(|(k, v)| k == "role" && v == "leader"));
        let follower = RaftExportView {
            is_leader: false,
            role: "follower",
            ..leader
        };
        let snap = raft_samples("g1", &follower);
        assert_eq!(gauge_value(&snap, "aidb_raft_replication_lag_entries"), 0.0);
        assert_eq!(
            gauge_value(&snap, "aidb_raft_leader_contact_age_seconds"),
            4.0
        );
    }

    #[test]
    fn redirect_counter_does_not_invent_a_command() {
        use crate::server::metrics::{RedirectKind, ServerMetrics};
        let metrics = ServerMetrics::default();
        metrics.on_cluster_redirect("moved");
        assert_eq!(metrics.redirect_count(RedirectKind::Moved), 1);
        assert!(metrics
            .command_counter_rows()
            .iter()
            .all(|(name, _, _)| name != "CLUSTER.REDIRECT.MOVED"));
    }

    #[test]
    fn redirect_ignores_gossip() {
        use crate::server::metrics::{RedirectKind, ServerMetrics};
        let metrics = ServerMetrics::default();
        metrics.on_gossip_refresh(2);
        metrics.on_redirect(RedirectKind::Moved);
        assert_eq!(metrics.redirect_count(RedirectKind::Moved), 1);
        assert_eq!(metrics.redirect_count(RedirectKind::Ask), 0);
        let snap = super::collect_export_snapshot(&metrics, 0, &[], None, &[]);
        let types: Vec<_> = snap
            .counters
            .iter()
            .filter(|c| c.name == "aikv_redirects_total")
            .map(|c| {
                c.labels
                    .iter()
                    .find(|(k, _)| k == "type")
                    .unwrap()
                    .1
                    .clone()
            })
            .collect();
        assert_eq!(types, vec!["moved".to_string(), "ask".to_string()]);
    }

    #[test]
    fn metrics_text_without_raft_has_no_group_label() {
        use crate::server::metrics::ServerMetrics;
        let metrics = ServerMetrics::default();
        let text =
            super::render_prometheus(&super::collect_export_snapshot(&metrics, 0, &[], None, &[]));
        assert!(!text.contains("aidb_raft_"), "{text}");
        assert!(!text.contains("group="), "{text}");
    }

    #[test]
    fn propose_duration_records_only_on_the_write_path() {
        use crate::server::metrics::ServerMetrics;
        let metrics = ServerMetrics::default();
        metrics.note_propose(20, false);
        assert_eq!(metrics.propose_count(), 0);
        metrics.note_propose(20, true);
        assert_eq!(metrics.propose_count(), 1);
    }

    #[test]
    fn raft_view_enters_the_snapshot() {
        use super::{collect_export_snapshot, render_prometheus, RaftExportView};
        use crate::server::metrics::ServerMetrics;
        let view = RaftExportView {
            commit_index: 10,
            applied_index: 7,
            last_log_index: 10,
            min_voter_matched: 6,
            is_leader: true,
            leader_contact_age_secs: 4.0,
            role: "leader",
        };
        let text = render_prometheus(&collect_export_snapshot(
            &ServerMetrics::default(),
            0,
            &[],
            None,
            &[("g1".into(), view)],
        ));
        assert!(
            text.contains("aidb_raft_apply_lag_entries{group=\"g1\"} 3"),
            "{text}"
        );
        assert!(
            text.contains("aidb_raft_replication_lag_entries{group=\"g1\"} 4"),
            "{text}"
        );
        assert!(
            text.contains("aidb_raft_leader_contact_age_seconds{group=\"g1\"} 0"),
            "{text}"
        );
        assert!(
            text.contains("aidb_raft_role{group=\"g1\",role=\"leader\"} 1"),
            "{text}"
        );
    }

    #[test]
    fn connections_current_follows_the_live_value() {
        use super::{collect_export_snapshot, push_deltas};
        use crate::server::metrics::ServerMetrics;
        let metrics = ServerMetrics::default();
        for _ in 0..10 {
            metrics.on_connect();
        }
        let high = collect_export_snapshot(&metrics, 0, &[], None, &[]);
        for _ in 0..3 {
            metrics.on_disconnect();
        }
        let low = collect_export_snapshot(&metrics, 0, &[], None, &[]);
        let mut sink = GaugeSink { value: None };
        push_deltas(&low, &high, &mut sink);
        assert_eq!(sink.value, Some(7.0));
    }

    struct GaugeSink {
        value: Option<f64>,
    }

    impl super::ExportSink for GaugeSink {
        fn add_counter(&mut self, _name: &str, _labels: &[(&str, &str)], _delta: u64) {}
        fn set_gauge(&mut self, name: &str, _labels: &[(&str, &str)], value: f64) {
            if name == "aikv_connections_current" {
                self.value = Some(value);
            }
        }
        fn set_histogram(&mut self, _: &str, _: &[(&str, &str)], _: &LatencySnapshot) {}
    }

    #[test]
    fn scan_entries_added_once() {
        use crate::server::metrics::ServerMetrics;
        let metrics = ServerMetrics::default();
        metrics.note_scan_entries(3);
        let snap = super::collect_export_snapshot(&metrics, 0, &[], None, &[]);
        let text = super::render_prometheus(&snap);
        assert!(text.contains("aikv_scan_entries_total 3\n"), "{text}");
        let scan_samples = snap
            .counters
            .iter()
            .filter(|c| c.name == "aikv_scan_entries_total")
            .count();
        assert_eq!(scan_samples, 1);
        for forbidden in [
            "instantaneous_ops",
            "evicted_keys",
            "used_memory_lua",
            "avg_ttl",
            "aidb_backup_",
            "group=",
        ] {
            assert!(!text.contains(forbidden), "{forbidden} in {text}");
        }
    }

    #[test]
    fn push_deltas_adds_each_counter_once() {
        let baseline = ExportSnapshot {
            counters: vec![CounterSample {
                name: "aikv_commands_total".into(),
                labels: vec![
                    ("command".into(), "GET".into()),
                    ("status".into(), "ok".into()),
                ],
                value: 2,
            }],
            ..ExportSnapshot::default()
        };
        let current = ExportSnapshot {
            counters: vec![CounterSample {
                name: "aikv_commands_total".into(),
                labels: vec![
                    ("command".into(), "GET".into()),
                    ("status".into(), "ok".into()),
                ],
                value: 5,
            }],
            ..ExportSnapshot::default()
        };
        let mut sink = RecordingSink {
            adds: RefCell::new(Vec::new()),
            histogram_sets: RefCell::new(0),
            record_calls: 0,
        };
        push_deltas(&current, &baseline, &mut sink);
        assert_eq!(sink.adds.borrow().as_slice(), &[3]);
        assert_eq!(sink.record_calls, 0);
    }

    #[cfg(feature = "monitoring")]
    #[test]
    fn histogram_otlp_point_keeps_real_buckets() {
        use super::histogram_data_point;
        use std::time::{Duration, UNIX_EPOCH};
        let histogram = LatencyHistogram::default();
        histogram.record(150);
        histogram.record(3_000);
        let snap = histogram.snapshot();
        let point = histogram_data_point(
            &[
                ("command".into(), "GET".into()),
                ("status".into(), "ok".into()),
            ],
            &snap,
            UNIX_EPOCH,
            UNIX_EPOCH + Duration::from_secs(1),
        )
        .expect("point");
        assert_eq!(point.count, 2);
        assert!((point.sum.unwrap() - 0.00315).abs() < 1e-12);
        assert_eq!(point.explicit_bounds.len() + 1, point.bucket_counts.len());
        assert_eq!(point.bucket_counts.iter().sum::<u64>(), 2);
        assert_eq!(point.bucket_counts[1], 1);
        assert_eq!(point.bucket_counts[5], 1);
        assert_eq!(point.explicit_bounds[0], 0.0001);
        assert!(point.attributes.iter().any(|attr| attr.key == "command"));
    }

    #[test]
    fn histogram_push_is_one_snapshot() {
        let histogram = LatencyHistogram::default();
        histogram.record(150);
        histogram.record(3_000);
        let snap = histogram.snapshot();
        let current = ExportSnapshot {
            histograms: vec![HistogramSample {
                name: "aikv_command_duration_seconds".into(),
                labels: vec![],
                snap,
            }],
            ..ExportSnapshot::default()
        };
        let mut sink = RecordingSink {
            adds: RefCell::new(Vec::new()),
            histogram_sets: RefCell::new(0),
            record_calls: 0,
        };
        push_deltas(&current, &ExportSnapshot::default(), &mut sink);
        assert_eq!(*sink.histogram_sets.borrow(), 1);
        assert_eq!(sink.record_calls, 0);
        let pushed = &current.histograms[0].snap;
        assert_eq!(pushed.count, 2);
        assert_eq!(pushed.sum_us, 3_150);
    }

    #[test]
    fn empty_otlp_endpoint_skips_init() {
        let cfg = export_config(None);
        assert!(!cfg.otlp_enabled);
    }
}
