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
        self.inner.read().map.get(sha1).is_some_and(|c| c.read_only)
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
/// 仅当脚本中所有 `redis` 访问均为静态只读命令调用时返回 `true`.
/// 若出现非只读命令, 动态拼接, 别名引用 (`local r = redis`) 或未识别调用, 保守判定为 `false`.
pub fn is_script_read_only(script: &str) -> bool {
    let bytes = script.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        i = skip_ws_and_comments(bytes, i);
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let Some((_, next)) = parse_quoted_string(bytes, i) else {
                return false;
            };
            i = next;
        } else if let Some(next) = skip_long_bracket(bytes, i) {
            i = next;
        } else if is_ident_start(bytes[i]) {
            let end = scan_ident_end(bytes, i);
            let ident = &bytes[i..end];
            if matches!(ident, b"_G" | b"load" | b"loadstring") {
                return false;
            }
            if ident == b"redis" {
                let Some(next) = check_redis_access(bytes, end) else {
                    return false;
                };
                i = next;
            } else {
                i = end;
            }
        } else {
            i += 1;
        }
    }
    true
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn scan_ident_end(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && is_ident_continue(bytes[i]) {
        i += 1;
    }
    i
}

fn check_redis_access(bytes: &[u8], after_redis: usize) -> Option<usize> {
    let dot = skip_ws_and_comments(bytes, after_redis);
    if bytes.get(dot) != Some(&b'.') {
        return None;
    }
    let member_start = skip_ws_and_comments(bytes, dot + 1);
    if member_start >= bytes.len() || !is_ident_start(bytes[member_start]) {
        return None;
    }
    let member_end = scan_ident_end(bytes, member_start);
    let member = &bytes[member_start..member_end];
    let paren = skip_ws_and_comments(bytes, member_end);
    if bytes.get(paren) != Some(&b'(') {
        return None;
    }
    if member == b"call" || member == b"pcall" {
        let arg_start = skip_ws_and_comments(bytes, paren + 1);
        let (cmd_name, after_arg) = parse_quoted_string(bytes, arg_start)?;
        if !crate::command::registry::is_readonly_command(&cmd_name) {
            return None;
        }
        Some(after_arg)
    } else if matches!(
        member,
        b"log" | b"sha1hex" | b"status_reply" | b"error_reply"
    ) {
        Some(paren + 1)
    } else {
        None
    }
}

fn skip_ws_and_comments(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            i += 2;
            if let Some(next) = skip_long_bracket(bytes, i) {
                i = next;
            } else {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
        } else {
            break;
        }
    }
    i
}

fn skip_long_bracket(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut i = start + 1;
    let mut eq_count = 0usize;
    while bytes.get(i) == Some(&b'=') {
        eq_count += 1;
        i += 1;
    }
    if bytes.get(i) != Some(&b'[') {
        return None;
    }
    i += 1;
    while i < bytes.len() {
        if bytes[i] == b']' && matches_closing_bracket(bytes, i + 1, eq_count) {
            return Some(i + 2 + eq_count);
        }
        i += 1;
    }
    Some(bytes.len())
}

fn matches_closing_bracket(bytes: &[u8], start: usize, eq_count: usize) -> bool {
    if start + eq_count >= bytes.len() || bytes[start + eq_count] != b']' {
        return false;
    }
    bytes[start..start + eq_count].iter().all(|&b| b == b'=')
}

fn parse_quoted_string(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let quote = *bytes.get(start)?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let mut out = Vec::new();
    let mut i = start + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            i += 1;
            out.push(*bytes.get(i)?);
        } else if b == quote {
            return Some((String::from_utf8_lossy(&out).into_owned(), i + 1));
        } else if b == b'\n' || b == b'\r' {
            return None;
        } else {
            out.push(b);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归测试 (Issue #93): 验证只读脚本静态词法判定, 确保 `local r = redis` 别名与字符串字面量内 `--` / `--[[` 不会绕过写命令检测.
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

        // 别名绕过与字符串内注释符号不得误判为只读
        let aliased = "local r = redis; return r.call('SET', KEYS[1], ARGV[1])";
        assert!(!is_script_read_only(aliased));

        let bracket_call = "return redis['call']('SET', KEYS[1], ARGV[1])";
        assert!(!is_script_read_only(bracket_call));

        let dash_in_string = "local x = '-- '; redis.call('SET', KEYS[1], x); return x";
        assert!(!is_script_read_only(dash_in_string));

        let block_comment_in_string =
            "local x = '--[['; redis.call('SET', KEYS[1], x); local y = ']]'; return y";
        assert!(!is_script_read_only(block_comment_in_string));
    }
}
