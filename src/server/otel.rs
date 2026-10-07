//! OTel traces + metrics 初始化 (monitoring feature).

use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use opentelemetry::global;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::reader::MetricReader;
use opentelemetry_sdk::metrics::Pipeline;
use opentelemetry_sdk::metrics::{InstrumentKind, ManualReader, SdkMeterProvider, Temporality};
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use opentelemetry_sdk::Resource;

use crate::config::ResolvedObservability;

static TRACER_PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();
static METER_PROVIDER: OnceLock<SdkMeterProvider> = OnceLock::new();
static EXPORT_READER: OnceLock<Arc<ManualReader>> = OnceLock::new();
static METRIC_EXPORTER: OnceLock<opentelemetry_otlp::MetricExporter> = OnceLock::new();

/// OTel 初始化参数 (Resource 标签).
#[derive(Debug, Clone)]
pub struct OtelInitConfig {
    pub endpoint: String,
    pub service_name: String,
    pub host_label: Option<String>,
    pub node_id: Option<u64>,
    pub tcp_port: u16,
    pub deployment_environment: Option<String>,
    pub extra_resource_attrs: Vec<KeyValue>,
    /// Trace 采样率 (0.0-1.0), 由 `AIKV_OTEL_SAMPLE_RATIO` 控制, 默认 0.0.
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
        _ => {
            eprintln!(
                "warn: invalid AIKV_OTEL_SAMPLE_RATIO value {:?}, falling back to 0.0 (no traces)",
                raw
            );
            0.0
        }
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
        .with_attribute(KeyValue::new("service.instance.id", instance_id.clone()));
    if let Some(host) = &config.host_label {
        builder = builder.with_attribute(KeyValue::new("host.name", host.clone()));
    }
    if let Some(node_id) = config.node_id {
        builder = builder.with_attribute(KeyValue::new("node_id", node_id.to_string()));
    }
    if let Some(env) = &config.deployment_environment {
        builder = builder.with_attribute(KeyValue::new("deployment.environment", env.clone()));
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
    #[cfg(feature = "cluster")] cluster_node_id: Option<u64>,
    #[cfg(not(feature = "cluster"))] cluster_node_id: Option<u64>,
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
        sample_ratio: obs.otel_sample_ratio,
    })
}

/// 从环境变量与 CLI 解析 OTel 配置 (legacy; main 路径使用 `otel_config_from_settings`).
pub fn otel_config_from_env(
    tcp_port: u16,
    #[cfg(feature = "cluster")] cluster_node_id: Option<u64>,
    #[cfg(not(feature = "cluster"))] cluster_node_id: Option<u64>,
) -> Option<OtelInitConfig> {
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

/// 初始化 OTel Tracer + Meter. 指标由导出任务按配置间隔推送, 这里不另起时钟.
pub fn init_otel(config: &OtelInitConfig) -> bool {
    let resource = build_resource(config);
    let endpoint = config.endpoint.clone();
    let export_traces =
        crate::config::should_export_traces(config.sample_ratio, Some(config.endpoint.as_str()));

    if export_traces {
        let span_exporter = match opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.clone())
            .build()
        {
            Ok(exporter) => exporter,
            Err(e) => {
                eprintln!("warn: OTel trace exporter build failed: {e}");
                return false;
            }
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
        .build()
    {
        Ok(exporter) => exporter,
        Err(e) => {
            eprintln!("warn: OTel metrics exporter build failed: {e}");
            if let Some(tp) = TRACER_PROVIDER.get() {
                let _ = tp.shutdown();
            }
            return false;
        }
    };
    let _ = METRIC_EXPORTER.set(metric_exporter);

    let reader = Arc::new(ManualReader::builder().build());
    let _ = EXPORT_READER.set(Arc::clone(&reader));
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(SharedReader { inner: reader })
        .with_resource(resource)
        .build();
    global::set_meter_provider(meter_provider.clone());
    let _ = METER_PROVIDER.set(meter_provider);

    aidb::metrics::init();
    if export_traces {
        eprintln!("info: OTel traces+metrics initialized (endpoint={endpoint})");
    } else {
        eprintln!("info: OTel metrics initialized (endpoint={endpoint})");
    }
    true
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
    if let Some(tp) = TRACER_PROVIDER.get() {
        let _ = tp.shutdown();
    }
    if let Some(mp) = METER_PROVIDER.get() {
        let _ = mp.shutdown();
    }
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
            endpoint: "http://127.0.0.1:4317".to_string(),
            service_name: "aikv".to_string(),
            host_label: None,
            node_id: None,
            tcp_port: 6379,
            deployment_environment: None,
            extra_resource_attrs: Vec::new(),
            sample_ratio: 0.0,
        };
        assert!(!crate::config::should_export_traces(
            config.sample_ratio,
            Some(config.endpoint.as_str())
        ));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let _ = init_otel(&config);
        });
        assert!(!tracer_installed());
    }
}
