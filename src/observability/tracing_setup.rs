//! Tracing setup module for OpenTelemetry integration
//!
//! Features:
//! - OpenTelemetry integration
//! - Request tracing with spans
//! - Distributed tracing support

use std::time::Duration;

/// Tracing configuration
#[derive(Debug, Clone)]
pub struct TracingConfig {
    /// Enable OpenTelemetry tracing
    pub enabled: bool,
    /// Service name for tracing
    pub service_name: String,
    /// OTLP endpoint (e.g., "http://localhost:4317")
    pub otlp_endpoint: Option<String>,
    /// Sampling ratio (0.0 to 1.0)
    pub sampling_ratio: f64,
    /// Batch export timeout
    pub export_timeout: Duration,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            service_name: "aikv".to_string(),
            otlp_endpoint: None,
            sampling_ratio: 1.0,
            export_timeout: Duration::from_secs(10),
        }
    }
}

impl TracingConfig {
    /// Create a new tracing config with service name
    pub fn new(service_name: &str) -> Self {
        Self {
            service_name: service_name.to_string(),
            ..Default::default()
        }
    }

    /// Enable tracing with OTLP endpoint
    pub fn with_otlp(mut self, endpoint: &str) -> Self {
        self.enabled = true;
        self.otlp_endpoint = Some(endpoint.to_string());
        self
    }

    /// Set sampling ratio
    pub fn with_sampling(mut self, ratio: f64) -> Self {
        self.sampling_ratio = ratio.clamp(0.0, 1.0);
        self
    }
}

/// Trace context for request tracing
#[derive(Debug, Clone)]
pub struct TraceContext {
    /// Trace ID (128-bit hex string)
    pub trace_id: String,
    /// Span ID (64-bit hex string)
    pub span_id: String,
    /// Parent span ID (if any)
    pub parent_span_id: Option<String>,
    /// Whether the trace is sampled
    pub sampled: bool,
}

impl TraceContext {
    /// Create a new trace context with generated IDs
    pub fn new() -> Self {
        Self {
            trace_id: Self::generate_trace_id(),
            span_id: Self::generate_span_id(),
            parent_span_id: None,
            sampled: true,
        }
    }

    /// Create a child context
    pub fn child(&self) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            span_id: Self::generate_span_id(),
            parent_span_id: Some(self.span_id.clone()),
            sampled: self.sampled,
        }
    }

    /// Generate a 128-bit trace ID
    fn generate_trace_id() -> String {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        use std::time::{SystemTime, UNIX_EPOCH};

        let hasher_builder = RandomState::new();
        let mut result = String::with_capacity(32);

        for i in 0..2 {
            let mut hasher = hasher_builder.build_hasher();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            hasher.write_u64(nanos.wrapping_add(i as u64));
            result.push_str(&format!("{:016x}", hasher.finish()));
        }

        result
    }

    /// Generate a 64-bit span ID
    fn generate_span_id() -> String {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        use std::time::{SystemTime, UNIX_EPOCH};

        let hasher_builder = RandomState::new();
        let mut hasher = hasher_builder.build_hasher();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        hasher.write_u64(nanos);
        format!("{:016x}", hasher.finish())
    }

    /// Format as W3C traceparent header
    pub fn to_traceparent(&self) -> String {
        let flags = if self.sampled { "01" } else { "00" };
        format!("00-{}-{}-{}", self.trace_id, self.span_id, flags)
    }

    /// Parse from W3C traceparent header
    pub fn from_traceparent(header: &str) -> Option<Self> {
        let parts: Vec<&str> = header.split('-').collect();
        if parts.len() != 4 || parts[0] != "00" {
            return None;
        }

        Some(Self {
            trace_id: parts[1].to_string(),
            span_id: parts[2].to_string(),
            parent_span_id: None,
            sampled: parts[3] == "01",
        })
    }
}

impl Default for TraceContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Command span for tracing
#[derive(Debug)]
pub struct CommandSpan {
    /// Command name
    pub command: String,
    /// Start time
    pub start: std::time::Instant,
    /// Trace context
    pub context: TraceContext,
    /// Attributes
    pub attributes: Vec<(String, String)>,
}

impl CommandSpan {
    /// Create a new command span
    pub fn new(command: &str) -> Self {
        Self {
            command: command.to_string(),
            start: std::time::Instant::now(),
            context: TraceContext::new(),
            attributes: Vec::new(),
        }
    }

    /// Create a span with parent context
    pub fn with_parent(command: &str, parent: &TraceContext) -> Self {
        Self {
            command: command.to_string(),
            start: std::time::Instant::now(),
            context: parent.child(),
            attributes: Vec::new(),
        }
    }

    /// Add an attribute
    pub fn add_attribute(&mut self, key: &str, value: &str) {
        self.attributes.push((key.to_string(), value.to_string()));
    }

    /// Get elapsed time
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Finish the span and return duration
    pub fn finish(self) -> Duration {
        self.start.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracing_config() {
        let config = TracingConfig::new("test-service")
            .with_otlp("http://localhost:4317")
            .with_sampling(0.5);

        assert!(config.enabled);
        assert_eq!(config.service_name, "test-service");
        assert_eq!(
            config.otlp_endpoint,
            Some("http://localhost:4317".to_string())
        );
        assert_eq!(config.sampling_ratio, 0.5);
    }

    #[test]
    fn test_trace_context() {
        let ctx = TraceContext::new();
        assert_eq!(ctx.trace_id.len(), 32);
        assert_eq!(ctx.span_id.len(), 16);
        assert!(ctx.sampled);
        assert!(ctx.parent_span_id.is_none());

        let child = ctx.child();
        assert_eq!(child.trace_id, ctx.trace_id);
        assert_ne!(child.span_id, ctx.span_id);
        assert_eq!(child.parent_span_id, Some(ctx.span_id.clone()));
    }

    #[test]
    fn test_traceparent() {
        let ctx = TraceContext::new();
        let header = ctx.to_traceparent();
        assert!(header.starts_with("00-"));
        assert!(header.ends_with("-01"));

        let parsed = TraceContext::from_traceparent(&header).unwrap();
        assert_eq!(parsed.trace_id, ctx.trace_id);
        assert_eq!(parsed.span_id, ctx.span_id);
        assert!(parsed.sampled);
    }

    #[test]
    fn test_command_span() {
        let mut span = CommandSpan::new("GET");
        span.add_attribute("key", "test_key");
        span.add_attribute("db", "0");

        assert_eq!(span.command, "GET");
        assert_eq!(span.attributes.len(), 2);

        std::thread::sleep(std::time::Duration::from_millis(10));
        let duration = span.finish();
        assert!(duration.as_millis() >= 10);
    }
}
