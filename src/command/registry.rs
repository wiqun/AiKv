//! 全量命令元数据表 (~131+ 条, 含 JSON/Lua/Server/Cluster 条目), 供 COMMAND 子命令、
//! ATOM WATCH 键追踪与写命令判定、cluster 路由等消费 (见 `router/` / `server.rs`).
//!
//! # 数据模型
//!
//! ```text
//! static COMMAND_TABLE: &[CommandInfo]
//!   CommandInfo { name, arity, flags, first_key, last_key, step }
//!     arity < 0         → 至少 |arity| 个参数
//!     first/last/step   → key 参数定位 (Redis COMMAND GETKEYS 语义)
//! COMMAND_INDEX: OnceLock<HashMap<name, CommandInfo>> 惰性建索引
//! ```
//!
//! # 接口
//!
//! - `lookup(name)`: 大小写不敏感查表.
//! - `key_indices(info, argc)`: 计算 key 参数位置; `first_key=0` → 无 key.
//! - `all_commands()` / `command_count()`: COMMAND 子命令数据源.
//!
//! # Invariant
//!
//! - registry ↔ router 双维护: 新增命令必须同时更新 `COMMAND_TABLE` 与 `router/mod.rs`
//!   `execute_inner` 的 match (或子 dispatch), 否则表内命中但路由漏分发.

use std::collections::HashMap;

use bytes::Bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandInfo {
    pub name: &'static str,
    /// 参数数量; 负值表示至少 |arity| 个参数
    pub arity: i64,
    pub flags: &'static [&'static str],
    pub first_key: i64,
    pub last_key: i64,
    pub step: i64,
}

macro_rules! cmd {
    ($name:expr, $arity:expr, $flags:expr, $first:expr, $last:expr, $step:expr) => {
        CommandInfo {
            name: $name,
            arity: $arity,
            flags: $flags,
            first_key: $first,
            last_key: $last,
            step: $step,
        }
    };
}

static COMMAND_TABLE: &[CommandInfo] = &[
    // String
    cmd!("GET", 2, &["readonly", "fast"], 1, 1, 1),
    cmd!("SET", -3, &["write", "denyoom"], 1, 1, 1),
    cmd!("MGET", -2, &["readonly", "fast"], 1, -1, 1),
    cmd!("MSET", -3, &["write", "denyoom"], 1, -1, 2),
    cmd!("MSETNX", -3, &["write", "denyoom"], 1, -1, 2),
    cmd!("DEL", -2, &["write"], 1, -1, 1),
    cmd!("UNLINK", -2, &["write"], 1, -1, 1),
    cmd!("EXISTS", -2, &["readonly", "fast"], 1, -1, 1),
    cmd!("STRLEN", 2, &["readonly", "fast"], 1, 1, 1),
    cmd!("GETRANGE", 4, &["readonly", "fast"], 1, 1, 1),
    cmd!("GETSET", 3, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("SUBSTR", 4, &["readonly", "fast"], 1, 1, 1),
    cmd!("SETRANGE", 4, &["write", "denyoom"], 1, 1, 1),
    cmd!("SETBIT", 4, &["write", "denyoom"], 1, 1, 1),
    cmd!("GETBIT", 3, &["readonly", "fast"], 1, 1, 1),
    cmd!("BITCOUNT", -2, &["readonly"], 1, 1, 1),
    cmd!("BITFIELD", -2, &["write", "denyoom"], 1, 1, 1),
    cmd!("BITFIELD_RO", -2, &["readonly", "fast"], 1, 1, 1),
    cmd!("APPEND", 3, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("INCR", 2, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("DECR", 2, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("INCRBY", 3, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("DECRBY", 3, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("INCRBYFLOAT", 3, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("GETDEL", 2, &["write", "fast"], 1, 1, 1),
    cmd!("GETEX", -2, &["write", "fast"], 1, 1, 1),
    cmd!("SETNX", 3, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("SETEX", 4, &["write", "denyoom"], 1, 1, 1),
    cmd!("PSETEX", 4, &["write", "denyoom"], 1, 1, 1),
    // JSON
    cmd!("JSON.GET", -2, &["readonly"], 1, 1, 1),
    cmd!("JSON.MGET", -2, &["readonly"], 1, -2, 1),
    cmd!("JSON.SET", -4, &["write", "denyoom"], 1, 1, 1),
    cmd!("JSON.DEL", -2, &["write"], 1, 1, 1),
    cmd!("JSON.TYPE", -2, &["readonly"], 1, 1, 1),
    cmd!("JSON.STRLEN", -2, &["readonly"], 1, 1, 1),
    cmd!("JSON.ARRLEN", -2, &["readonly"], 1, 1, 1),
    cmd!("JSON.OBJLEN", -2, &["readonly"], 1, 1, 1),
    cmd!("JSON.NUMINCRBY", -3, &["write"], 1, 1, 1),
    cmd!("JSON.ARRAPPEND", -3, &["write", "denyoom"], 1, 1, 1),
    cmd!("JSON.UPDATE", -4, &["write", "denyoom"], 1, 1, 1),
    cmd!("JSON.MSET", -3, &["write", "denyoom"], 1, -1, 3),
    // Lua
    cmd!("EVAL", -3, &["write", "denyoom", "noscript"], 2, 2, 1),
    cmd!("EVALSHA", -3, &["write", "denyoom", "noscript"], 2, 2, 1),
    cmd!("SCRIPT", -2, &["write", "noscript"], 0, 0, 0),
    // Hash
    cmd!("HSET", -4, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("HMSET", -4, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("HGET", 3, &["readonly", "fast"], 1, 1, 1),
    cmd!("HDEL", -3, &["write", "fast"], 1, 1, 1),
    cmd!("HEXISTS", 3, &["readonly", "fast"], 1, 1, 1),
    cmd!("HLEN", 2, &["readonly", "fast"], 1, 1, 1),
    cmd!("HKEYS", 2, &["readonly"], 1, 1, 1),
    cmd!("HVALS", 2, &["readonly"], 1, 1, 1),
    cmd!("HGETALL", 2, &["readonly"], 1, 1, 1),
    cmd!("HMGET", -3, &["readonly", "fast"], 1, 1, 1),
    cmd!("HSETNX", 4, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("HINCRBY", 4, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("HINCRBYFLOAT", 4, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("HSCAN", -3, &["readonly"], 1, 1, 1),
    // List
    cmd!("LPUSH", -3, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("LPUSHX", -3, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("RPUSH", -3, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("RPUSHX", -3, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("LPOP", -2, &["write", "fast"], 1, 1, 1),
    cmd!("RPOP", -2, &["write", "fast"], 1, 1, 1),
    cmd!("LLEN", 2, &["readonly", "fast"], 1, 1, 1),
    cmd!("LRANGE", 4, &["readonly"], 1, 1, 1),
    cmd!("LINDEX", 3, &["readonly"], 1, 1, 1),
    cmd!("LSET", 4, &["write", "denyoom"], 1, 1, 1),
    cmd!("LREM", 4, &["write"], 1, 1, 1),
    cmd!("LTRIM", 4, &["write"], 1, 1, 1),
    cmd!("LINSERT", 5, &["write", "denyoom"], 1, 1, 1),
    cmd!("LMOVE", 5, &["write", "denyoom"], 1, 2, 1),
    cmd!("RPOPLPUSH", 3, &["write", "denyoom"], 1, 2, 1),
    cmd!("LPOS", -3, &["readonly"], 1, 1, 1),
    cmd!("BLPOP", -3, &["write", "blocking"], 1, -2, 1),
    cmd!("BRPOP", -3, &["write", "blocking"], 1, -2, 1),
    cmd!("BLMOVE", 6, &["write", "denyoom", "blocking"], 1, 2, 1),
    cmd!("BRPOPLPUSH", 4, &["write", "denyoom", "blocking"], 1, 2, 1),
    // Set
    cmd!("SADD", -3, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("SREM", -3, &["write", "fast"], 1, 1, 1),
    cmd!("SISMEMBER", 3, &["readonly", "fast"], 1, 1, 1),
    cmd!("SMISMEMBER", -3, &["readonly", "fast"], 1, 1, 1),
    cmd!("SMEMBERS", 2, &["readonly"], 1, 1, 1),
    cmd!("SCARD", 2, &["readonly", "fast"], 1, 1, 1),
    cmd!("SPOP", -2, &["write", "fast"], 1, 1, 1),
    cmd!("SRANDMEMBER", -2, &["readonly"], 1, 1, 1),
    cmd!("SUNION", -2, &["readonly"], 1, -1, 1),
    cmd!("SINTER", -2, &["readonly"], 1, -1, 1),
    cmd!("SINTERCARD", -3, &["readonly"], 0, 0, 0),
    cmd!("SDIFF", -2, &["readonly"], 1, -1, 1),
    cmd!("SUNIONSTORE", -3, &["write", "denyoom"], 1, -1, 1),
    cmd!("SINTERSTORE", -3, &["write", "denyoom"], 1, -1, 1),
    cmd!("SDIFFSTORE", -3, &["write", "denyoom"], 1, -1, 1),
    cmd!("SMOVE", 4, &["write", "fast"], 1, 2, 1),
    cmd!("SSCAN", -3, &["readonly"], 1, 1, 1),
    // ZSet
    cmd!("ZADD", -4, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("ZREM", -3, &["write", "fast"], 1, 1, 1),
    cmd!("ZSCORE", 3, &["readonly", "fast"], 1, 1, 1),
    cmd!("ZMSCORE", -3, &["readonly", "fast"], 1, 1, 1),
    cmd!("ZRANK", 3, &["readonly", "fast"], 1, 1, 1),
    cmd!("ZREVRANK", 3, &["readonly", "fast"], 1, 1, 1),
    cmd!("ZRANGE", -4, &["readonly"], 1, 1, 1),
    cmd!("ZREVRANGE", -4, &["readonly"], 1, 1, 1),
    cmd!("ZRANGEBYSCORE", -4, &["readonly"], 1, 1, 1),
    cmd!("ZREVRANGEBYSCORE", -4, &["readonly"], 1, 1, 1),
    cmd!("ZCARD", 2, &["readonly", "fast"], 1, 1, 1),
    cmd!("ZCOUNT", 4, &["readonly", "fast"], 1, 1, 1),
    cmd!("ZINCRBY", 4, &["write", "denyoom", "fast"], 1, 1, 1),
    cmd!("ZSCAN", -3, &["readonly"], 1, 1, 1),
    cmd!("ZPOPMIN", -2, &["write", "fast"], 1, 1, 1),
    cmd!("ZPOPMAX", -2, &["write", "fast"], 1, 1, 1),
    cmd!("BZPOPMIN", -3, &["write", "blocking"], 1, -2, 1),
    cmd!("BZPOPMAX", -3, &["write", "blocking"], 1, -2, 1),
    cmd!("ZRANGEBYLEX", -4, &["readonly"], 1, 1, 1),
    cmd!("ZREVRANGEBYLEX", -4, &["readonly"], 1, 1, 1),
    cmd!("ZLEXCOUNT", 4, &["readonly", "fast"], 1, 1, 1),
    cmd!("ZREMRANGEBYLEX", 4, &["write", "fast"], 1, 1, 1),
    cmd!("ZREMRANGEBYRANK", 4, &["write", "fast"], 1, 1, 1),
    cmd!("ZREMRANGEBYSCORE", 4, &["write", "fast"], 1, 1, 1),
    cmd!("ZINTER", -3, &["readonly"], 1, -1, 1),
    cmd!("ZINTERSTORE", -4, &["write", "denyoom"], 0, 0, 0),
    cmd!("ZUNION", -3, &["readonly"], 1, -1, 1),
    cmd!("ZUNIONSTORE", -4, &["write", "denyoom"], 0, 0, 0),
    cmd!("ZDIFF", -3, &["readonly"], 1, -1, 1),
    cmd!("ZDIFFSTORE", -4, &["write", "denyoom"], 0, 0, 0),
    // Database
    cmd!("SELECT", 2, &["fast"], 0, 0, 0),
    cmd!("DBSIZE", 1, &["readonly", "fast"], 0, 0, 0),
    cmd!("FLUSHDB", -1, &["write"], 0, 0, 0),
    cmd!("FLUSHALL", -1, &["write"], 0, 0, 0),
    cmd!("SWAPDB", 3, &["write", "fast"], 0, 0, 0),
    cmd!("MOVE", 3, &["write", "fast"], 1, 1, 1),
    // Key
    cmd!("EXPIRE", -3, &["write", "fast"], 1, 1, 1),
    cmd!("EXPIREAT", -3, &["write", "fast"], 1, 1, 1),
    cmd!("PEXPIRE", -3, &["write", "fast"], 1, 1, 1),
    cmd!("PEXPIREAT", -3, &["write", "fast"], 1, 1, 1),
    cmd!("TTL", 2, &["readonly", "fast"], 1, 1, 1),
    cmd!("PTTL", 2, &["readonly", "fast"], 1, 1, 1),
    cmd!("PERSIST", 2, &["write", "fast"], 1, 1, 1),
    cmd!("KEYS", 2, &["readonly"], 0, 0, 0),
    cmd!("SCAN", -2, &["readonly"], 0, 0, 0),
    cmd!("RANDOMKEY", 1, &["readonly"], 0, 0, 0),
    cmd!("RENAME", 3, &["write"], 1, 2, 1),
    cmd!("RENAMENX", 3, &["write", "fast"], 1, 2, 1),
    cmd!("TYPE", 2, &["readonly", "fast"], 1, 1, 1),
    cmd!("COPY", -3, &["write", "denyoom"], 1, 2, 1),
    cmd!("EXPIRETIME", 2, &["readonly", "fast"], 1, 1, 1),
    cmd!("PEXPIRETIME", 2, &["readonly", "fast"], 1, 1, 1),
    cmd!("DUMP", 2, &["readonly", "fast"], 1, 1, 1),
    cmd!("RESTORE", -4, &["write", "denyoom"], 1, 1, 1),
    cmd!("MIGRATE", -6, &["write", "movablekeys"], 3, 3, 1),
    // Server (router)
    cmd!("INFO", -1, &["stale", "fast"], 0, 0, 0),
    cmd!("TIME", 1, &["fast", "stale"], 0, 0, 0),
    cmd!("CONFIG", -2, &["admin", "stale"], 0, 0, 0),
    cmd!("OBJECT", -2, &["readonly", "fast"], 2, 2, 1),
    cmd!("CLIENT", -2, &["admin", "stale"], 0, 0, 0),
    cmd!("LATENCY", -2, &["admin", "stale"], 0, 0, 0),
    cmd!("SLOWLOG", -2, &["admin"], 0, 0, 0),
    cmd!("COMMAND", -1, &["random", "loading"], 0, 0, 0),
    // Connection inline
    cmd!("PING", -1, &["fast", "stale"], 0, 0, 0),
    cmd!("ECHO", 2, &["fast"], 0, 0, 0),
    cmd!("HELLO", -1, &["fast", "stale"], 0, 0, 0),
    cmd!("QUIT", 1, &["fast"], 0, 0, 0),
    cmd!("MONITOR", 1, &["admin"], 0, 0, 0),
    // 事务 (inline, connection-level) — 标准 Redis 别名与 ATOM.* 共用实现
    cmd!("MULTI", 1, &["fast"], 0, 0, 0),
    cmd!("EXEC", 1, &["fast"], 0, 0, 0),
    cmd!("DISCARD", 1, &["fast"], 0, 0, 0),
    cmd!("WATCH", -2, &["fast"], 1, -1, 1),
    cmd!("UNWATCH", 1, &["fast"], 0, 0, 0),
    cmd!("ATOM.MULTI", 1, &["fast"], 0, 0, 0),
    cmd!("ATOM.EXEC", 1, &["fast"], 0, 0, 0),
    cmd!("ATOM.DISCARD", 1, &["fast"], 0, 0, 0),
    cmd!("ATOM.WATCH", -2, &["fast"], 1, -1, 1),
    cmd!("ATOM.UNWATCH", 1, &["fast"], 0, 0, 0),
    // Persistence (11.6′)
    cmd!("SAVE", 1, &["admin"], 0, 0, 0),
    cmd!("BGSAVE", 1, &["admin"], 0, 0, 0),
    cmd!("LASTSAVE", 1, &["admin", "fast"], 0, 0, 0),
    cmd!("SHUTDOWN", -1, &["admin"], 0, 0, 0),
    #[cfg(feature = "cluster")]
    cmd!("CLUSTER", -2, &["readonly", "movablekeys"], 2, 2, 1),
    #[cfg(feature = "cluster")]
    cmd!("READONLY", 1, &["readonly", "fast"], 0, 0, 0),
    #[cfg(feature = "cluster")]
    cmd!("READWRITE", 1, &["readonly", "fast"], 0, 0, 0),
    #[cfg(feature = "cluster")]
    cmd!("ASKING", 1, &["readonly", "fast"], 0, 0, 0),
];

static COMMAND_INDEX: std::sync::OnceLock<HashMap<&'static str, CommandInfo>> =
    std::sync::OnceLock::new();

fn command_index() -> &'static HashMap<&'static str, CommandInfo> {
    COMMAND_INDEX.get_or_init(|| {
        COMMAND_TABLE
            .iter()
            .map(|info| (info.name, *info))
            .collect()
    })
}

pub fn lookup(name: &str) -> Option<CommandInfo> {
    let upper = name.to_ascii_uppercase();
    command_index().get(upper.as_str()).copied()
}

/// 命令表里的名字返回那条 `&'static str`. 表里没有的返回静态 `unknown`.
pub fn canonical_command(name: &str) -> &'static str {
    for info in COMMAND_TABLE {
        if info.name.eq_ignore_ascii_case(name) {
            return info.name;
        }
    }
    "unknown"
}

pub fn all_commands() -> &'static [CommandInfo] {
    COMMAND_TABLE
}

pub fn command_count() -> usize {
    COMMAND_TABLE.len()
}

/// 计算命令参数中的 key 索引 (Redis COMMAND GETKEYS 语义)
pub fn key_indices(info: &CommandInfo, argc: usize) -> Vec<usize> {
    if info.first_key == 0 {
        return Vec::new();
    }
    let last_key = if info.last_key >= 0 {
        info.last_key
    } else {
        (argc as i64) + info.last_key
    };
    if last_key < info.first_key {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pos = info.first_key;
    while pos <= last_key {
        if pos > 0 && (pos as usize) < argc {
            out.push(pos as usize);
        }
        if info.step == 0 {
            break;
        }
        pos += info.step;
    }
    out
}

pub fn command_keys<'a>(cmd: &str, args: &'a [Bytes]) -> Vec<&'a [u8]> {
    match cmd.to_ascii_uppercase().as_str() {
        "EVAL" | "EVALSHA" => counted_keys(args, 1, 2),
        "ZINTER" | "ZUNION" | "ZDIFF" => counted_keys(args, 0, 1),
        "SINTERCARD" => counted_keys(args, 0, 1),
        "ZINTERSTORE" | "ZUNIONSTORE" | "ZDIFFSTORE" => {
            let mut keys = Vec::new();
            if let Some(dest) = args.first() {
                keys.push(dest.as_ref());
            }
            keys.extend(counted_keys(args, 1, 2));
            keys
        }
        _ => lookup(cmd)
            .map(|info| {
                key_indices(&info, args.len() + 1)
                    .into_iter()
                    .filter_map(|index| args.get(index - 1).map(Bytes::as_ref))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn counted_keys(args: &[Bytes], count_index: usize, first_key_index: usize) -> Vec<&[u8]> {
    let Some(raw_count) = args.get(count_index) else {
        return Vec::new();
    };
    let Ok(raw_count) = std::str::from_utf8(raw_count) else {
        return Vec::new();
    };
    let Ok(count) = raw_count.parse::<usize>() else {
        return Vec::new();
    };
    let Some(end) = first_key_index.checked_add(count) else {
        return Vec::new();
    };
    let Some(keys) = args.get(first_key_index..end) else {
        return Vec::new();
    };
    keys.iter().map(Bytes::as_ref).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(values: &[&'static str]) -> Vec<Bytes> {
        values
            .iter()
            .map(|value| Bytes::from_static(value.as_bytes()))
            .collect()
    }

    /// Issue #78: 事务预检必须统一提取静态与动态 key, 避免漏检跨 slot 命令.
    #[test]
    fn test_command_keys_static_and_dynamic() {
        let mset = bytes(&["k1", "v1", "k2", "v2"]);
        assert_eq!(command_keys("MSET", &mset), vec![b"k1".as_slice(), b"k2"]);

        let eval = bytes(&["return 1", "2", "k1", "k2", "arg"]);
        assert_eq!(command_keys("EVAL", &eval), vec![b"k1".as_slice(), b"k2"]);
        let eval_without_keys = bytes(&["return 1", "0", "arg"]);
        assert!(command_keys("EVAL", &eval_without_keys).is_empty());

        let zinter = bytes(&["2", "z1", "z2", "WEIGHTS", "1", "2"]);
        assert_eq!(
            command_keys("ZINTER", &zinter),
            vec![b"z1".as_slice(), b"z2"]
        );
        let zdiff = bytes(&["2", "z1", "z2", "WITHSCORES"]);
        assert_eq!(command_keys("ZDIFF", &zdiff), vec![b"z1".as_slice(), b"z2"]);

        let invalid = bytes(&["not-a-number", "z1"]);
        assert!(command_keys("ZUNION", &invalid).is_empty());
        let out_of_bounds = bytes(&["3", "z1", "z2"]);
        assert!(command_keys("ZUNION", &out_of_bounds).is_empty());

        let sintercard = bytes(&["2", "s1", "s2", "LIMIT", "1"]);
        assert_eq!(
            command_keys("SINTERCARD", &sintercard),
            vec![b"s1".as_slice(), b"s2"]
        );
        let zunionstore = bytes(&["out", "2", "z1", "z2", "WEIGHTS", "1", "2"]);
        assert_eq!(
            command_keys("ZUNIONSTORE", &zunionstore),
            vec![b"out".as_slice(), b"z1", b"z2"]
        );
    }

    #[test]
    fn test_registry_lookup() {
        let get = lookup("get").unwrap();
        assert_eq!(get.name, "GET");
        assert_eq!(get.arity, 2);
        assert!(get.flags.contains(&"readonly"));

        let mset = lookup("MSET").unwrap();
        assert_eq!(mset.first_key, 1);
        assert_eq!(mset.step, 2);

        assert!(lookup("nosuch").is_none());
        assert!(command_count() >= 131);
    }

    #[test]
    fn test_key_indices() {
        let mset = lookup("MSET").unwrap();
        assert_eq!(key_indices(&mset, 5), vec![1, 3]);
        let get = lookup("GET").unwrap();
        assert_eq!(key_indices(&get, 2), vec![1]);
    }
}
