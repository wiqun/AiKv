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
    cluster_commands: Option<crate::cluster::ClusterCommands>,
}

impl CommandExecutor {
    pub fn new(storage: StorageEngine) -> Self {
        Self::with_port(storage, 6379)
    }

    pub fn with_port(storage: StorageEngine, port: u16) -> Self {
        // Check if cluster feature is enabled at compile time
        #[cfg(feature = "cluster")]
        let cluster_enabled = true;
        #[cfg(not(feature = "cluster"))]
        let cluster_enabled = false;

        Self {
            string_commands: StringCommands::new(storage.clone()),
            json_commands: JsonCommands::new(storage.clone()),
            database_commands: DatabaseCommands::new(storage.clone()),
            key_commands: KeyCommands::new(storage.clone()),
            server_commands: ServerCommands::with_port_and_cluster(port, cluster_enabled),
            script_commands: ScriptCommands::new(storage.clone()),
            list_commands: ListCommands::new(storage.clone()),
            hash_commands: HashCommands::new(storage.clone()),
            set_commands: SetCommands::new(storage.clone()),
            zset_commands: ZSetCommands::new(storage),
            #[cfg(feature = "cluster")]
            cluster_commands: None, // Will be set later when cluster is initialized
        }
    }

    /// Set cluster commands after initialization.
    ///
    /// This allows setting the cluster commands after the CommandExecutor is created,
    /// once the cluster node components are fully initialized.
    #[cfg(feature = "cluster")]
    pub fn set_cluster_commands(&mut self, cluster_commands: crate::cluster::ClusterCommands) {
        self.cluster_commands = Some(cluster_commands);
    }

    /// Check if the key belongs to this node in cluster mode.
    ///
    /// Returns `Ok(())` if:
    /// - Cluster mode is disabled
    /// - Cluster is not initialized
    /// - The key belongs to this node
    ///
    /// Returns `Err(AikvError::Moved(slot, addr))` if the key belongs to another node.
    #[cfg(feature = "cluster")]
    fn check_key_routing(&self, key: &[u8]) -> Result<()> {
        if let Some(ref cluster_commands) = self.cluster_commands {
            cluster_commands.check_key_slot(key)
        } else {
            // Cluster not initialized, allow all operations locally
            Ok(())
        }
    }

    /// Check if multiple keys belong to this node in cluster mode.
    ///
    /// For multi-key commands (like MGET, MSET), all keys must be in the same slot.
    #[cfg(feature = "cluster")]
    fn check_keys_routing(&self, keys: &[&[u8]]) -> Result<()> {
        if let Some(ref cluster_commands) = self.cluster_commands {
            cluster_commands.check_keys_slot(keys)
        } else {
            Ok(())
        }
    }

    /// Placeholder for non-cluster builds
    #[cfg(not(feature = "cluster"))]
    fn check_key_routing(&self, _key: &[u8]) -> Result<()> {
        Ok(())
    }

    /// Placeholder for non-cluster builds
    #[cfg(not(feature = "cluster"))]
    fn check_keys_routing(&self, _keys: &[&[u8]]) -> Result<()> {
        Ok(())
    }

    pub fn execute(
        &self,
        command: &str,
        args: &[Bytes],
        current_db: &mut usize,
        client_id: usize,
    ) -> Result<RespValue> {
        match command.to_uppercase().as_str() {
            // String commands - single key operations
            "GET" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.get(args, *current_db)
            }
            "SET" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.set(args, *current_db)
            }
            "DEL" => {
                // DEL can take multiple keys, check all of them
                if !args.is_empty() {
                    let keys: Vec<&[u8]> = args.iter().map(|b| b.as_ref()).collect();
                    self.check_keys_routing(&keys)?;
                }
                self.string_commands.del(args, *current_db)
            }
            "EXISTS" => {
                // EXISTS can take multiple keys
                if !args.is_empty() {
                    let keys: Vec<&[u8]> = args.iter().map(|b| b.as_ref()).collect();
                    self.check_keys_routing(&keys)?;
                }
                self.string_commands.exists(args, *current_db)
            }
            "MGET" => {
                // MGET takes multiple keys, all must be in the same slot
                if !args.is_empty() {
                    let keys: Vec<&[u8]> = args.iter().map(|b| b.as_ref()).collect();
                    self.check_keys_routing(&keys)?;
                }
                self.string_commands.mget(args, *current_db)
            }
            "MSET" => {
                // MSET takes key-value pairs, check all keys (every other arg starting at 0)
                if args.len() >= 2 {
                    let keys: Vec<&[u8]> = args.iter().step_by(2).map(|b| b.as_ref()).collect();
                    self.check_keys_routing(&keys)?;
                }
                self.string_commands.mset(args, *current_db)
            }
            "STRLEN" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.strlen(args, *current_db)
            }
            "APPEND" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.append(args, *current_db)
            }
            "INCR" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.incr(args, *current_db)
            }
            "DECR" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.decr(args, *current_db)
            }
            "INCRBY" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.incrby(args, *current_db)
            }
            "DECRBY" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.decrby(args, *current_db)
            }
            "INCRBYFLOAT" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.incrbyfloat(args, *current_db)
            }
            "GETRANGE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.getrange(args, *current_db)
            }
            "SETRANGE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.setrange(args, *current_db)
            }
            "GETEX" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.getex(args, *current_db)
            }
            "GETDEL" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.getdel(args, *current_db)
            }
            "SETNX" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.setnx(args, *current_db)
            }
            "SETEX" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.setex(args, *current_db)
            }
            "PSETEX" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.string_commands.psetex(args, *current_db)
            }

            // JSON commands - single key operations
            "JSON.GET" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.json_commands.json_get(args, *current_db)
            }
            "JSON.SET" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.json_commands.json_set(args, *current_db)
            }
            "JSON.DEL" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.json_commands.json_del(args, *current_db)
            }
            "JSON.TYPE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.json_commands.json_type(args, *current_db)
            }
            "JSON.STRLEN" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.json_commands.json_strlen(args, *current_db)
            }
            "JSON.ARRLEN" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.json_commands.json_arrlen(args, *current_db)
            }
            "JSON.OBJLEN" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.json_commands.json_objlen(args, *current_db)
            }

            // Database commands - these are node-local, no routing needed
            "SELECT" => self.database_commands.select(args, current_db),
            "DBSIZE" => self.database_commands.dbsize(args, *current_db),
            "FLUSHDB" => self.database_commands.flushdb(args, *current_db),
            "FLUSHALL" => self.database_commands.flushall(args),
            "SWAPDB" => self.database_commands.swapdb(args),
            "MOVE" => self.database_commands.move_key(args, *current_db),

            // Key commands - most need routing checks
            "KEYS" => self.key_commands.keys(args, *current_db), // Local scan, no routing
            "SCAN" => self.key_commands.scan(args, *current_db), // Local scan, no routing
            "RANDOMKEY" => self.key_commands.randomkey(args, *current_db), // Local, no routing
            "RENAME" => {
                // RENAME takes two keys, both must be in the same slot
                if args.len() >= 2 {
                    let keys: Vec<&[u8]> = vec![args[0].as_ref(), args[1].as_ref()];
                    self.check_keys_routing(&keys)?;
                }
                self.key_commands.rename(args, *current_db)
            }
            "RENAMENX" => {
                if args.len() >= 2 {
                    let keys: Vec<&[u8]> = vec![args[0].as_ref(), args[1].as_ref()];
                    self.check_keys_routing(&keys)?;
                }
                self.key_commands.renamenx(args, *current_db)
            }
            "TYPE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.key_commands.get_type(args, *current_db)
            }
            "COPY" => {
                // COPY takes source and destination keys
                if args.len() >= 2 {
                    let keys: Vec<&[u8]> = vec![args[0].as_ref(), args[1].as_ref()];
                    self.check_keys_routing(&keys)?;
                }
                self.key_commands.copy(args, *current_db)
            }
            "DUMP" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.key_commands.dump(args, *current_db)
            }
            "RESTORE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.key_commands.restore(args, *current_db)
            }
            "MIGRATE" => self.key_commands.migrate(args, *current_db), // MIGRATE handles routing internally

            // Key expiration commands - single key operations
            "EXPIRE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.key_commands.expire(args, *current_db)
            }
            "EXPIREAT" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.key_commands.expireat(args, *current_db)
            }
            "PEXPIRE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.key_commands.pexpire(args, *current_db)
            }
            "PEXPIREAT" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.key_commands.pexpireat(args, *current_db)
            }
            "TTL" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.key_commands.ttl(args, *current_db)
            }
            "PTTL" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.key_commands.pttl(args, *current_db)
            }
            "PERSIST" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.key_commands.persist(args, *current_db)
            }
            "EXPIRETIME" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.key_commands.expiretime(args, *current_db)
            }
            "PEXPIRETIME" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.key_commands.pexpiretime(args, *current_db)
            }

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
                    "REWRITE" => self.server_commands.config_rewrite(&args[1..]),
                    _ => Err(AikvError::InvalidCommand(format!(
                        "Unknown CONFIG subcommand: {}",
                        subcommand
                    ))),
                }
            }
            "SLOWLOG" => self.server_commands.slowlog(args),
            "TIME" => self.server_commands.time(args),
            "COMMAND" => self.server_commands.command(args),
            "SAVE" => self.server_commands.save(args),
            "BGSAVE" => self.server_commands.bgsave(args),
            "LASTSAVE" => self.server_commands.lastsave(args),
            "SHUTDOWN" => self.server_commands.shutdown(args),
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

            // List commands - single key operations
            "LPUSH" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.list_commands.lpush(args, *current_db)
            }
            "RPUSH" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.list_commands.rpush(args, *current_db)
            }
            "LPOP" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.list_commands.lpop(args, *current_db)
            }
            "RPOP" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.list_commands.rpop(args, *current_db)
            }
            "LLEN" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.list_commands.llen(args, *current_db)
            }
            "LRANGE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.list_commands.lrange(args, *current_db)
            }
            "LINDEX" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.list_commands.lindex(args, *current_db)
            }
            "LSET" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.list_commands.lset(args, *current_db)
            }
            "LREM" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.list_commands.lrem(args, *current_db)
            }
            "LTRIM" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.list_commands.ltrim(args, *current_db)
            }
            "LINSERT" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.list_commands.linsert(args, *current_db)
            }
            "LMOVE" => {
                // LMOVE takes source and destination keys
                if args.len() >= 2 {
                    let keys: Vec<&[u8]> = vec![args[0].as_ref(), args[1].as_ref()];
                    self.check_keys_routing(&keys)?;
                }
                self.list_commands.lmove(args, *current_db)
            }
            "LPOS" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.list_commands.lpos(args, *current_db)
            }

            // Hash commands - single key operations
            "HSET" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hset(args, *current_db)
            }
            "HSETNX" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hsetnx(args, *current_db)
            }
            "HGET" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hget(args, *current_db)
            }
            "HMGET" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hmget(args, *current_db)
            }
            "HMSET" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hmset(args, *current_db)
            }
            "HDEL" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hdel(args, *current_db)
            }
            "HEXISTS" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hexists(args, *current_db)
            }
            "HLEN" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hlen(args, *current_db)
            }
            "HKEYS" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hkeys(args, *current_db)
            }
            "HVALS" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hvals(args, *current_db)
            }
            "HGETALL" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hgetall(args, *current_db)
            }
            "HINCRBY" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hincrby(args, *current_db)
            }
            "HINCRBYFLOAT" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hincrbyfloat(args, *current_db)
            }
            "HSCAN" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.hash_commands.hscan(args, *current_db)
            }

            // Set commands - single key and multi-key operations
            "SADD" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.set_commands.sadd(args, *current_db)
            }
            "SREM" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.set_commands.srem(args, *current_db)
            }
            "SISMEMBER" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.set_commands.sismember(args, *current_db)
            }
            "SMEMBERS" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.set_commands.smembers(args, *current_db)
            }
            "SCARD" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.set_commands.scard(args, *current_db)
            }
            "SPOP" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.set_commands.spop(args, *current_db)
            }
            "SRANDMEMBER" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.set_commands.srandmember(args, *current_db)
            }
            "SUNION" => {
                // SUNION takes multiple keys
                if !args.is_empty() {
                    let keys: Vec<&[u8]> = args.iter().map(|b| b.as_ref()).collect();
                    self.check_keys_routing(&keys)?;
                }
                self.set_commands.sunion(args, *current_db)
            }
            "SINTER" => {
                if !args.is_empty() {
                    let keys: Vec<&[u8]> = args.iter().map(|b| b.as_ref()).collect();
                    self.check_keys_routing(&keys)?;
                }
                self.set_commands.sinter(args, *current_db)
            }
            "SDIFF" => {
                if !args.is_empty() {
                    let keys: Vec<&[u8]> = args.iter().map(|b| b.as_ref()).collect();
                    self.check_keys_routing(&keys)?;
                }
                self.set_commands.sdiff(args, *current_db)
            }
            "SUNIONSTORE" => {
                // First arg is destination, rest are source keys
                if !args.is_empty() {
                    let keys: Vec<&[u8]> = args.iter().map(|b| b.as_ref()).collect();
                    self.check_keys_routing(&keys)?;
                }
                self.set_commands.sunionstore(args, *current_db)
            }
            "SINTERSTORE" => {
                if !args.is_empty() {
                    let keys: Vec<&[u8]> = args.iter().map(|b| b.as_ref()).collect();
                    self.check_keys_routing(&keys)?;
                }
                self.set_commands.sinterstore(args, *current_db)
            }
            "SDIFFSTORE" => {
                if !args.is_empty() {
                    let keys: Vec<&[u8]> = args.iter().map(|b| b.as_ref()).collect();
                    self.check_keys_routing(&keys)?;
                }
                self.set_commands.sdiffstore(args, *current_db)
            }
            "SSCAN" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.set_commands.sscan(args, *current_db)
            }
            "SMOVE" => {
                // SMOVE takes source and destination keys
                if args.len() >= 2 {
                    let keys: Vec<&[u8]> = vec![args[0].as_ref(), args[1].as_ref()];
                    self.check_keys_routing(&keys)?;
                }
                self.set_commands.smove(args, *current_db)
            }

            // Sorted Set commands - single key operations
            "ZADD" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zadd(args, *current_db)
            }
            "ZREM" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zrem(args, *current_db)
            }
            "ZSCORE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zscore(args, *current_db)
            }
            "ZRANK" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zrank(args, *current_db)
            }
            "ZREVRANK" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zrevrank(args, *current_db)
            }
            "ZRANGE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zrange(args, *current_db)
            }
            "ZREVRANGE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zrevrange(args, *current_db)
            }
            "ZRANGEBYSCORE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zrangebyscore(args, *current_db)
            }
            "ZREVRANGEBYSCORE" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zrevrangebyscore(args, *current_db)
            }
            "ZCARD" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zcard(args, *current_db)
            }
            "ZCOUNT" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zcount(args, *current_db)
            }
            "ZINCRBY" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zincrby(args, *current_db)
            }
            "ZSCAN" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zscan(args, *current_db)
            }
            "ZPOPMIN" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zpopmin(args, *current_db)
            }
            "ZPOPMAX" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zpopmax(args, *current_db)
            }
            "ZRANGEBYLEX" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zrangebylex(args, *current_db)
            }
            "ZREVRANGEBYLEX" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zrevrangebylex(args, *current_db)
            }
            "ZLEXCOUNT" => {
                if !args.is_empty() {
                    self.check_key_routing(&args[0])?;
                }
                self.zset_commands.zlexcount(args, *current_db)
            }

            // Cluster commands (only available with cluster feature)
            #[cfg(feature = "cluster")]
            "CLUSTER" => {
                if let Some(ref cluster_commands) = self.cluster_commands {
                    cluster_commands.execute(args)
                } else {
                    // Return fallback responses for read-only cluster commands when not initialized
                    // This helps Redis UI clients detect cluster mode properly
                    Self::handle_cluster_fallback(args)
                }
            }
            #[cfg(feature = "cluster")]
            "READONLY" => {
                if let Some(ref cluster_commands) = self.cluster_commands {
                    cluster_commands.readonly()
                } else {
                    // READONLY is safe to acknowledge even without cluster
                    Ok(RespValue::simple_string("OK"))
                }
            }
            #[cfg(feature = "cluster")]
            "READWRITE" => {
                if let Some(ref cluster_commands) = self.cluster_commands {
                    cluster_commands.readwrite()
                } else {
                    // READWRITE is safe to acknowledge even without cluster
                    Ok(RespValue::simple_string("OK"))
                }
            }
            #[cfg(feature = "cluster")]
            "ASKING" => {
                if let Some(ref cluster_commands) = self.cluster_commands {
                    cluster_commands.asking()
                } else {
                    // ASKING is safe to acknowledge even without cluster
                    Ok(RespValue::simple_string("OK"))
                }
            }

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

    #[cfg(feature = "cluster")]
    pub fn cluster_commands(&self) -> Option<&crate::cluster::ClusterCommands> {
        self.cluster_commands.as_ref()
    }

    /// Handle cluster commands when cluster is not yet initialized.
    /// Returns fallback responses for read-only commands to help Redis clients
    /// detect cluster mode properly.
    #[cfg(feature = "cluster")]
    fn handle_cluster_fallback(args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("CLUSTER".to_string()));
        }

        let subcommand = String::from_utf8_lossy(&args[0]).to_uppercase();
        match subcommand.as_str() {
            "INFO" => {
                // Return minimal cluster info indicating cluster is not ready
                let info = "cluster_state:fail\r\n\
                           cluster_slots_assigned:0\r\n\
                           cluster_slots_ok:0\r\n\
                           cluster_slots_pfail:0\r\n\
                           cluster_slots_fail:0\r\n\
                           cluster_known_nodes:1\r\n\
                           cluster_size:0\r\n\
                           cluster_current_epoch:0\r\n\
                           cluster_my_epoch:0\r\n\
                           cluster_stats_messages_sent:0\r\n\
                           cluster_stats_messages_received:0";
                Ok(RespValue::BulkString(Some(Bytes::from(info))))
            }
            "NODES" => {
                // Return empty nodes list - node not yet configured
                Ok(RespValue::BulkString(Some(Bytes::from(""))))
            }
            "SLOTS" => {
                // Return empty slots array
                Ok(RespValue::Array(Some(vec![])))
            }
            "SHARDS" => {
                // Return empty shards array
                Ok(RespValue::Array(Some(vec![])))
            }
            "MYID" => {
                // Return a placeholder node ID (all zeros)
                Ok(RespValue::BulkString(Some(Bytes::from(
                    "0000000000000000000000000000000000000000",
                ))))
            }
            "MYSHARDID" => {
                // Return a placeholder shard ID
                Ok(RespValue::BulkString(Some(Bytes::from(
                    "0000000000000000000000000000000000000000",
                ))))
            }
            "KEYSLOT" => {
                if args.len() != 2 {
                    return Err(AikvError::WrongArgCount("CLUSTER KEYSLOT".to_string()));
                }
                // Calculate slot even without cluster - this is a pure function
                let slot = crate::cluster::Router::key_to_slot(&args[1]);
                Ok(RespValue::Integer(slot as i64))
            }
            "SAVECONFIG" => Ok(RespValue::simple_string("OK")),
            "BUMPEPOCH" => Ok(RespValue::BulkString(Some(Bytes::from("BUMPED 0")))),
            "SET-CONFIG-EPOCH" => Ok(RespValue::simple_string("OK")),
            "COUNT-FAILURE-REPORTS" => Ok(RespValue::Integer(0)),
            "COUNTKEYSINSLOT" => Ok(RespValue::Integer(0)),
            "GETKEYSINSLOT" => Ok(RespValue::Array(Some(vec![]))),
            _ => Err(AikvError::Internal(
                "Cluster not initialized. Please initialize cluster node first.".to_string(),
            )),
        }
    }
}
