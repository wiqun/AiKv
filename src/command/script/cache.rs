//! SCRIPT LOAD 脚本缓存 (LRU 256; EVAL 亦写入并做只读解析)

use parking_lot::RwLock;
use std::collections::{HashMap, VecDeque};

const MAX_SCRIPTS: usize = 256;

#[derive(Clone, Debug)]
pub struct CachedScript {
    pub script: String,
    pub read_only: bool,
}

pub struct ScriptCache {
    inner: RwLock<ScriptCacheInner>,
}

struct ScriptCacheInner {
    map: HashMap<String, CachedScript>,
    order: VecDeque<String>,
}

impl Default for ScriptCache {
    fn default() -> Self {
        Self {
            inner: RwLock::new(ScriptCacheInner {
                map: HashMap::new(),
                order: VecDeque::new(),
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

    pub fn insert(&self, sha1: String, script: String) {
        let read_only = is_script_read_only(&script);
        let mut inner = self.inner.write();
        if inner.map.contains_key(&sha1) {
            inner.order.retain(|s| s != &sha1);
        } else if inner.map.len() >= MAX_SCRIPTS {
            if let Some(old) = inner.order.pop_front() {
                inner.map.remove(&old);
            }
        }
        inner.order.push_back(sha1.clone());
        inner.map.insert(sha1, CachedScript { script, read_only });
    }

    pub fn is_read_only(&self, sha1: &str) -> bool {
        self.inner
            .read()
            .map
            .get(sha1)
            .is_some_and(|c| c.read_only)
    }

    pub fn exists(&self, sha1: &str) -> bool {
        self.inner.read().map.contains_key(sha1)
    }

    pub fn flush(&self) {
        let mut inner = self.inner.write();
        inner.map.clear();
        inner.order.clear();
    }
}

/// 静态分析 Lua 脚本是否为只读.
/// 仅当脚本中所有 `redis.call` / `redis.pcall` 均为静态只读命令时返回 `true`.
/// 若出现非只读命令, 动态拼接命令或未识别调用, 则保守判定为 `false`.
pub fn is_script_read_only(script: &str) -> bool {
    let clean = strip_lua_comments(script);
    let mut idx = 0;

    while let Some(pos) = clean[idx..].find("redis.") {
        let abs_pos = idx + pos;
        let rest = &clean[abs_pos + 6..];
        if rest.starts_with("call") || rest.starts_with("pcall") {
            let offset = if rest.starts_with("call") { 4 } else { 5 };
            let call_args = rest[offset..].trim_start();
            if !call_args.starts_with('(') {
                return false;
            }
            let inner = call_args[1..].trim_start();
            let Some((cmd_name, _)) = parse_string_literal(inner) else {
                return false;
            };
            if !crate::command::registry::is_readonly_command(&cmd_name) {
                return false;
            }
            idx = abs_pos + 6 + offset;
        } else if rest.starts_with("log")
            || rest.starts_with("sha1hex")
            || rest.starts_with("status_reply")
            || rest.starts_with("error_reply")
        {
            idx = abs_pos + 6;
        } else {
            return false;
        }
    }
    true
}

/// 去除 Lua 脚本中的注释 (支持 `--` 单行注释与 `--[[ ... ]]` 多行注释).
fn strip_lua_comments(script: &str) -> String {
    let mut out = String::with_capacity(script.len());
    let bytes = script.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if i + 1 < len && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            if i + 3 < len && bytes[i + 2] == b'[' && bytes[i + 3] == b'[' {
                i += 4;
                while i + 1 < len && !(bytes[i] == b']' && bytes[i + 1] == b']') {
                    i += 1;
                }
                i = (i + 2).min(len);
            } else {
                i += 2;
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// 解析 Lua 字符串字面量 (`"..."` 或 `'...'`).
fn parse_string_literal(input: &str) -> Option<(String, usize)> {
    let mut chars = input.char_indices();
    let quote = match chars.next()? {
        (_, '"') => '"',
        (_, '\'') => '\'',
        _ => return None,
    };
    let mut val = String::new();
    let mut escaped = false;
    for (i, ch) in chars {
        if escaped {
            val.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == quote {
            return Some((val, i + 1));
        } else {
            val.push(ch);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_script_read_only() {
        // 无 redis 调用
        assert!(is_script_read_only("return 1"));

        // 只读命令: ZRANGEBYSCORE, ZSCORE
        let s7 = r#"
            local a = redis.call("ZRANGEBYSCORE", KEYS[1], 0, "+inf", "LIMIT", 0, 32)
            local s = redis.call('ZSCORE', KEYS[1], "m1000")
            if s then return #a end
            return 0
        "#;
        assert!(is_script_read_only(s7));

        // 单引号与 pcall 只读
        let pcall_read = "return redis.pcall('GET', KEYS[1])";
        assert!(is_script_read_only(pcall_read));

        // 包含写命令: SET
        let s4 = r#"
            local v = redis.call("get", KEYS[1])
            redis.call("set", KEYS[1], 1)
            return v
        "#;
        assert!(!is_script_read_only(s4));

        // 注释中的写命令应被忽略
        let commented = r#"
            -- redis.call("set", KEYS[1], "foo")
            local v = redis.call("get", KEYS[1])
            return v
        "#;
        assert!(is_script_read_only(commented));

        // 动态变量作为命令名保守判定为写
        let dynamic = "local c = 'GET'; return redis.call(c, KEYS[1])";
        assert!(!is_script_read_only(dynamic));
    }
}
