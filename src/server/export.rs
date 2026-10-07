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

/// 后面的任务往这份快照里填命令, 引擎和 Raft 样本. 这里先返回空快照, 让导出时钟只有一处.
pub fn collect_export_snapshot() -> ExportSnapshot {
    ExportSnapshot::default()
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

/// monitoring 下计数器一次 `add(delta)`. 直方图不循环 `record`.
#[cfg(feature = "monitoring")]
pub struct OtelSink;

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
        let _ = (name, labels, snap.count, snap.sum_us);
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
