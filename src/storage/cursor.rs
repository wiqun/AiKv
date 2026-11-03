//! SCAN 族增量游标管理器 (FIFO 容量淘汰 + 60s TTL + 类型隔离)
//!
//! # 设计原则
//! - Redis 协议要求游标为 u64. 0 严格保留为迭代起始/结束标记, 分配从 1 开始.
//! - 内存硬上限: 默认保留最多 4096 条活跃游标 (基于 FIFO 队列约束容量), 单游标 TTL 60 秒.
//! - 严格安全防御: 失效或过期游标明确返回 `ERR invalid cursor`, 严禁返回 0 导致静默漏数据.
//! - 类型隔离: 区分 Global SCAN / Hash / Set / ZSet, 阻止游标跨 DB 或跨结构混用.
//! - 集合标记: 记录 `is_collection`, 使全库扫描在恢复游标时仅对集合键执行 Subkey 跳跃.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;

/// SCAN 命令类型 (用于类型混用防御)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    Global,
    Hash,
    Set,
    ZSet,
}

/// 单个游标元数据
#[derive(Debug, Clone)]
pub struct CursorEntry {
    pub db: usize,
    pub kind: ScanKind,
    pub last_key: Vec<u8>,
    pub is_collection: bool,
    pub expire_at: Instant,
}

pub struct CursorManager {
    next_id: AtomicU64,
    capacity: usize,
    ttl: Duration,
    entries: RwLock<HashMap<u64, CursorEntry>>,
    order: RwLock<VecDeque<u64>>,
}

impl Default for CursorManager {
    fn default() -> Self {
        Self::new(4096, Duration::from_secs(60))
    }
}

impl CursorManager {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            capacity,
            ttl,
            entries: RwLock::new(HashMap::new()),
            order: RwLock::new(VecDeque::new()),
        }
    }

    /// 获取游标对应的起始 key 与集合标识.
    /// - `cursor == 0`: 从头开始, 返回 `Ok(None)`.
    /// - `cursor > 0`: 检索游标. 若未找到、已过期、或 DB / 类型不匹配, 返回 `Err("ERR invalid cursor")`.
    pub fn get(
        &self,
        cursor: u64,
        db: usize,
        kind: ScanKind,
    ) -> Result<Option<(Vec<u8>, bool)>, &'static str> {
        if cursor == 0 {
            return Ok(None);
        }
        let now = Instant::now();
        let entries = self.entries.read();
        let entry = entries.get(&cursor).ok_or("ERR invalid cursor")?;
        if entry.expire_at <= now || entry.db != db || entry.kind != kind {
            return Err("ERR invalid cursor");
        }
        Ok(Some((entry.last_key.clone(), entry.is_collection)))
    }

    /// 分配并记录新的游标. 返回新生成的 cursor_id (非零).
    pub fn put(&self, db: usize, kind: ScanKind, last_key: Vec<u8>, is_collection: bool) -> u64 {
        let mut entries = self.entries.write();
        let mut order = self.order.write();
        let now = Instant::now();

        // 1. 清理队首已过期项
        while let Some(&oldest_id) = order.front() {
            if let Some(entry) = entries.get(&oldest_id) {
                if entry.expire_at <= now {
                    entries.remove(&oldest_id);
                    order.pop_front();
                    continue;
                }
            }
            break;
        }

        // 2. 超出容量上限时挤出最老项 (基于 FIFO 队列淘汰)
        while order.len() >= self.capacity {
            if let Some(evicted_id) = order.pop_front() {
                entries.remove(&evicted_id);
            }
        }

        // 3. 分配非零递增 ID (若溢出跳过 0)
        let id = {
            let mut next = self.next_id.fetch_add(1, Ordering::Relaxed);
            if next == 0 {
                next = self.next_id.fetch_add(1, Ordering::Relaxed);
            }
            next
        };

        let expire_at = now + self.ttl;
        entries.insert(
            id,
            CursorEntry {
                db,
                kind,
                last_key,
                is_collection,
                expire_at,
            },
        );
        order.push_back(id);
        id
    }

    /// 主动清空所有游标 (运维 / 重置测试使用)
    pub fn clear(&self) {
        self.entries.write().clear();
        self.order.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_zero_returns_none() {
        let mgr = CursorManager::default();
        let res = mgr.get(0, 0, ScanKind::Global);
        assert_eq!(res, Ok(None));
    }

    #[test]
    fn test_cursor_allocation_and_lookup_success() {
        let mgr = CursorManager::default();
        let id = mgr.put(0, ScanKind::Global, b"user:100".to_vec(), false);
        assert_ne!(id, 0);

        let item = mgr.get(id, 0, ScanKind::Global).unwrap();
        assert_eq!(item, Some((b"user:100".to_vec(), false)));
    }

    #[test]
    fn test_cursor_invalid_id_returns_error() {
        let mgr = CursorManager::default();
        let res = mgr.get(99999, 0, ScanKind::Global);
        assert_eq!(res, Err("ERR invalid cursor"));
    }

    #[test]
    fn test_cursor_expired_returns_error() {
        let mgr = CursorManager::new(10, Duration::from_millis(1));
        let id = mgr.put(0, ScanKind::Global, b"key".to_vec(), false);
        std::thread::sleep(Duration::from_millis(10));

        let res = mgr.get(id, 0, ScanKind::Global);
        assert_eq!(res, Err("ERR invalid cursor"));
    }

    #[test]
    fn test_cursor_type_and_db_mismatch_returns_error() {
        let mgr = CursorManager::default();
        let id = mgr.put(0, ScanKind::Hash, b"field1".to_vec(), false);

        // 尝试使用 Global SCAN 模式读取 Hash 的游标
        let res1 = mgr.get(id, 0, ScanKind::Global);
        assert_eq!(res1, Err("ERR invalid cursor"));

        // 尝试跨 DB 读取游标
        let res2 = mgr.get(id, 1, ScanKind::Hash);
        assert_eq!(res2, Err("ERR invalid cursor"));
    }

    #[test]
    fn test_cursor_capacity_eviction() {
        let mgr = CursorManager::new(2, Duration::from_secs(60));
        let id1 = mgr.put(0, ScanKind::Global, b"k1".to_vec(), false);
        let id2 = mgr.put(0, ScanKind::Global, b"k2".to_vec(), true);
        let id3 = mgr.put(0, ScanKind::Global, b"k3".to_vec(), false); // 挤出 id1

        assert_eq!(mgr.get(id1, 0, ScanKind::Global), Err("ERR invalid cursor"));
        assert_eq!(
            mgr.get(id2, 0, ScanKind::Global),
            Ok(Some((b"k2".to_vec(), true)))
        );
        assert_eq!(
            mgr.get(id3, 0, ScanKind::Global),
            Ok(Some((b"k3".to_vec(), false)))
        );
    }
}
