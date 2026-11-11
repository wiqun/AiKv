//! 慢查询日志 (Phase 10-ext Step 0)

use parking_lot::RwLock;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn is_evalsha_command(cmd: &str) -> bool {
    cmd.eq_ignore_ascii_case("EVALSHA") || cmd.eq_ignore_ascii_case("EVALSHA_RO")
}

pub const DEFAULT_SLOWLOG_THRESHOLD_US: u64 = 100_000;
pub const DEFAULT_SLOWLOG_MAX_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlowQueryEntry {
    pub id: u64,
    pub timestamp_s: i64,
    pub duration_us: u64,
    pub command: String,
    pub args: Vec<String>,
    pub client_addr: String,
    pub db_index: u16,
}

#[derive(Debug)]
pub struct SlowQueryLog {
    threshold_us: AtomicU64,
    max_entries: AtomicU64,
    next_id: AtomicU64,
    entries: RwLock<VecDeque<SlowQueryEntry>>,
}

impl Default for SlowQueryLog {
    fn default() -> Self {
        Self::new()
    }
}

impl SlowQueryLog {
    pub fn new() -> Self {
        Self::with_settings(DEFAULT_SLOWLOG_MAX_LEN, DEFAULT_SLOWLOG_THRESHOLD_US)
    }

    pub fn with_settings(max_entries: usize, threshold_us: u64) -> Self {
        Self {
            threshold_us: AtomicU64::new(threshold_us),
            max_entries: AtomicU64::new(max_entries as u64),
            next_id: AtomicU64::new(1),
            entries: RwLock::new(VecDeque::with_capacity(max_entries)),
        }
    }

    pub fn threshold_us(&self) -> u64 {
        self.threshold_us.load(Ordering::Relaxed)
    }

    pub fn set_threshold_us(&self, threshold_us: u64) {
        self.threshold_us.store(threshold_us, Ordering::Relaxed);
    }

    pub fn max_entries(&self) -> usize {
        self.max_entries.load(Ordering::Relaxed) as usize
    }

    pub fn set_max_entries(&self, max_entries: usize) {
        self.max_entries
            .store(max_entries as u64, Ordering::Relaxed);
        let mut entries = self.entries.write();
        while entries.len() > max_entries {
            entries.pop_back();
        }
    }

    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn reset(&self) {
        self.entries.write().clear();
    }

    pub fn record(
        &self,
        command: &str,
        args: &[String],
        duration_us: u64,
        client_addr: &str,
        db_index: u16,
    ) {
        if duration_us < self.threshold_us() {
            return;
        }

        let entry = SlowQueryEntry {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            timestamp_s: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            duration_us,
            command: command.to_ascii_uppercase(),
            args: args.to_vec(),
            client_addr: client_addr.to_string(),
            db_index,
        };

        let max_entries = self.max_entries();
        let mut entries = self.entries.write();
        entries.push_front(entry);
        while entries.len() > max_entries {
            entries.pop_back();
        }
    }

    /// `EVALSHA` / `EVALSHA_RO` 慢日志在原参数后附上脚本正文. 其他命令不改参数.
    pub fn append_evalsha_source(cmd: &str, args: &mut Vec<String>, source: Option<String>) {
        if !is_evalsha_command(cmd) {
            return;
        }
        if let Some(source) = source {
            args.push(source);
        }
    }

    /// 返回最近 `count` 条; count ≤ 0 时返回空 Vec
    pub fn get(&self, count: usize) -> Vec<SlowQueryEntry> {
        if count == 0 {
            return Vec::new();
        }
        let entries = self.entries.read();
        entries.iter().take(count).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slowlog_push_get_trim() {
        let log = SlowQueryLog::with_settings(3, 0);

        for i in 0..5 {
            log.record(
                "SET",
                &[format!("k{i}"), format!("v{i}")],
                1000 + i as u64,
                "127.0.0.1:1234",
                0,
            );
        }

        assert_eq!(log.len(), 3);
        let recent = log.get(10);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].id, 5);
        assert_eq!(recent[0].command, "SET");
        assert_eq!(recent[0].client_addr, "127.0.0.1:1234");
        assert_eq!(recent[0].db_index, 0);

        assert!(log.get(0).is_empty());

        log.set_max_entries(1);
        assert_eq!(log.len(), 1);
        assert_eq!(log.get(10)[0].id, 5);

        log.reset();
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn test_slowlog_threshold() {
        let log = SlowQueryLog::with_settings(128, 5000);
        log.record("GET", &["k".into()], 4999, "127.0.0.1:1", 1);
        assert_eq!(log.len(), 0);
        log.record("GET", &["k".into()], 5000, "127.0.0.1:1", 1);
        assert_eq!(log.len(), 1);
        assert_eq!(log.get(1)[0].db_index, 1);
    }

    #[test]
    fn evalsha_slowlog_keeps_sha_and_appends_source() {
        let mut args = vec!["abc".to_string(), "0".to_string()];
        SlowQueryLog::append_evalsha_source("EVALSHA", &mut args, Some("return 1".to_string()));
        assert_eq!(args, vec!["abc", "0", "return 1"]);

        let mut ro = vec!["abc".to_string()];
        SlowQueryLog::append_evalsha_source("evalsha_ro", &mut ro, Some("return 1".to_string()));
        assert_eq!(ro, vec!["abc", "return 1"]);

        let mut missing = vec!["abc".to_string()];
        SlowQueryLog::append_evalsha_source("EVALSHA", &mut missing, None);
        assert_eq!(missing, vec!["abc"]);

        let mut other = vec!["k".to_string()];
        SlowQueryLog::append_evalsha_source("GET", &mut other, Some("return 1".to_string()));
        assert_eq!(other, vec!["k"]);
    }
}
