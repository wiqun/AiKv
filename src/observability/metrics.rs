//! Metrics module for Prometheus integration and statistics collection
//!
//! Features:
//! - Prometheus metrics export
//! - Command execution statistics
//! - Connection statistics
//! - Memory usage statistics

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Atomic counter for metrics
#[derive(Debug, Default)]
pub struct Counter {
    value: AtomicU64,
}

impl Counter {
    /// Create a new counter
    pub fn new() -> Self {
        Self {
            value: AtomicU64::new(0),
        }
    }

    /// Increment the counter by 1
    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment the counter by a specific amount
    pub fn inc_by(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    /// Get the current value
    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }

    /// Reset the counter
    pub fn reset(&self) {
        self.value.store(0, Ordering::Relaxed);
    }
}

/// Atomic gauge for metrics (can go up and down)
#[derive(Debug, Default)]
pub struct Gauge {
    value: AtomicU64,
}

impl Gauge {
    /// Create a new gauge
    pub fn new() -> Self {
        Self {
            value: AtomicU64::new(0),
        }
    }

    /// Set the gauge value
    pub fn set(&self, value: u64) {
        self.value.store(value, Ordering::Relaxed);
    }

    /// Get the current value
    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }

    /// Increment the gauge
    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement the gauge
    pub fn dec(&self) {
        self.value.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Command execution metrics
#[derive(Debug)]
pub struct CommandMetrics {
    /// Total commands processed
    pub total_commands: Counter,
    /// Commands per command type
    pub commands_by_type: RwLock<HashMap<String, Counter>>,
    /// Total command errors
    pub total_errors: Counter,
    /// Errors per command type
    pub errors_by_type: RwLock<HashMap<String, Counter>>,
    /// Total command execution time in microseconds
    pub total_duration_us: AtomicU64,
    /// Commands per second (calculated)
    ops_per_sec: RwLock<f64>,
    /// Last calculation time
    last_ops_calc: RwLock<Instant>,
    /// Commands at last calculation
    last_ops_count: AtomicU64,
}

impl Default for CommandMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandMetrics {
    /// Create new command metrics
    pub fn new() -> Self {
        Self {
            total_commands: Counter::new(),
            commands_by_type: RwLock::new(HashMap::new()),
            total_errors: Counter::new(),
            errors_by_type: RwLock::new(HashMap::new()),
            total_duration_us: AtomicU64::new(0),
            ops_per_sec: RwLock::new(0.0),
            last_ops_calc: RwLock::new(Instant::now()),
            last_ops_count: AtomicU64::new(0),
        }
    }

    /// Record a successful command execution
    pub fn record_command(&self, command: &str, duration: Duration) {
        self.total_commands.inc();
        self.total_duration_us
            .fetch_add(duration.as_micros() as u64, Ordering::Relaxed);

        let command_upper = command.to_uppercase();
        if let Ok(mut commands) = self.commands_by_type.write() {
            commands
                .entry(command_upper)
                .or_insert_with(Counter::new)
                .inc();
        }
    }

    /// Record a command error
    pub fn record_error(&self, command: &str) {
        self.total_errors.inc();

        let command_upper = command.to_uppercase();
        if let Ok(mut errors) = self.errors_by_type.write() {
            errors
                .entry(command_upper)
                .or_insert_with(Counter::new)
                .inc();
        }
    }

    /// Get total commands processed
    pub fn total_commands(&self) -> u64 {
        self.total_commands.get()
    }

    /// Get total errors
    pub fn total_errors(&self) -> u64 {
        self.total_errors.get()
    }

    /// Get average command duration in microseconds
    pub fn avg_duration_us(&self) -> f64 {
        let total = self.total_commands.get();
        if total == 0 {
            return 0.0;
        }
        self.total_duration_us.load(Ordering::Relaxed) as f64 / total as f64
    }

    /// Get commands by type
    pub fn commands_by_type(&self) -> HashMap<String, u64> {
        if let Ok(commands) = self.commands_by_type.read() {
            commands.iter().map(|(k, v)| (k.clone(), v.get())).collect()
        } else {
            HashMap::new()
        }
    }

    /// Calculate and get operations per second
    pub fn ops_per_sec(&self) -> f64 {
        let now = Instant::now();
        let current_count = self.total_commands.get();

        if let (Ok(mut ops), Ok(mut last_calc)) =
            (self.ops_per_sec.write(), self.last_ops_calc.write())
        {
            let elapsed = now.duration_since(*last_calc);
            if elapsed >= Duration::from_secs(1) {
                let last_count = self.last_ops_count.swap(current_count, Ordering::Relaxed);
                let delta = current_count.saturating_sub(last_count);
                *ops = delta as f64 / elapsed.as_secs_f64();
                *last_calc = now;
            }
            *ops
        } else {
            0.0
        }
    }

    /// Reset all metrics
    pub fn reset(&self) {
        self.total_commands.reset();
        self.total_errors.reset();
        self.total_duration_us.store(0, Ordering::Relaxed);
        if let Ok(mut commands) = self.commands_by_type.write() {
            commands.clear();
        }
        if let Ok(mut errors) = self.errors_by_type.write() {
            errors.clear();
        }
    }
}

/// Connection metrics
#[derive(Debug, Default)]
pub struct ConnectionMetrics {
    /// Total connections received
    pub total_connections: Counter,
    /// Currently connected clients
    pub connected_clients: Gauge,
    /// Rejected connections
    pub rejected_connections: Counter,
    /// Total bytes received
    pub bytes_received: Counter,
    /// Total bytes sent
    pub bytes_sent: Counter,
}

impl ConnectionMetrics {
    /// Create new connection metrics
    pub fn new() -> Self {
        Self {
            total_connections: Counter::new(),
            connected_clients: Gauge::new(),
            rejected_connections: Counter::new(),
            bytes_received: Counter::new(),
            bytes_sent: Counter::new(),
        }
    }

    /// Record a new connection
    pub fn record_connection(&self) {
        self.total_connections.inc();
        self.connected_clients.inc();
    }

    /// Record a disconnection
    pub fn record_disconnection(&self) {
        self.connected_clients.dec();
    }

    /// Record a rejected connection
    pub fn record_rejected(&self) {
        self.rejected_connections.inc();
    }

    /// Record bytes received
    pub fn record_bytes_received(&self, bytes: u64) {
        self.bytes_received.inc_by(bytes);
    }

    /// Record bytes sent
    pub fn record_bytes_sent(&self, bytes: u64) {
        self.bytes_sent.inc_by(bytes);
    }

    /// Get total connections
    pub fn total_connections(&self) -> u64 {
        self.total_connections.get()
    }

    /// Get connected clients count
    pub fn connected_clients(&self) -> u64 {
        self.connected_clients.get()
    }

    /// Get rejected connections count
    pub fn rejected_connections(&self) -> u64 {
        self.rejected_connections.get()
    }

    /// Calculate kbps from bytes in a period
    fn calculate_kbps(bytes_in_period: u64, period_secs: f64) -> f64 {
        if period_secs <= 0.0 {
            return 0.0;
        }
        (bytes_in_period as f64 / 1024.0) / period_secs
    }

    /// Get instantaneous input kbps (requires periodic calculation)
    pub fn input_kbps(&self, bytes_in_period: u64, period_secs: f64) -> f64 {
        Self::calculate_kbps(bytes_in_period, period_secs)
    }

    /// Get instantaneous output kbps (requires periodic calculation)
    pub fn output_kbps(&self, bytes_in_period: u64, period_secs: f64) -> f64 {
        Self::calculate_kbps(bytes_in_period, period_secs)
    }
}

/// Memory usage metrics
#[derive(Debug, Default)]
pub struct MemoryMetrics {
    /// Used memory in bytes
    pub used_memory: Gauge,
    /// Peak used memory in bytes
    pub used_memory_peak: AtomicU64,
    /// Memory used by Lua scripts
    pub used_memory_lua: Gauge,
    /// Number of keys in keyspace
    pub total_keys: Gauge,
    /// Expired keys count
    pub expired_keys: Counter,
    /// Evicted keys count
    pub evicted_keys: Counter,
    /// Keyspace hits
    pub keyspace_hits: Counter,
    /// Keyspace misses
    pub keyspace_misses: Counter,
}

impl MemoryMetrics {
    /// Create new memory metrics
    pub fn new() -> Self {
        Self {
            used_memory: Gauge::new(),
            used_memory_peak: AtomicU64::new(0),
            used_memory_lua: Gauge::new(),
            total_keys: Gauge::new(),
            expired_keys: Counter::new(),
            evicted_keys: Counter::new(),
            keyspace_hits: Counter::new(),
            keyspace_misses: Counter::new(),
        }
    }

    /// Update used memory
    pub fn set_used_memory(&self, bytes: u64) {
        self.used_memory.set(bytes);
        // Update peak if necessary
        let mut current_peak = self.used_memory_peak.load(Ordering::Relaxed);
        while bytes > current_peak {
            match self.used_memory_peak.compare_exchange_weak(
                current_peak,
                bytes,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(peak) => current_peak = peak,
            }
        }
    }

    /// Get used memory
    pub fn used_memory(&self) -> u64 {
        self.used_memory.get()
    }

    /// Get peak used memory
    pub fn used_memory_peak(&self) -> u64 {
        self.used_memory_peak.load(Ordering::Relaxed)
    }

    /// Record a key hit
    pub fn record_hit(&self) {
        self.keyspace_hits.inc();
    }

    /// Record a key miss
    pub fn record_miss(&self) {
        self.keyspace_misses.inc();
    }

    /// Record an expired key
    pub fn record_expired(&self) {
        self.expired_keys.inc();
    }

    /// Record an evicted key
    pub fn record_evicted(&self) {
        self.evicted_keys.inc();
    }

    /// Get hit rate
    pub fn hit_rate(&self) -> f64 {
        let hits = self.keyspace_hits.get();
        let misses = self.keyspace_misses.get();
        let total = hits + misses;
        if total == 0 {
            return 0.0;
        }
        hits as f64 / total as f64
    }

    /// Format bytes as human readable
    pub fn format_bytes(bytes: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;

        if bytes >= GB {
            format!("{:.2}G", bytes as f64 / GB as f64)
        } else if bytes >= MB {
            format!("{:.2}M", bytes as f64 / MB as f64)
        } else if bytes >= KB {
            format!("{:.2}K", bytes as f64 / KB as f64)
        } else {
            format!("{}B", bytes)
        }
    }
}

/// Combined metrics for the entire server
#[derive(Debug)]
pub struct Metrics {
    /// Command execution metrics
    pub commands: Arc<CommandMetrics>,
    /// Connection metrics
    pub connections: Arc<ConnectionMetrics>,
    /// Memory metrics
    pub memory: Arc<MemoryMetrics>,
    /// Server start time
    pub start_time: Instant,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    /// Create new metrics instance
    pub fn new() -> Self {
        Self {
            commands: Arc::new(CommandMetrics::new()),
            connections: Arc::new(ConnectionMetrics::new()),
            memory: Arc::new(MemoryMetrics::new()),
            start_time: Instant::now(),
        }
    }

    /// Get uptime in seconds
    pub fn uptime_seconds(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    /// Export metrics in Prometheus text format
    pub fn export_prometheus(&self) -> String {
        let mut output = String::new();

        // Server metrics
        output.push_str("# HELP aikv_uptime_seconds Server uptime in seconds\n");
        output.push_str("# TYPE aikv_uptime_seconds gauge\n");
        output.push_str(&format!("aikv_uptime_seconds {}\n", self.uptime_seconds()));

        // Command metrics
        output.push_str("# HELP aikv_commands_total Total commands processed\n");
        output.push_str("# TYPE aikv_commands_total counter\n");
        output.push_str(&format!(
            "aikv_commands_total {}\n",
            self.commands.total_commands()
        ));

        output.push_str("# HELP aikv_commands_errors_total Total command errors\n");
        output.push_str("# TYPE aikv_commands_errors_total counter\n");
        output.push_str(&format!(
            "aikv_commands_errors_total {}\n",
            self.commands.total_errors()
        ));

        output.push_str(
            "# HELP aikv_commands_duration_avg_us Average command duration in microseconds\n",
        );
        output.push_str("# TYPE aikv_commands_duration_avg_us gauge\n");
        output.push_str(&format!(
            "aikv_commands_duration_avg_us {:.2}\n",
            self.commands.avg_duration_us()
        ));

        output.push_str("# HELP aikv_ops_per_second Current operations per second\n");
        output.push_str("# TYPE aikv_ops_per_second gauge\n");
        output.push_str(&format!(
            "aikv_ops_per_second {:.2}\n",
            self.commands.ops_per_sec()
        ));

        // Connection metrics
        output.push_str("# HELP aikv_connections_total Total connections received\n");
        output.push_str("# TYPE aikv_connections_total counter\n");
        output.push_str(&format!(
            "aikv_connections_total {}\n",
            self.connections.total_connections()
        ));

        output.push_str("# HELP aikv_connected_clients Current connected clients\n");
        output.push_str("# TYPE aikv_connected_clients gauge\n");
        output.push_str(&format!(
            "aikv_connected_clients {}\n",
            self.connections.connected_clients()
        ));

        output.push_str("# HELP aikv_rejected_connections_total Total rejected connections\n");
        output.push_str("# TYPE aikv_rejected_connections_total counter\n");
        output.push_str(&format!(
            "aikv_rejected_connections_total {}\n",
            self.connections.rejected_connections()
        ));

        // Memory metrics
        output.push_str("# HELP aikv_used_memory_bytes Used memory in bytes\n");
        output.push_str("# TYPE aikv_used_memory_bytes gauge\n");
        output.push_str(&format!(
            "aikv_used_memory_bytes {}\n",
            self.memory.used_memory()
        ));

        output.push_str("# HELP aikv_used_memory_peak_bytes Peak used memory in bytes\n");
        output.push_str("# TYPE aikv_used_memory_peak_bytes gauge\n");
        output.push_str(&format!(
            "aikv_used_memory_peak_bytes {}\n",
            self.memory.used_memory_peak()
        ));

        output.push_str("# HELP aikv_keyspace_hits_total Total keyspace hits\n");
        output.push_str("# TYPE aikv_keyspace_hits_total counter\n");
        output.push_str(&format!(
            "aikv_keyspace_hits_total {}\n",
            self.memory.keyspace_hits.get()
        ));

        output.push_str("# HELP aikv_keyspace_misses_total Total keyspace misses\n");
        output.push_str("# TYPE aikv_keyspace_misses_total counter\n");
        output.push_str(&format!(
            "aikv_keyspace_misses_total {}\n",
            self.memory.keyspace_misses.get()
        ));

        output.push_str("# HELP aikv_expired_keys_total Total expired keys\n");
        output.push_str("# TYPE aikv_expired_keys_total counter\n");
        output.push_str(&format!(
            "aikv_expired_keys_total {}\n",
            self.memory.expired_keys.get()
        ));

        // Commands by type
        output.push_str("# HELP aikv_commands_by_type Commands processed by type\n");
        output.push_str("# TYPE aikv_commands_by_type counter\n");
        for (cmd, count) in self.commands.commands_by_type() {
            output.push_str(&format!(
                "aikv_commands_by_type{{command=\"{}\"}} {}\n",
                cmd, count
            ));
        }

        output
    }

    /// Get metrics summary for INFO command
    pub fn get_stats_info(&self) -> Vec<(String, String)> {
        vec![
            (
                "total_connections_received".to_string(),
                self.connections.total_connections().to_string(),
            ),
            (
                "total_commands_processed".to_string(),
                self.commands.total_commands().to_string(),
            ),
            (
                "instantaneous_ops_per_sec".to_string(),
                format!("{:.0}", self.commands.ops_per_sec()),
            ),
            (
                "total_net_input_bytes".to_string(),
                self.connections.bytes_received.get().to_string(),
            ),
            (
                "total_net_output_bytes".to_string(),
                self.connections.bytes_sent.get().to_string(),
            ),
            (
                "rejected_connections".to_string(),
                self.connections.rejected_connections().to_string(),
            ),
            (
                "expired_keys".to_string(),
                self.memory.expired_keys.get().to_string(),
            ),
            (
                "evicted_keys".to_string(),
                self.memory.evicted_keys.get().to_string(),
            ),
            (
                "keyspace_hits".to_string(),
                self.memory.keyspace_hits.get().to_string(),
            ),
            (
                "keyspace_misses".to_string(),
                self.memory.keyspace_misses.get().to_string(),
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter() {
        let counter = Counter::new();
        assert_eq!(counter.get(), 0);

        counter.inc();
        assert_eq!(counter.get(), 1);

        counter.inc_by(5);
        assert_eq!(counter.get(), 6);

        counter.reset();
        assert_eq!(counter.get(), 0);
    }

    #[test]
    fn test_gauge() {
        let gauge = Gauge::new();
        assert_eq!(gauge.get(), 0);

        gauge.set(100);
        assert_eq!(gauge.get(), 100);

        gauge.inc();
        assert_eq!(gauge.get(), 101);

        gauge.dec();
        assert_eq!(gauge.get(), 100);
    }

    #[test]
    fn test_command_metrics() {
        let metrics = CommandMetrics::new();

        metrics.record_command("GET", Duration::from_micros(100));
        metrics.record_command("SET", Duration::from_micros(200));
        metrics.record_error("GET");

        assert_eq!(metrics.total_commands(), 2);
        assert_eq!(metrics.total_errors(), 1);
        assert_eq!(metrics.avg_duration_us(), 150.0);

        let by_type = metrics.commands_by_type();
        assert_eq!(by_type.get("GET"), Some(&1));
        assert_eq!(by_type.get("SET"), Some(&1));
    }

    #[test]
    fn test_connection_metrics() {
        let metrics = ConnectionMetrics::new();

        metrics.record_connection();
        metrics.record_connection();
        assert_eq!(metrics.connected_clients(), 2);
        assert_eq!(metrics.total_connections(), 2);

        metrics.record_disconnection();
        assert_eq!(metrics.connected_clients(), 1);

        metrics.record_rejected();
        assert_eq!(metrics.rejected_connections(), 1);
    }

    #[test]
    fn test_memory_metrics() {
        let metrics = MemoryMetrics::new();

        metrics.set_used_memory(1024 * 1024);
        assert_eq!(metrics.used_memory(), 1024 * 1024);
        assert_eq!(metrics.used_memory_peak(), 1024 * 1024);

        metrics.set_used_memory(512 * 1024);
        assert_eq!(metrics.used_memory(), 512 * 1024);
        assert_eq!(metrics.used_memory_peak(), 1024 * 1024); // Peak unchanged

        metrics.record_hit();
        metrics.record_hit();
        metrics.record_miss();
        assert_eq!(metrics.keyspace_hits.get(), 2);
        assert_eq!(metrics.keyspace_misses.get(), 1);
        assert!((metrics.hit_rate() - 0.6666).abs() < 0.01);
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(MemoryMetrics::format_bytes(500), "500B");
        assert_eq!(MemoryMetrics::format_bytes(1024), "1.00K");
        assert_eq!(MemoryMetrics::format_bytes(1024 * 1024), "1.00M");
        assert_eq!(MemoryMetrics::format_bytes(1024 * 1024 * 1024), "1.00G");
    }

    #[test]
    fn test_prometheus_export() {
        let metrics = Metrics::new();
        metrics
            .commands
            .record_command("GET", Duration::from_micros(100));
        metrics.connections.record_connection();

        let output = metrics.export_prometheus();
        assert!(output.contains("aikv_commands_total 1"));
        assert!(output.contains("aikv_connected_clients 1"));
    }
}
