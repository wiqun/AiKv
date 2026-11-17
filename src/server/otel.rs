//! OTel traces + metrics + logs 初始化 (monitoring feature).

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use opentelemetry::global;
use opentelemetry::propagation::{Extractor, Injector};
use opentelemetry::{Context, KeyValue};
use opentelemetry_otlp::{WithExportConfig, WithTonicConfig};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::reader::MetricReader;
use opentelemetry_sdk::metrics::{
    Aggregation, InstrumentKind, ManualReader, Pipeline, SdkMeterProvider, Temporality,
};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};

use crate::config::ResolvedObservability;

static TRACER_PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();
static METER_PROVIDER: OnceLock<SdkMeterProvider> = OnceLock::new();
static LOGGER_PROVIDER: OnceLock<SdkLoggerProvider> = OnceLock::new();
static EXPORT_READER: OnceLock<Arc<ManualReader>> = OnceLock::new();
static METRIC_EXPORTER: OnceLock<opentelemetry_otlp::MetricExporter> = OnceLock::new();

/// OTel 初始化参数 (Resource 标签与导出凭据).
#[derive(Debug, Clone)]
pub struct OtelInitConfig {
    pub endpoint: String,
    pub service_name: String,
    pub host_label: Option<String>,
    pub node_id: Option<u64>,
    pub tcp_port: u16,
    pub deployment_environment: Option<String>,
    pub extra_resource_attrs: Vec<KeyValue>,
    pub otlp_headers: BTreeMap<String, String>,
    /// Trace 采样率 (0.0-1.0), 默认 0.0.
    pub sample_ratio: f64,
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn resolve_node_id(env_node_id: Option<&str>, cluster_node_id: Option<u64>) -> Option<u64> {
    env_node_id
        .and_then(|value| value.parse::<u64>().ok())
        .or(cluster_node_id)
}

/// 解析采样率 (0.0-1.0). 非法输入回退 0.0, 不导出 trace.
pub fn parse_sample_ratio(raw: &str) -> f64 {
    match raw.trim().parse::<f64>() {
        Ok(r) if r.is_finite() && (0.0..=1.0).contains(&r) => r,
        _ => 0.0,
    }
}

/// 解析 `OTEL_RESOURCE_ATTRIBUTES` (key=value,key2=value2).
fn parse_resource_attributes(raw: &str) -> Vec<KeyValue> {
    raw.split(',')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            let k = k.trim();
            let v = v.trim();
            if k.is_empty() {
                None
            } else {
                Some(KeyValue::new(k.to_string(), v.to_string()))
            }
        })
        .collect()
}

/// 将 `otlp_headers` 转换为 tonic gRPC `MetadataMap` (key 小写化).
pub fn build_tonic_metadata(headers: &BTreeMap<String, String>) -> MetadataMap {
    let mut map = MetadataMap::new();
    for (k, v) in headers {
        let lower = k.trim().to_ascii_lowercase();
        if let (Ok(key), Ok(val)) = (
            MetadataKey::from_bytes(lower.as_bytes()),
            MetadataValue::from_str(v.trim()),
        ) {
            map.insert(key, val);
        }
    }
    map
}

fn service_instance_id(config: &OtelInitConfig) -> String {
    if let Some(node_id) = config.node_id {
        return node_id.to_string();
    }
    let host = config
        .host_label
        .as_deref()
        .filter(|h| !h.is_empty())
        .unwrap_or("localhost");
    format!("{host}:{}", config.tcp_port)
}

fn build_resource(config: &OtelInitConfig) -> Resource {
    let instance_id = service_instance_id(config);
    let mut builder = Resource::builder()
        .with_service_name(config.service_name.clone())
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .with_attribute(KeyValue::new("service.instance.id", instance_id));
    if let Some(host) = &config.host_label {
        builder = builder.with_attribute(KeyValue::new("host.name", host.clone()));
    }
    if let Some(node_id) = config.node_id {
        builder = builder.with_attribute(KeyValue::new("node_id", node_id.to_string()));
    }
    if let Some(env) = &config.deployment_environment {
        builder = builder.with_attribute(KeyValue::new("deployment.environment.name", env.clone()));
    }
    for kv in &config.extra_resource_attrs {
        builder = builder.with_attribute(kv.clone());
    }
    builder.build()
}

/// 从 resolved observability 配置解析 OTel 初始化参数.
pub fn otel_config_from_settings(
    obs: &ResolvedObservability,
    tcp_port: u16,
    cluster_node_id: Option<u64>,
) -> Option<OtelInitConfig> {
    let endpoint = obs.otlp_endpoint.as_ref().filter(|s| !s.is_empty())?;
    let deployment_environment = obs
        .deployment_env
        .clone()
        .or_else(|| Some("dev".to_string()));
    let extra_resource_attrs = env_nonempty("OTEL_RESOURCE_ATTRIBUTES")
        .map(|raw| parse_resource_attributes(&raw))
        .unwrap_or_default();
    Some(OtelInitConfig {
        endpoint: endpoint.clone(),
        service_name: obs.otel_service_name.clone(),
        host_label: obs.host_label.clone(),
        node_id: resolve_node_id(env_nonempty("AIKV_NODE_ID").as_deref(), cluster_node_id),
        tcp_port,
        deployment_environment,
        extra_resource_attrs,
        otlp_headers: obs.otlp_headers.clone(),
        sample_ratio: obs.otel_sample_ratio,
    })
}

/// 从环境变量与 CLI 解析 OTel 配置 (legacy; main 路径使用 `otel_config_from_settings`).
pub fn otel_config_from_env(tcp_port: u16, cluster_node_id: Option<u64>) -> Option<OtelInitConfig> {
    let endpoint = env_nonempty("OTEL_EXPORTER_OTLP_ENDPOINT")
        .or_else(|| env_nonempty("AIKV_OTLP_ENDPOINT"))?;
    let service_name = env_nonempty("OTEL_SERVICE_NAME").unwrap_or_else(|| "aikv".to_string());
    let host_label = env_nonempty("AIKV_HOST_LABEL");
    let node_id = resolve_node_id(env_nonempty("AIKV_NODE_ID").as_deref(), cluster_node_id);
    let deployment_environment = env_nonempty("OTEL_DEPLOYMENT_ENVIRONMENT")
        .or_else(|| env_nonempty("AIKV_DEPLOYMENT_ENV"))
        .or_else(|| Some("dev".to_string()));
    let extra_resource_attrs = env_nonempty("OTEL_RESOURCE_ATTRIBUTES")
        .map(|raw| parse_resource_attributes(&raw))
        .unwrap_or_default();
    let sample_ratio = env_nonempty("AIKV_OTEL_SAMPLE_RATIO")
        .map(|v| parse_sample_ratio(&v))
        .unwrap_or(0.0);
    Some(OtelInitConfig {
        endpoint,
        service_name,
        host_label,
        node_id,
        tcp_port,
        deployment_environment,
        extra_resource_attrs,
        otlp_headers: BTreeMap::new(),
        sample_ratio,
    })
}

/// 将 OTel trace/span id 写入 tracing span 字段, JSON 日志可提取.
pub fn record_trace_ids_on_span(span: &tracing::Span) {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let cx = span.context();
    let otel_span = cx.span();
    let sc = otel_span.span_context();
    if sc.is_valid() {
        span.record("trace_id", tracing::field::display(sc.trace_id()));
        span.record("span_id", tracing::field::display(sc.span_id()));
    }
}

/// W3C TraceContext 跨节点传播辅助 (`traceparent` / `tracestate`).
struct MapCarrier<'a>(&'a mut BTreeMap<String, String>);

impl Injector for MapCarrier<'_> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value);
    }
}

struct ReadMapCarrier<'a>(&'a BTreeMap<String, String>);

impl Extractor for ReadMapCarrier<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }
}

/// 将当前 `Context` 的 W3C `traceparent`/`tracestate` 注入字符串映射.
pub fn inject_trace_context(carrier: &mut BTreeMap<String, String>) {
    global::get_text_map_propagator(|prop| {
        prop.inject_context(&Context::current(), &mut MapCarrier(carrier));
    });
}

/// 从字符串映射提取上游 W3C `Context`.
pub fn extract_trace_context(carrier: &BTreeMap<String, String>) -> Context {
    global::get_text_map_propagator(|prop| prop.extract(&ReadMapCarrier(carrier)))
}

/// 初始化 OTel Tracer + Meter + Logger. 指标由导出任务按配置间隔推送.
pub fn init_otel(config: &OtelInitConfig) -> bool {
    global::set_text_map_propagator(TraceContextPropagator::new());
    let resource = build_resource(config);
    let endpoint = config.endpoint.clone();
    let metadata = build_tonic_metadata(&config.otlp_headers);
    let export_traces =
        crate::config::should_export_traces(config.sample_ratio, Some(config.endpoint.as_str()));

    if export_traces {
        let span_exporter = match opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.clone())
            .with_metadata(metadata.clone())
            .build()
        {
            Ok(exporter) => exporter,
            Err(_) => return false,
        };

        let tracer_provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
                config.sample_ratio,
            ))))
            .with_batch_exporter(span_exporter)
            .with_resource(resource.clone())
            .build();
        global::set_tracer_provider(tracer_provider.clone());
        let _ = TRACER_PROVIDER.set(tracer_provider);
    }

    let metric_exporter = match opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint.clone())
        .with_metadata(metadata.clone())
        .build()
    {
        Ok(exporter) => exporter,
        Err(_) => {
            if let Some(tp) = TRACER_PROVIDER.get() {
                let _ = tp.shutdown();
            }
            return false;
        }
    };
    let _ = METRIC_EXPORTER.set(metric_exporter);

    let bounds_secs: Vec<f64> = aidb::statistics::COMMAND_DURATION_BOUNDS_US
        .iter()
        .map(|&us| us as f64 / 1_000_000.0)
        .collect();
    let reader = Arc::new(ManualReader::builder().build());
    let _ = EXPORT_READER.set(Arc::clone(&reader));
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(SharedReader { inner: reader })
        .with_view(move |inst: &opentelemetry_sdk::metrics::Instrument| {
            if inst.kind() == InstrumentKind::Histogram {
                opentelemetry_sdk::metrics::Stream::builder()
                    .with_aggregation(Aggregation::ExplicitBucketHistogram {
                        boundaries: bounds_secs.clone(),
                        record_min_max: true,
                    })
                    .build()
                    .ok()
            } else {
                None
            }
        })
        .with_resource(resource.clone())
        .build();
    global::set_meter_provider(meter_provider.clone());
    let _ = METER_PROVIDER.set(meter_provider);

    if let Ok(log_exporter) = opentelemetry_otlp::LogExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .with_metadata(metadata)
        .build()
    {
        let logger_provider = SdkLoggerProvider::builder()
            .with_batch_exporter(log_exporter)
            .with_resource(resource)
            .build();
        let _ = LOGGER_PROVIDER.set(logger_provider);
    }

    aidb::metrics::init();
    true
}

/// 返回已初始化的全局 `SdkLoggerProvider`, 供 `OpenTelemetryTracingBridge` 接入.
pub fn logger_provider() -> Option<&'static SdkLoggerProvider> {
    LOGGER_PROVIDER.get()
}

/// 导出任务调用: 收集全局 meter 上这一轮的增量并送出, 不另起 15 秒时钟.
pub async fn flush_otel_metrics() {
    let (Some(reader), Some(exporter)) = (EXPORT_READER.get(), METRIC_EXPORTER.get()) else {
        return;
    };
    let mut collected = ResourceMetrics::default();
    if MetricReader::collect(reader.as_ref(), &mut collected).is_err() {
        return;
    }
    let _ = exporter.export(&collected).await;
}

struct SharedReader {
    inner: Arc<ManualReader>,
}

impl std::fmt::Debug for SharedReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedReader")
    }
}

impl MetricReader for SharedReader {
    fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
        self.inner.register_pipeline(pipeline);
    }

    fn collect(&self, rm: &mut ResourceMetrics) -> OTelSdkResult {
        MetricReader::collect(self.inner.as_ref(), rm)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn temporality(&self, kind: InstrumentKind) -> Temporality {
        self.inner.temporality(kind)
    }
}

pub fn tracer_installed() -> bool {
    TRACER_PROVIDER.get().is_some()
}

pub fn shutdown_otel() {
    if let Some(lp) = LOGGER_PROVIDER.get() {
        let _ = lp.shutdown();
    }
    if let Some(tp) = TRACER_PROVIDER.get() {
        let _ = tp.shutdown();
    }
    if let Some(mp) = METER_PROVIDER.get() {
        let _ = mp.shutdown();
    }
}

/// 判断日志 target 是否属于 OTel/HTTP/gRPC 导出路径, 防止 OTLP Logs 自激循环.
pub fn is_otel_self_target(target: &str) -> bool {
    target.starts_with("opentelemetry")
        || target.starts_with("tonic")
        || target.starts_with("h2")
        || target.starts_with("hyper")
        || target.starts_with("reqwest")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归测试: `AIKV_NODE_ID` 缺失时 OTel 应回落到集群节点 ID.
    #[test]
    fn node_id_falls_back_to_cluster_node_id() {
        assert_eq!(resolve_node_id(None, Some(42)), Some(42));
    }

    /// 回归测试: 有效 `AIKV_NODE_ID` 优先于传入的集群节点 ID.
    #[test]
    fn node_id_from_env_takes_precedence() {
        assert_eq!(resolve_node_id(Some("7"), Some(42)), Some(7));
    }

    /// 回归测试: 非法 `AIKV_NODE_ID` 不应覆盖有效的集群节点 ID.
    #[test]
    fn invalid_env_node_id_falls_back_to_cluster_node_id() {
        assert_eq!(resolve_node_id(Some("not-a-number"), Some(42)), Some(42));
    }

    #[test]
    fn sample_zero_does_not_install_tracer() {
        let config = OtelInitConfig {
            endpoint: String::new(),
            service_name: "aikv".to_string(),
            host_label: None,
            node_id: None,
            tcp_port: 6379,
            deployment_environment: None,
            extra_resource_attrs: Vec::new(),
            otlp_headers: BTreeMap::new(),
            sample_ratio: 0.0,
        };
        assert!(!crate::config::should_export_traces(
            config.sample_ratio,
            Some(config.endpoint.as_str())
        ));
    }

    #[test]
    fn tonic_metadata_preserves_auth_and_org_headers() {
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".into(), "Basic abc=".into());
        headers.insert("organization".into(), "default".into());
        let md = build_tonic_metadata(&headers);
        assert_eq!(
            md.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Basic abc=")
        );
        assert_eq!(
            md.get("organization").and_then(|v| v.to_str().ok()),
            Some("default")
        );
    }

    #[test]
    fn self_target_filter_blocks_otel_and_tonic() {
        assert!(is_otel_self_target("opentelemetry_otlp::exporter"));
        assert!(is_otel_self_target("tonic::transport::channel"));
        assert!(is_otel_self_target("h2::codec"));
        assert!(!is_otel_self_target("aikv::server"));
    }
}

/// 内存态 OTel 导出测试辅助 (用于集成测试验证 `export::OtelSink`).
#[doc(hidden)]
pub mod testutil {
    use std::collections::HashMap;
    use std::future::Future as _;
    use std::sync::Arc;

    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter as _;
    use opentelemetry_sdk::metrics::reader::MetricReader as _;
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, ManualReader, SdkMeterProvider};

    use crate::server::export::{collect_export_snapshot, push_deltas, ExportSnapshot, OtelSink};
    use crate::server::metrics::ServerMetrics;

    pub struct InMemoryExportHarness {
        pub exporter: InMemoryMetricExporter,
        pub provider: Arc<SdkMeterProvider>,
        reader: Arc<ManualReader>,
        pub sink: OtelSink,
        baseline: ExportSnapshot,
    }

    impl InMemoryExportHarness {
        pub fn new() -> Self {
            let exporter = InMemoryMetricExporter::default();
            let reader = Arc::new(ManualReader::builder().build());
            let provider = Arc::new(
                SdkMeterProvider::builder()
                    .with_reader(super::SharedReader {
                        inner: Arc::clone(&reader),
                    })
                    .build(),
            );
            let sink = OtelSink::with_meter(provider.meter("aikv-test"));
            Self {
                exporter,
                provider,
                reader,
                sink,
                baseline: ExportSnapshot::default(),
            }
        }

        pub fn push_from(&mut self, metrics: &ServerMetrics) {
            let current = collect_export_snapshot(metrics, 0, &[], None, &[]);
            push_deltas(&current, &self.baseline, &mut self.sink);
            self.baseline = current;
            let mut rm = ResourceMetrics::default();
            if self.reader.collect(&mut rm).is_ok() {
                let fut = self.exporter.export(&rm);
                let mut pinned = std::pin::pin!(fut);
                let waker = std::task::Waker::noop();
                let mut cx = std::task::Context::from_waker(waker);
                let _ = pinned.as_mut().poll(&mut cx);
            }
        }
    }

    impl Default for InMemoryExportHarness {
        fn default() -> Self {
            Self::new()
        }
    }

    pub fn counter_sum(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
        let metrics = exporter.get_finished_metrics().unwrap_or_default();
        let mut by_attrs: HashMap<Vec<(String, String)>, u64> = HashMap::new();
        for rm in &metrics {
            for sm in rm.scope_metrics() {
                for m in sm.metrics() {
                    if m.name() != name {
                        continue;
                    }
                    if let AggregatedMetrics::U64(MetricData::Sum(sum)) = m.data() {
                        for dp in sum.data_points() {
                            let key = dp
                                .attributes()
                                .map(|kv| (kv.key.to_string(), kv.value.to_string()))
                                .collect();
                            by_attrs.insert(key, dp.value());
                        }
                    }
                }
            }
        }
        by_attrs.values().sum()
    }

    pub fn gauge_value(exporter: &InMemoryMetricExporter, name: &str) -> f64 {
        let metrics = exporter.get_finished_metrics().unwrap_or_default();
        let mut last = 0.0;
        for rm in &metrics {
            for sm in rm.scope_metrics() {
                for m in sm.metrics() {
                    if m.name() != name {
                        continue;
                    }
                    if let AggregatedMetrics::F64(MetricData::Gauge(g)) = m.data() {
                        if let Some(dp) = g.data_points().last() {
                            last = dp.value();
                        }
                    }
                }
            }
        }
        last
    }

    pub fn histogram_count(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
        let metrics = exporter.get_finished_metrics().unwrap_or_default();
        let mut total = 0;
        for rm in &metrics {
            for sm in rm.scope_metrics() {
                for m in sm.metrics() {
                    if m.name() != name {
                        continue;
                    }
                    if let AggregatedMetrics::F64(MetricData::Histogram(h)) = m.data() {
                        if let Some(dp) = h.data_points().last() {
                            total = dp.count();
                        }
                    }
                }
            }
        }
        total
    }

    pub fn metric_exists(exporter: &InMemoryMetricExporter, name: &str) -> bool {
        let metrics = exporter.get_finished_metrics().unwrap_or_default();
        metrics.iter().any(|rm| {
            rm.scope_metrics()
                .any(|sm| sm.metrics().any(|m| m.name() == name))
        })
    }
}
