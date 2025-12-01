pub mod database;
pub mod hash;
pub mod json;
pub mod key;
pub mod list;
pub mod script;
pub mod server;
pub mod set;
pub mod string;
pub mod zset;

use self::database::DatabaseCommands;
use self::hash::HashCommands;
use self::json::JsonCommands;
use self::key::KeyCommands;
use self::list::ListCommands;
use self::script::ScriptCommands;
use self::server::ServerCommands;
use self::set::SetCommands;
use self::string::StringCommands;
use self::zset::ZSetCommands;
use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::StorageEngine;
use bytes::Bytes;

/// Command executor with database context
pub struct CommandExecutor {
    string_commands: StringCommands,
    json_commands: JsonCommands,
    database_commands: DatabaseCommands,
    key_commands: KeyCommands,
    server_commands: ServerCommands,
    script_commands: ScriptCommands,
    list_commands: ListCommands,
    hash_commands: HashCommands,
    set_commands: SetCommands,
    zset_commands: ZSetCommands,
    #[cfg(feature = "cluster")]
    cluster_commands: crate::cluster::ClusterCommands,
}

impl CommandExecutor {
    pub fn new(storage: StorageEngine) -> Self {
        Self::with_port(storage, 6379)
    }

    pub fn with_port(storage: StorageEngine, port: u16) -> Self {
        Self {
            string_commands: StringCommands::new(storage.clone()),
            json_commands: JsonCommands::new(storage.clone()),
            database_commands: DatabaseCommands::new(storage.clone()),
            key_commands: KeyCommands::new(storage.clone()),
            server_commands: ServerCommands::with_port(port),
            script_commands: ScriptCommands::new(storage.clone()),
            list_commands: ListCommands::new(storage.clone()),
            hash_commands: HashCommands::new(storage.clone()),
            set_commands: SetCommands::new(storage.clone()),
            zset_commands: ZSetCommands::new(storage),
            #[cfg(feature = "cluster")]
            cluster_commands: crate::cluster::ClusterCommands::new(),
        }
    }

    pub fn execute(
        &self,
        command: &str,
        args: &[Bytes],
        current_db: &mut usize,
        client_id: usize,
    ) -> Result<RespValue> {
        match command.to_uppercase().as_str() {
            // String commands
            "GET" => self.string_commands.get(args, *current_db),
            "SET" => self.string_commands.set(args, *current_db),
            "DEL" => self.string_commands.del(args, *current_db),
            "EXISTS" => self.string_commands.exists(args, *current_db),
            "MGET" => self.string_commands.mget(args, *current_db),
            "MSET" => self.string_commands.mset(args, *current_db),
            "STRLEN" => self.string_commands.strlen(args, *current_db),
            "APPEND" => self.string_commands.append(args, *current_db),

            // JSON commands
            "JSON.GET" => self.json_commands.json_get(args, *current_db),
            "JSON.SET" => self.json_commands.json_set(args, *current_db),
            "JSON.DEL" => self.json_commands.json_del(args, *current_db),
            "JSON.TYPE" => self.json_commands.json_type(args, *current_db),
            "JSON.STRLEN" => self.json_commands.json_strlen(args, *current_db),
            "JSON.ARRLEN" => self.json_commands.json_arrlen(args, *current_db),
            "JSON.OBJLEN" => self.json_commands.json_objlen(args, *current_db),

            // Database commands
            "SELECT" => self.database_commands.select(args, current_db),
            "DBSIZE" => self.database_commands.dbsize(args, *current_db),
            "FLUSHDB" => self.database_commands.flushdb(args, *current_db),
            "FLUSHALL" => self.database_commands.flushall(args),
            "SWAPDB" => self.database_commands.swapdb(args),
            "MOVE" => self.database_commands.move_key(args, *current_db),

            // Key commands
            "KEYS" => self.key_commands.keys(args, *current_db),
            "SCAN" => self.key_commands.scan(args, *current_db),
            "RANDOMKEY" => self.key_commands.randomkey(args, *current_db),
            "RENAME" => self.key_commands.rename(args, *current_db),
            "RENAMENX" => self.key_commands.renamenx(args, *current_db),
            "TYPE" => self.key_commands.get_type(args, *current_db),
            "COPY" => self.key_commands.copy(args, *current_db),

            // Key expiration commands
            "EXPIRE" => self.key_commands.expire(args, *current_db),
            "EXPIREAT" => self.key_commands.expireat(args, *current_db),
            "PEXPIRE" => self.key_commands.pexpire(args, *current_db),
            "PEXPIREAT" => self.key_commands.pexpireat(args, *current_db),
            "TTL" => self.key_commands.ttl(args, *current_db),
            "PTTL" => self.key_commands.pttl(args, *current_db),
            "PERSIST" => self.key_commands.persist(args, *current_db),
            "EXPIRETIME" => self.key_commands.expiretime(args, *current_db),
            "PEXPIRETIME" => self.key_commands.pexpiretime(args, *current_db),

            // Server commands
            "INFO" => self.server_commands.info(args),
            "CONFIG" => {
                if args.is_empty() {
                    return Err(AikvError::WrongArgCount("CONFIG".to_string()));
                }
                let subcommand = String::from_utf8_lossy(&args[0]).to_uppercase();
                match subcommand.as_str() {
                    "GET" => self.server_commands.config_get(&args[1..]),
                    "SET" => self.server_commands.config_set(&args[1..]),
                    _ => Err(AikvError::InvalidCommand(format!(
                        "Unknown CONFIG subcommand: {}",
                        subcommand
                    ))),
                }
            }
            "SLOWLOG" => self.server_commands.slowlog(args),
            "TIME" => self.server_commands.time(args),
            "CLIENT" => {
                if args.is_empty() {
                    return Err(AikvError::WrongArgCount("CLIENT".to_string()));
                }
                let subcommand = String::from_utf8_lossy(&args[0]).to_uppercase();
                match subcommand.as_str() {
                    "LIST" => self.server_commands.client_list(&args[1..]),
                    "SETNAME" => self.server_commands.client_setname(&args[1..], client_id),
                    "GETNAME" => self.server_commands.client_getname(&args[1..], client_id),
                    _ => Err(AikvError::InvalidCommand(format!(
                        "Unknown CLIENT subcommand: {}",
                        subcommand
                    ))),
                }
            }

            // Script commands
            "EVAL" => self.script_commands.eval(args, *current_db),
            "EVALSHA" => self.script_commands.evalsha(args, *current_db),
            "SCRIPT" => {
                if args.is_empty() {
                    return Err(AikvError::WrongArgCount("SCRIPT".to_string()));
                }
                let subcommand = String::from_utf8_lossy(&args[0]).to_uppercase();
                match subcommand.as_str() {
                    "LOAD" => self.script_commands.script_load(&args[1..]),
                    "EXISTS" => self.script_commands.script_exists(&args[1..]),
                    "FLUSH" => self.script_commands.script_flush(&args[1..]),
                    "KILL" => self.script_commands.script_kill(&args[1..]),
                    _ => Err(AikvError::InvalidCommand(format!(
                        "Unknown SCRIPT subcommand: {}",
                        subcommand
                    ))),
                }
            }

            // List commands
            "LPUSH" => self.list_commands.lpush(args, *current_db),
            "RPUSH" => self.list_commands.rpush(args, *current_db),
            "LPOP" => self.list_commands.lpop(args, *current_db),
            "RPOP" => self.list_commands.rpop(args, *current_db),
            "LLEN" => self.list_commands.llen(args, *current_db),
            "LRANGE" => self.list_commands.lrange(args, *current_db),
            "LINDEX" => self.list_commands.lindex(args, *current_db),
            "LSET" => self.list_commands.lset(args, *current_db),
            "LREM" => self.list_commands.lrem(args, *current_db),
            "LTRIM" => self.list_commands.ltrim(args, *current_db),
            "LINSERT" => self.list_commands.linsert(args, *current_db),
            "LMOVE" => self.list_commands.lmove(args, *current_db),

            // Hash commands
            "HSET" => self.hash_commands.hset(args, *current_db),
            "HSETNX" => self.hash_commands.hsetnx(args, *current_db),
            "HGET" => self.hash_commands.hget(args, *current_db),
            "HMGET" => self.hash_commands.hmget(args, *current_db),
            "HMSET" => self.hash_commands.hmset(args, *current_db),
            "HDEL" => self.hash_commands.hdel(args, *current_db),
            "HEXISTS" => self.hash_commands.hexists(args, *current_db),
            "HLEN" => self.hash_commands.hlen(args, *current_db),
            "HKEYS" => self.hash_commands.hkeys(args, *current_db),
            "HVALS" => self.hash_commands.hvals(args, *current_db),
            "HGETALL" => self.hash_commands.hgetall(args, *current_db),
            "HINCRBY" => self.hash_commands.hincrby(args, *current_db),
            "HINCRBYFLOAT" => self.hash_commands.hincrbyfloat(args, *current_db),
            "HSCAN" => self.hash_commands.hscan(args, *current_db),

            // Set commands
            "SADD" => self.set_commands.sadd(args, *current_db),
            "SREM" => self.set_commands.srem(args, *current_db),
            "SISMEMBER" => self.set_commands.sismember(args, *current_db),
            "SMEMBERS" => self.set_commands.smembers(args, *current_db),
            "SCARD" => self.set_commands.scard(args, *current_db),
            "SPOP" => self.set_commands.spop(args, *current_db),
            "SRANDMEMBER" => self.set_commands.srandmember(args, *current_db),
            "SUNION" => self.set_commands.sunion(args, *current_db),
            "SINTER" => self.set_commands.sinter(args, *current_db),
            "SDIFF" => self.set_commands.sdiff(args, *current_db),
            "SUNIONSTORE" => self.set_commands.sunionstore(args, *current_db),
            "SINTERSTORE" => self.set_commands.sinterstore(args, *current_db),
            "SDIFFSTORE" => self.set_commands.sdiffstore(args, *current_db),

            // Sorted Set commands
            "ZADD" => self.zset_commands.zadd(args, *current_db),
            "ZREM" => self.zset_commands.zrem(args, *current_db),
            "ZSCORE" => self.zset_commands.zscore(args, *current_db),
            "ZRANK" => self.zset_commands.zrank(args, *current_db),
            "ZREVRANK" => self.zset_commands.zrevrank(args, *current_db),
            "ZRANGE" => self.zset_commands.zrange(args, *current_db),
            "ZREVRANGE" => self.zset_commands.zrevrange(args, *current_db),
            "ZRANGEBYSCORE" => self.zset_commands.zrangebyscore(args, *current_db),
            "ZREVRANGEBYSCORE" => self.zset_commands.zrevrangebyscore(args, *current_db),
            "ZCARD" => self.zset_commands.zcard(args, *current_db),
            "ZCOUNT" => self.zset_commands.zcount(args, *current_db),
            "ZINCRBY" => self.zset_commands.zincrby(args, *current_db),

            // Cluster commands (only available with cluster feature)
            #[cfg(feature = "cluster")]
            "CLUSTER" => self.cluster_commands.execute(args),
            #[cfg(feature = "cluster")]
            "READONLY" => self.cluster_commands.readonly(),
            #[cfg(feature = "cluster")]
            "READWRITE" => self.cluster_commands.readwrite(),

            // Utility commands
            "PING" => {
                if args.is_empty() {
                    Ok(RespValue::simple_string("PONG"))
                } else if args.len() == 1 {
                    // Return a copy of the argument as a bulk string
                    Ok(RespValue::bulk_string(args[0].clone()))
                } else {
                    Err(AikvError::WrongArgCount("PING".to_string()))
                }
            }
            "ECHO" => {
                if args.len() != 1 {
                    return Err(AikvError::WrongArgCount("ECHO".to_string()));
                }
                Ok(RespValue::bulk_string(args[0].clone()))
            }

            _ => Err(AikvError::InvalidCommand(format!(
                "Unknown command: {}",
                command
            ))),
        }
    }

    pub fn server_commands(&self) -> &ServerCommands {
        &self.server_commands
    }
}
