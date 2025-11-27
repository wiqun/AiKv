//! Observability module for monitoring and tracing
//!
//! This module provides:
//! - Structured logging with JSON format support
//! - Dynamic log level adjustment
//! - Slow query logging
//! - Prometheus metrics
//! - OpenTelemetry tracing integration

pub mod logging;
pub mod metrics;
pub mod tracing_setup;

pub use logging::{LogConfig, LogFormat, LoggingManager, SlowQueryLog};
pub use metrics::{CommandMetrics, ConnectionMetrics, MemoryMetrics, Metrics};
pub use tracing_setup::TracingConfig;
