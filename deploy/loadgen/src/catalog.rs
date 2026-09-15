//! 发压命令目录: 与 aikv `COMMAND_TABLE` 对齐的可调度命令.
//!
//! 类型正交 (`str`/`hash`/`list`/`set`/`zset`/`json`/`int`) + 意图分层
//! (`key`/`miss`/`ttl`/`churn`) 保证命中率不被 DEL/TTL 腐蚀, 且不会 WRONGTYPE.
//! 阻塞命令降级为非阻塞等价物; 会毁掉连接或实例的命令不进入目录.

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueType {
    String = 0,
    Hash = 1,
    List = 2,
    Set = 3,
    ZSet = 4,
    Json = 5,
    Int = 6,
    None = 7,
}

impl ValueType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "str",
            Self::Hash => "hash",
            Self::List => "list",
            Self::Set => "set",
            Self::ZSet => "zset",
            Self::Json => "json",
            Self::Int => "int",
            Self::None => "none",
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpKind {
    /// 推进主数据段水位 (只写不删、不带 TTL)
    Populate,
    /// 挥发段写入 (SET EX / SETEX / JSON.SET+EX)
    PopulateTtl,
    /// 命中读走 main, 未命中走 miss
    HitMiss,
    /// 扰动段删除/弹出
    Churn,
    /// 对已有 ttl 段 key 做 EXPIRE/GETEX/PERSIST/TTL
    Ttl,
    /// 整型段 INCR 族
    Int,
    /// 无 key (PING / INFO / SCAN 等)
    NoKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArgTmpl {
    KeyOnly,
    KeyValue,
    KeyField,
    KeyFieldValue,
    KeyMember,
    KeyScoreMember,
    KeyRange,
    KeyIndex,
    KeyBitGet,
    KeyBitSet,
    KeyRangeBytes,
    KeySetRange,
    KeyIncrBy,
    KeyIncrByFloat,
    KeyHashIncr,
    KeyHashIncrFloat,
    JsonRoot,
    JsonSet,
    JsonPathStr,
    JsonPathArr,
    JsonPathObj,
    JsonPathNum,
    JsonNumIncr,
    JsonArrAppend,
    JsonUpdate,
    JsonMset,
    JsonMget,
    Mset,
    Mget,
    TwoKey,
    Lmove,
    Expire,
    Pexpire,
    ExpireAt,
    PexpireAt,
    Getex,
    Setex,
    Psetex,
    Scan,
    KeyScan,
    Ping,
    Echo,
    Time,
    Dbsize,
    Info,
    Command,
    Lastsave,
    Eval,
    ClusterInfo,
    ConfigGet,
    ClientId,
    ScriptExists,
    Asking,
    Readonly,
    Readwrite,
    Randomkey,
    Persist,
    Ttl,
    Pttl,
    TypeKey,
    Dump,
    ObjectEncoding,
    Zcount,
    ZlexCount,
    ZrangeByScore,
    ZrangeByLex,
    Union,
    UnionStore,
    Zinter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommandSpec {
    pub mix_key: &'static str,
    pub redis: &'static str,
    pub group: &'static str,
    pub value_type: ValueType,
    pub kind: OpKind,
    pub tmpl: ArgTmpl,
}

macro_rules! spec {
    ($mix:expr, $redis:expr, $group:expr, $ty:ident, $kind:ident, $tmpl:ident) => {
        CommandSpec {
            mix_key: $mix,
            redis: $redis,
            group: $group,
            value_type: ValueType::$ty,
            kind: OpKind::$kind,
            tmpl: ArgTmpl::$tmpl,
        }
    };
}

/// 可调度命令. mix_key = 前端小写且 `.` → `_`.
pub static CATALOG: &[CommandSpec] = &[
    // String
    spec!("get", "GET", "string", String, HitMiss, KeyOnly),
    spec!("set", "SET", "string", String, Populate, KeyValue),
    spec!("mget", "MGET", "string", String, HitMiss, Mget),
    spec!("mset", "MSET", "string", String, Populate, Mset),
    spec!("del", "DEL", "string", String, Churn, KeyOnly),
    spec!("exists", "EXISTS", "string", String, HitMiss, KeyOnly),
    spec!("strlen", "STRLEN", "string", String, HitMiss, KeyOnly),
    spec!("getrange", "GETRANGE", "string", String, HitMiss, KeyRangeBytes),
    spec!("setrange", "SETRANGE", "string", String, Populate, KeySetRange),
    spec!("setbit", "SETBIT", "string", String, Populate, KeyBitSet),
    spec!("getbit", "GETBIT", "string", String, HitMiss, KeyBitGet),
    spec!("append", "APPEND", "string", String, Populate, KeyValue),
    spec!("incr", "INCR", "string", Int, Int, KeyOnly),
    spec!("decr", "DECR", "string", Int, Int, KeyOnly),
    spec!("incrby", "INCRBY", "string", Int, Int, KeyIncrBy),
    spec!("decrby", "DECRBY", "string", Int, Int, KeyIncrBy),
    spec!("incrbyfloat", "INCRBYFLOAT", "string", Int, Int, KeyIncrByFloat),
    spec!("getdel", "GETDEL", "string", String, Churn, KeyOnly),
    spec!("getex", "GETEX", "string", String, Ttl, Getex),
    spec!("setnx", "SETNX", "string", String, Populate, KeyValue),
    spec!("setex", "SETEX", "string", String, PopulateTtl, Setex),
    spec!("psetex", "PSETEX", "string", String, PopulateTtl, Psetex),
    // JSON (底层是 String KV, 仍隔离命名空间以免解析失败)
    spec!("json_get", "JSON.GET", "json", Json, HitMiss, JsonRoot),
    spec!("json_mget", "JSON.MGET", "json", Json, HitMiss, JsonMget),
    spec!("json_set", "JSON.SET", "json", Json, Populate, JsonSet),
    spec!("json_del", "JSON.DEL", "json", Json, Churn, JsonRoot),
    spec!("json_type", "JSON.TYPE", "json", Json, HitMiss, JsonRoot),
    spec!("json_strlen", "JSON.STRLEN", "json", Json, HitMiss, JsonPathStr),
    spec!("json_arrlen", "JSON.ARRLEN", "json", Json, HitMiss, JsonPathArr),
    spec!("json_objlen", "JSON.OBJLEN", "json", Json, HitMiss, JsonPathObj),
    spec!("json_numincrby", "JSON.NUMINCRBY", "json", Json, Populate, JsonNumIncr),
    spec!("json_arrappend", "JSON.ARRAPPEND", "json", Json, Populate, JsonArrAppend),
    spec!("json_update", "JSON.UPDATE", "json", Json, Populate, JsonUpdate),
    spec!("json_mset", "JSON.MSET", "json", Json, Populate, JsonMset),
    // Hash
    spec!("hset", "HSET", "hash", Hash, Populate, KeyFieldValue),
    spec!("hmset", "HMSET", "hash", Hash, Populate, KeyFieldValue),
    spec!("hget", "HGET", "hash", Hash, HitMiss, KeyField),
    spec!("hdel", "HDEL", "hash", Hash, Churn, KeyField),
    spec!("hexists", "HEXISTS", "hash", Hash, HitMiss, KeyField),
    spec!("hlen", "HLEN", "hash", Hash, HitMiss, KeyOnly),
    spec!("hkeys", "HKEYS", "hash", Hash, HitMiss, KeyOnly),
    spec!("hvals", "HVALS", "hash", Hash, HitMiss, KeyOnly),
    spec!("hgetall", "HGETALL", "hash", Hash, HitMiss, KeyOnly),
    spec!("hmget", "HMGET", "hash", Hash, HitMiss, KeyField),
    spec!("hsetnx", "HSETNX", "hash", Hash, Populate, KeyFieldValue),
    spec!("hincrby", "HINCRBY", "hash", Hash, Populate, KeyHashIncr),
    spec!("hincrbyfloat", "HINCRBYFLOAT", "hash", Hash, Populate, KeyHashIncrFloat),
    spec!("hscan", "HSCAN", "hash", Hash, HitMiss, KeyScan),
    // List (BLPOP/BRPOP/BLMOVE 降级为非阻塞)
    spec!("lpush", "LPUSH", "list", List, Populate, KeyValue),
    spec!("rpush", "RPUSH", "list", List, Populate, KeyValue),
    spec!("lpop", "LPOP", "list", List, Churn, KeyOnly),
    spec!("rpop", "RPOP", "list", List, Churn, KeyOnly),
    spec!("llen", "LLEN", "list", List, HitMiss, KeyOnly),
    spec!("lrange", "LRANGE", "list", List, HitMiss, KeyRange),
    spec!("lindex", "LINDEX", "list", List, HitMiss, KeyIndex),
    spec!("lset", "LSET", "list", List, Populate, KeyIndex),
    spec!("lrem", "LREM", "list", List, Churn, KeyMember),
    spec!("ltrim", "LTRIM", "list", List, Churn, KeyRange),
    spec!("linsert", "LINSERT", "list", List, Populate, KeyMember),
    spec!("lmove", "LMOVE", "list", List, Churn, Lmove),
    spec!("lpos", "LPOS", "list", List, HitMiss, KeyMember),
    spec!("blpop", "LPOP", "list", List, Churn, KeyOnly),
    spec!("brpop", "RPOP", "list", List, Churn, KeyOnly),
    spec!("blmove", "LMOVE", "list", List, Churn, Lmove),
    // Set
    spec!("sadd", "SADD", "set", Set, Populate, KeyMember),
    spec!("srem", "SREM", "set", Set, Churn, KeyMember),
    spec!("sismember", "SISMEMBER", "set", Set, HitMiss, KeyMember),
    spec!("smembers", "SMEMBERS", "set", Set, HitMiss, KeyOnly),
    spec!("scard", "SCARD", "set", Set, HitMiss, KeyOnly),
    spec!("spop", "SPOP", "set", Set, Churn, KeyOnly),
    spec!("srandmember", "SRANDMEMBER", "set", Set, HitMiss, KeyOnly),
    spec!("sunion", "SUNION", "set", Set, HitMiss, Union),
    spec!("sinter", "SINTER", "set", Set, HitMiss, Union),
    spec!("sdiff", "SDIFF", "set", Set, HitMiss, Union),
    spec!("sunionstore", "SUNIONSTORE", "set", Set, Populate, UnionStore),
    spec!("sinterstore", "SINTERSTORE", "set", Set, Populate, UnionStore),
    spec!("sdiffstore", "SDIFFSTORE", "set", Set, Populate, UnionStore),
    spec!("smove", "SMOVE", "set", Set, Churn, TwoKey),
    spec!("sscan", "SSCAN", "set", Set, HitMiss, KeyScan),
    // ZSet (BZPOP* 降级为 ZPOP*)
    spec!("zadd", "ZADD", "zset", ZSet, Populate, KeyScoreMember),
    spec!("zrem", "ZREM", "zset", ZSet, Churn, KeyMember),
    spec!("zscore", "ZSCORE", "zset", ZSet, HitMiss, KeyMember),
    spec!("zrank", "ZRANK", "zset", ZSet, HitMiss, KeyMember),
    spec!("zrevrank", "ZREVRANK", "zset", ZSet, HitMiss, KeyMember),
    spec!("zrange", "ZRANGE", "zset", ZSet, HitMiss, KeyRange),
    spec!("zrevrange", "ZREVRANGE", "zset", ZSet, HitMiss, KeyRange),
    spec!("zrangebyscore", "ZRANGEBYSCORE", "zset", ZSet, HitMiss, ZrangeByScore),
    spec!("zrevrangebyscore", "ZREVRANGEBYSCORE", "zset", ZSet, HitMiss, ZrangeByScore),
    spec!("zcard", "ZCARD", "zset", ZSet, HitMiss, KeyOnly),
    spec!("zcount", "ZCOUNT", "zset", ZSet, HitMiss, Zcount),
    spec!("zincrby", "ZINCRBY", "zset", ZSet, Populate, KeyScoreMember),
    spec!("zscan", "ZSCAN", "zset", ZSet, HitMiss, KeyScan),
    spec!("zpopmin", "ZPOPMIN", "zset", ZSet, Churn, KeyOnly),
    spec!("zpopmax", "ZPOPMAX", "zset", ZSet, Churn, KeyOnly),
    spec!("bzpopmin", "ZPOPMIN", "zset", ZSet, Churn, KeyOnly),
    spec!("bzpopmax", "ZPOPMAX", "zset", ZSet, Churn, KeyOnly),
    spec!("zrangebylex", "ZRANGEBYLEX", "zset", ZSet, HitMiss, ZrangeByLex),
    spec!("zrevrangebylex", "ZREVRANGEBYLEX", "zset", ZSet, HitMiss, ZrangeByLex),
    spec!("zlexcount", "ZLEXCOUNT", "zset", ZSet, HitMiss, ZlexCount),
    spec!("zinter", "ZINTER", "zset", ZSet, HitMiss, Zinter),
    spec!("zunion", "ZUNION", "zset", ZSet, HitMiss, Zinter),
    spec!("zdiff", "ZDIFF", "zset", ZSet, HitMiss, Zinter),
    // Key / expire
    spec!("expire", "EXPIRE", "key", String, Ttl, Expire),
    spec!("expireat", "EXPIREAT", "key", String, Ttl, ExpireAt),
    spec!("pexpire", "PEXPIRE", "key", String, Ttl, Pexpire),
    spec!("pexpireat", "PEXPIREAT", "key", String, Ttl, PexpireAt),
    spec!("ttl", "TTL", "key", String, Ttl, Ttl),
    spec!("pttl", "PTTL", "key", String, Ttl, Pttl),
    spec!("persist", "PERSIST", "key", String, Ttl, Persist),
    spec!("scan", "SCAN", "key", None, NoKey, Scan),
    spec!("randomkey", "RANDOMKEY", "key", None, NoKey, Randomkey),
    spec!("rename", "RENAME", "key", String, Churn, TwoKey),
    spec!("renamenx", "RENAMENX", "key", String, Churn, TwoKey),
    spec!("type", "TYPE", "key", String, HitMiss, TypeKey),
    spec!("copy", "COPY", "key", String, Populate, TwoKey),
    spec!("expiretime", "EXPIRETIME", "key", String, Ttl, Ttl),
    spec!("pexpiretime", "PEXPIRETIME", "key", String, Ttl, Pttl),
    spec!("dump", "DUMP", "key", String, HitMiss, Dump),
    spec!("object", "OBJECT", "key", String, HitMiss, ObjectEncoding),
    // Server / 连接 (只发只读或无副作用形态)
    spec!("dbsize", "DBSIZE", "server", None, NoKey, Dbsize),
    spec!("info", "INFO", "server", None, NoKey, Info),
    spec!("time", "TIME", "server", None, NoKey, Time),
    spec!("ping", "PING", "server", None, NoKey, Ping),
    spec!("echo", "ECHO", "server", None, NoKey, Echo),
    spec!("command", "COMMAND", "server", None, NoKey, Command),
    spec!("lastsave", "LASTSAVE", "server", None, NoKey, Lastsave),
    spec!("client", "CLIENT", "server", None, NoKey, ClientId),
    spec!("config", "CONFIG", "server", None, NoKey, ConfigGet),
    spec!("cluster", "CLUSTER", "server", None, NoKey, ClusterInfo),
    spec!("readonly", "READONLY", "server", None, NoKey, Readonly),
    spec!("readwrite", "READWRITE", "server", None, NoKey, Readwrite),
    spec!("asking", "ASKING", "server", None, NoKey, Asking),
    spec!("eval", "EVAL", "script", None, NoKey, Eval),
    spec!("evalsha", "EVAL", "script", None, NoKey, Eval),
    spec!("script", "SCRIPT", "script", None, NoKey, ScriptExists),
];

/// 会清空实例、切断连接或卡住 pipeline, 拒绝进入 mix.
pub const FORBIDDEN_MIX_KEYS: &[&str] = &[
    "flushall",
    "flushdb",
    "shutdown",
    "save",
    "bgsave",
    "monitor",
    "quit",
    "hello",
    "select",
    "swapdb",
    "move",
    "migrate",
    "restore",
    "multi",
    "exec",
    "discard",
    "watch",
    "unwatch",
    "atom_multi",
    "atom_exec",
    "atom_discard",
    "atom_watch",
    "atom_unwatch",
    "latency",
    "slowlog",
];

fn index() -> &'static HashMap<&'static str, &'static CommandSpec> {
    static INDEX: OnceLock<HashMap<&'static str, &'static CommandSpec>> = OnceLock::new();
    INDEX.get_or_init(|| CATALOG.iter().map(|s| (s.mix_key, s)).collect())
}

pub fn lookup(mix_key: &str) -> Option<&'static CommandSpec> {
    index().get(mix_key).copied()
}

pub fn is_forbidden(mix_key: &str) -> bool {
    FORBIDDEN_MIX_KEYS.iter().any(|k| *k == mix_key)
}

pub fn mix_key_from_redis(name: &str) -> String {
    name.to_ascii_lowercase().replace('.', "_")
}

/// 选进 mix 仍允许, 但控制台会弹窗提醒: 全量扫描 / 整结构返回 / 服务端转储.
pub fn heavy_hint(spec: &CommandSpec) -> Option<&'static str> {
    Some(match spec.mix_key {
        "scan" => "SCAN 会遍历匹配键，占比高时会拖住目标。",
        "randomkey" => "RANDOMKEY 在全库抽样，占比高时会拖住目标。",
        "smembers" => "SMEMBERS 返回整个集合。",
        "hgetall" => "HGETALL 返回整个 hash。",
        "hkeys" => "HKEYS 返回 hash 的全部字段名。",
        "hvals" => "HVALS 返回 hash 的全部字段值。",
        "lrange" => "当前发压形态是 LRANGE 0 -1，会取出整个 list。",
        "zrange" => "当前发压形态是 ZRANGE 0 -1，会取出整个 zset。",
        "zrevrange" => "当前发压形态是 ZREVRANGE 0 -1，会取出整个 zset。",
        "zrangebyscore" => "当前发压形态是按 -inf/+inf 取出整个 zset。",
        "zrevrangebyscore" => "当前发压形态是按 -inf/+inf 取出整个 zset。",
        "sunion" | "sinter" | "sdiff" => "多集合运算，随成员数变重。",
        "zinter" | "zunion" | "zdiff" => "多集合运算，随成员数变重。",
        "info" => "INFO 会收集服务端统计，比普通读写重。",
        "command" => "COMMAND 返回完整命令表，响应很大。",
        "dump" => "DUMP 会序列化整个 key。",
        _ => return None,
    })
}

pub const JSON_DOC: &str = r#"{"v":1,"a":[1],"o":{"k":1},"s":"x"}"#;
pub const FIELD: &str = "f0";
pub const NUM_FIELD: &str = "n0";
pub const MEMBER: &str = "m0";
pub const EVAL_SCRIPT: &str = "return 1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heavy_hint_covers_scan_and_dump_commands() {
        for key in ["info", "smembers", "hgetall", "lrange", "scan"] {
            let spec = lookup(key).unwrap_or_else(|| panic!("missing {key}"));
            assert!(heavy_hint(spec).is_some(), "{key} should warn");
        }
        assert!(heavy_hint(lookup("get").unwrap()).is_none());
        assert!(heavy_hint(lookup("set").unwrap()).is_none());
    }
}
