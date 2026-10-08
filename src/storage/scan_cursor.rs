//! SCAN 游标记下这次没返回的第一把 key。下次从大于等于它的 key 继续，
//! 中间新插入的更小 key 不会把已经排在后面的 key 挤掉。不进 WAL。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

pub struct ScanCursorTable {
    next_id: AtomicU64,
    cursors: Mutex<HashMap<u64, Vec<u8>>>,
}

impl ScanCursorTable {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            cursors: Mutex::new(HashMap::new()),
        }
    }

    pub fn page(&self, cursor: u64, mut keys: Vec<Vec<u8>>, count: usize) -> (u64, Vec<Vec<u8>>) {
        keys.sort();
        let start = self.start_after(cursor, &keys);
        let end = (start + count).min(keys.len());
        let page = keys[start..end].to_vec();
        if page.is_empty() || end >= keys.len() {
            return (0, page);
        }
        let id = self.alloc_id();
        self.cursors.lock().insert(id, keys[end].clone());
        (id, page)
    }

    fn start_after(&self, cursor: u64, keys: &[Vec<u8>]) -> usize {
        if cursor == 0 {
            return 0;
        }
        let Some(last) = self.cursors.lock().get(&cursor).cloned() else {
            return keys.len();
        };
        keys.iter()
            .position(|key| key.as_slice() >= last.as_slice())
            .unwrap_or(keys.len())
    }

    fn alloc_id(&self) -> u64 {
        loop {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            if id != 0 {
                return id;
            }
        }
    }
}

impl Default for ScanCursorTable {
    fn default() -> Self {
        Self::new()
    }
}
