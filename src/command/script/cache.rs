//! SCRIPT LOAD 脚本缓存 (LRU 256; EVAL 不写入)

use parking_lot::RwLock;
use std::collections::{HashMap, VecDeque};

const MAX_SCRIPTS: usize = 256;

#[derive(Clone, Debug)]
pub struct CachedScript {
    pub script: String,
}

pub struct ScriptCache {
    inner: RwLock<ScriptCacheInner>,
}

struct ScriptCacheInner {
    map: HashMap<String, CachedScript>,
    order: VecDeque<String>,
    generation: u64,
}

impl Default for ScriptCache {
    fn default() -> Self {
        Self {
            inner: RwLock::new(ScriptCacheInner {
                map: HashMap::new(),
                order: VecDeque::new(),
                generation: 0,
            }),
        }
    }
}

impl ScriptCache {
    pub fn get(&self, sha1: &str) -> Option<String> {
        let mut inner = self.inner.write();
        if !inner.map.contains_key(sha1) {
            return None;
        }
        inner.order.retain(|s| s != sha1);
        inner.order.push_back(sha1.to_string());
        inner.map.get(sha1).map(|c| c.script.clone())
    }

    pub fn generation(&self) -> u64 {
        self.inner.read().generation
    }

    pub fn insert(&self, sha1: String, script: String) {
        let mut inner = self.inner.write();
        if inner.map.contains_key(&sha1) {
            inner.order.retain(|s| s != &sha1);
            inner.generation = inner.generation.wrapping_add(1);
        } else if inner.map.len() >= MAX_SCRIPTS {
            if let Some(old) = inner.order.pop_front() {
                inner.map.remove(&old);
            }
            inner.generation = inner.generation.wrapping_add(1);
        }
        inner.order.push_back(sha1.clone());
        inner.map.insert(sha1, CachedScript { script });
    }

    pub fn exists(&self, sha1: &str) -> bool {
        self.inner.read().map.contains_key(sha1)
    }

    pub fn flush(&self) {
        let mut inner = self.inner.write();
        inner.map.clear();
        inner.order.clear();
        inner.generation = inner.generation.wrapping_add(1);
    }
}
