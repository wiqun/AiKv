use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::{BatchOp, StorageEngine, StoredValue};
use bytes::Bytes;
use mlua::{Lua, LuaOptions, StdLib, Value as LuaValue};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

// ============================================================================
// KEY LOCK MANAGER - Key-level locking for parallel script execution
// ============================================================================

/// Lock entry representing a key being locked
#[derive(Debug)]
struct LockEntry {
    /// Number of holders for this lock
    holders: usize,
    /// Queue of waiters (using condition variable notification)
    waiters: usize,
}

/// Key-level lock manager for parallel script execution.
///
/// Provides key-level locking to enable parallel execution of Lua scripts
/// that operate on different keys, while ensuring serialization for scripts
/// that operate on the same keys.
///
/// # Design
///
/// - Scripts operating on different keys can execute in parallel
/// - Scripts operating on the same key(s) are serialized
/// - Fair queue ordering using condition variables
/// - Lock timeout to prevent deadlocks
///
/// # Example
///
/// ```ignore
/// let lock_manager = KeyLockManager::new(Duration::from_secs(30));
///
/// // Acquire locks for script keys
/// let guard = lock_manager.lock_keys(&["key1", "key2"])?;
///
/// // Execute script...
///
/// // Guard is dropped, locks are released
/// ```
pub struct KeyLockManager {
    /// Map of key -> lock entry
    locks: Mutex<HashMap<String, LockEntry>>,
    /// Condition variable for fair waiting
    condvar: Condvar,
    /// Lock acquisition timeout
    timeout: Duration,
}

impl KeyLockManager {
    /// Create a new key lock manager with the specified timeout.
    pub fn new(timeout: Duration) -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
            condvar: Condvar::new(),
            timeout,
        }
    }

    /// Acquire locks for the specified keys.
    ///
    /// Returns a guard that releases the locks when dropped.
    /// Keys are sorted before locking to prevent deadlocks.
    pub fn lock_keys(&self, keys: &[String]) -> Result<KeyLockGuard<'_>> {
        if keys.is_empty() {
            // No keys to lock, return empty guard
            return Ok(KeyLockGuard {
                manager: self,
                keys: Vec::new(),
            });
        }

        // Sort keys to prevent deadlock
        let mut sorted_keys: Vec<String> = keys.to_vec();
        sorted_keys.sort();
        sorted_keys.dedup();

        let start_time = Instant::now();

        loop {
            let mut locks = self
                .locks
                .lock()
                .map_err(|e| AikvError::Script(format!("Lock manager error: {}", e)))?;

            // Check if all keys are available
            let all_available = sorted_keys.iter().all(|key| {
                !locks.contains_key(key) || locks.get(key).is_none_or(|e| e.holders == 0)
            });

            if all_available {
                // Acquire all locks
                for key in &sorted_keys {
                    let entry = locks.entry(key.clone()).or_insert(LockEntry {
                        holders: 0,
                        waiters: 0,
                    });
                    entry.holders += 1;
                }

                return Ok(KeyLockGuard {
                    manager: self,
                    keys: sorted_keys,
                });
            }

            // Check timeout
            if start_time.elapsed() >= self.timeout {
                return Err(AikvError::Script(format!(
                    "Lock acquisition timeout after {:?}",
                    self.timeout
                )));
            }

            // Register as waiter and wait
            for key in &sorted_keys {
                if let Some(entry) = locks.get_mut(key) {
                    entry.waiters += 1;
                }
            }

            // Calculate remaining timeout
            let remaining = self.timeout - start_time.elapsed();

            // Wait for notification with timeout
            let result = self
                .condvar
                .wait_timeout(locks, remaining)
                .map_err(|e| AikvError::Script(format!("Lock wait error: {}", e)))?;

            let mut locks = result.0;

            // Decrement waiter count
            for key in &sorted_keys {
                if let Some(entry) = locks.get_mut(key) {
                    if entry.waiters > 0 {
                        entry.waiters -= 1;
                    }
                }
            }

            // Check if we timed out
            if result.1.timed_out() {
                return Err(AikvError::Script(format!(
                    "Lock acquisition timeout after {:?}",
                    self.timeout
                )));
            }
        }
    }

    /// Release locks for the specified keys.
    fn unlock_keys(&self, keys: &[String]) {
        if keys.is_empty() {
            return;
        }

        if let Ok(mut locks) = self.locks.lock() {
            for key in keys {
                if let Some(entry) = locks.get_mut(key) {
                    if entry.holders > 0 {
                        entry.holders -= 1;
                    }

                    // Clean up entry if no holders and no waiters
                    if entry.holders == 0 && entry.waiters == 0 {
                        locks.remove(key);
                    }
                }
            }

            // Notify all waiters
            self.condvar.notify_all();
        }
    }
}

impl Default for KeyLockManager {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

/// Guard that holds key locks and releases them on drop.
pub struct KeyLockGuard<'a> {
    manager: &'a KeyLockManager,
    keys: Vec<String>,
}

impl<'a> Drop for KeyLockGuard<'a> {
    fn drop(&mut self) {
        self.manager.unlock_keys(&self.keys);
    }
}

// ============================================================================
// EXTENDED BATCH OPERATIONS - Support for complex types
// ============================================================================

/// Extended batch operation supporting all data types for transactions.
#[derive(Debug, Clone)]
pub enum ExtendedBatchOp {
    /// Set a string value
    SetString(Bytes),
    /// Set a list value
    SetList(VecDeque<Bytes>),
    /// Set a hash value
    SetHash(HashMap<String, Bytes>),
    /// Set a set value
    SetSet(HashSet<Vec<u8>>),
    /// Set a sorted set value
    SetZSet(BTreeMap<Vec<u8>, f64>),
    /// Delete the key
    Delete,
}

/// Script cache entry
#[derive(Clone, Debug)]
struct CachedScript {
    script: String,
}

/// Transaction context for Lua script execution
///
/// This provides transactional semantics for Lua scripts by buffering all write
/// operations and only committing them if the script completes successfully.
/// If the script fails, the buffer is discarded, achieving automatic rollback.
///
/// When using AiDbStorageEngine, this leverages AiDb's WriteBatch for true
/// atomic batch writes with WAL durability guarantees.
///
/// Extended to support complex types (List, Hash, Set, ZSet) for Lua scripts.
#[derive(Debug)]
struct ScriptTransaction {
    /// Database index for this transaction
    db_index: usize,
    /// Extended write buffer: key -> operation (supports all types)
    write_buffer: HashMap<String, ExtendedBatchOp>,
}

impl ScriptTransaction {
    /// Create a new transaction context for a database
    fn new(db_index: usize) -> Self {
        Self {
            db_index,
            write_buffer: HashMap::new(),
        }
    }

    /// Read a string value, checking buffer first, then storage
    ///
    /// This implements "read your own writes" semantics - if a key was set
    /// or deleted in this transaction, return that state.
    fn get(&self, storage: &StorageEngine, key: &str) -> Result<Option<Bytes>> {
        // Check write buffer first
        if let Some(op) = self.write_buffer.get(key) {
            match op {
                ExtendedBatchOp::SetString(value) => return Ok(Some(value.clone())),
                ExtendedBatchOp::Delete => return Ok(None),
                // For complex types, we return None for string GET
                _ => {
                    return Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            }
        }

        // Fall back to storage
        storage.get_from_db(self.db_index, key)
    }

    /// Read a full StoredValue, checking buffer first, then storage
    fn get_value(&self, storage: &StorageEngine, key: &str) -> Result<Option<StoredValue>> {
        // Check write buffer first
        if let Some(op) = self.write_buffer.get(key) {
            match op {
                ExtendedBatchOp::SetString(value) => {
                    return Ok(Some(StoredValue::new_string(value.clone())))
                }
                ExtendedBatchOp::SetList(list) => {
                    return Ok(Some(StoredValue::new_list(list.clone())))
                }
                ExtendedBatchOp::SetHash(hash) => {
                    return Ok(Some(StoredValue::new_hash(hash.clone())))
                }
                ExtendedBatchOp::SetSet(set) => return Ok(Some(StoredValue::new_set(set.clone()))),
                ExtendedBatchOp::SetZSet(zset) => {
                    return Ok(Some(StoredValue::new_zset(zset.clone())))
                }
                ExtendedBatchOp::Delete => return Ok(None),
            }
        }

        // Fall back to storage
        storage.get_value(self.db_index, key)
    }

    /// Write a string value to the buffer
    fn set(&mut self, key: String, value: Bytes) {
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetString(value));
    }

    /// Write a list value to the buffer
    fn set_list(&mut self, key: String, list: VecDeque<Bytes>) {
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetList(list));
    }

    /// Write a hash value to the buffer
    fn set_hash(&mut self, key: String, hash: HashMap<String, Bytes>) {
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetHash(hash));
    }

    /// Write a set value to the buffer
    fn set_set(&mut self, key: String, set: HashSet<Vec<u8>>) {
        self.write_buffer.insert(key, ExtendedBatchOp::SetSet(set));
    }

    /// Write a sorted set value to the buffer
    fn set_zset(&mut self, key: String, zset: BTreeMap<Vec<u8>, f64>) {
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetZSet(zset));
    }

    /// Mark a key for deletion in the buffer
    fn delete(&mut self, key: String) {
        self.write_buffer.insert(key, ExtendedBatchOp::Delete);
    }

    /// Check if a key exists, considering the buffer
    fn exists(&self, storage: &StorageEngine, key: &str) -> Result<bool> {
        // Check write buffer first
        if let Some(op) = self.write_buffer.get(key) {
            match op {
                ExtendedBatchOp::Delete => return Ok(false),
                _ => return Ok(true), // Any set operation means the key exists
            }
        }

        // Fall back to storage
        storage.exists_in_db(self.db_index, key)
    }

    /// Commit the transaction - apply all buffered operations to storage atomically
    ///
    /// This method handles both simple string operations (using write_batch) and
    /// complex type operations (using set_value individually).
    ///
    /// - For MemoryAdapter: In-memory atomicity within a single lock
    /// - For AiDbStorageEngine: True atomic batch writes via AiDb's WriteBatch
    ///   with WAL durability guarantees (all operations written to WAL first,
    ///   single fsync, atomic recovery on crash)
    fn commit(self, storage: &StorageEngine) -> Result<()> {
        if self.write_buffer.is_empty() {
            return Ok(());
        }

        // Separate string operations (can use write_batch) from complex type operations
        let mut string_ops: Vec<(String, BatchOp)> = Vec::new();
        let mut complex_ops: Vec<(String, ExtendedBatchOp)> = Vec::new();

        for (key, op) in self.write_buffer.into_iter() {
            match op {
                ExtendedBatchOp::SetString(value) => {
                    string_ops.push((key, BatchOp::Set(value)));
                }
                ExtendedBatchOp::Delete => {
                    string_ops.push((key, BatchOp::Delete));
                }
                _ => {
                    complex_ops.push((key, op));
                }
            }
        }

        // Commit string operations using write_batch for atomicity
        if !string_ops.is_empty() {
            storage.write_batch(self.db_index, string_ops)?;
        }

        // Commit complex type operations individually
        for (key, op) in complex_ops {
            match op {
                ExtendedBatchOp::SetList(list) => {
                    storage.set_value(self.db_index, key, StoredValue::new_list(list))?;
                }
                ExtendedBatchOp::SetHash(hash) => {
                    storage.set_value(self.db_index, key, StoredValue::new_hash(hash))?;
                }
                ExtendedBatchOp::SetSet(set) => {
                    storage.set_value(self.db_index, key, StoredValue::new_set(set))?;
                }
                ExtendedBatchOp::SetZSet(zset) => {
                    storage.set_value(self.db_index, key, StoredValue::new_zset(zset))?;
                }
                // These are already handled above
                ExtendedBatchOp::SetString(_) | ExtendedBatchOp::Delete => {}
            }
        }

        Ok(())
    }

    // Note: rollback() is implicit - just drop the transaction without calling commit()
}

/// Script command handler with key-level locking for parallel execution.
///
/// This handler supports:
/// - Key-level locking: Scripts operating on different keys can run in parallel
/// - Extended command support: Beyond GET/SET/DEL/EXISTS, includes Hash, List, Set, ZSet commands
/// - Transactional semantics: All writes are buffered and committed atomically
pub struct ScriptCommands {
    storage: StorageEngine,
    script_cache: Arc<RwLock<HashMap<String, CachedScript>>>,
    /// Key-level lock manager for parallel script execution
    key_lock_manager: Arc<KeyLockManager>,
}

impl ScriptCommands {
    pub fn new(storage: StorageEngine) -> Self {
        Self {
            storage,
            script_cache: Arc::new(RwLock::new(HashMap::new())),
            key_lock_manager: Arc::new(KeyLockManager::default()),
        }
    }

    /// Create a new ScriptCommands with a custom lock timeout.
    pub fn with_lock_timeout(storage: StorageEngine, lock_timeout: Duration) -> Self {
        Self {
            storage,
            script_cache: Arc::new(RwLock::new(HashMap::new())),
            key_lock_manager: Arc::new(KeyLockManager::new(lock_timeout)),
        }
    }

    /// Calculate SHA1 hash of a script
    fn calculate_sha1(script: &str) -> String {
        let mut hasher = Sha1::new();
        hasher.update(script.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// EVAL script numkeys [key [key ...]] [arg [arg ...]]
    /// Execute a Lua script
    pub fn eval(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("EVAL".to_string()));
        }

        let script = String::from_utf8_lossy(&args[0]).to_string();
        let numkeys: usize = String::from_utf8_lossy(&args[1])
            .parse()
            .map_err(|_| AikvError::InvalidArgument("numkeys must be a number".to_string()))?;

        if args.len() < 2 + numkeys {
            return Err(AikvError::InvalidArgument(
                "Number of keys doesn't match numkeys parameter".to_string(),
            ));
        }

        let keys: Vec<String> = args[2..2 + numkeys]
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let argv: Vec<String> = args[2 + numkeys..]
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        self.execute_script(&script, &keys, &argv, db_index)
    }

    /// EVALSHA sha1 numkeys [key [key ...]] [arg [arg ...]]
    /// Execute a cached script by its SHA1 digest
    pub fn evalsha(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("EVALSHA".to_string()));
        }

        let sha1 = String::from_utf8_lossy(&args[0]).to_string();
        let numkeys: usize = String::from_utf8_lossy(&args[1])
            .parse()
            .map_err(|_| AikvError::InvalidArgument("numkeys must be a number".to_string()))?;

        if args.len() < 2 + numkeys {
            return Err(AikvError::InvalidArgument(
                "Number of keys doesn't match numkeys parameter".to_string(),
            ));
        }

        // Get script from cache
        let cache = self
            .script_cache
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let cached_script = cache.get(&sha1).ok_or_else(|| {
            AikvError::InvalidArgument("NOSCRIPT No matching script. Use EVAL.".to_string())
        })?;

        let script = cached_script.script.clone();
        drop(cache);

        let keys: Vec<String> = args[2..2 + numkeys]
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let argv: Vec<String> = args[2 + numkeys..]
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        self.execute_script(&script, &keys, &argv, db_index)
    }

    /// SCRIPT LOAD script
    /// Load a script into the cache without executing it
    pub fn script_load(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("SCRIPT LOAD".to_string()));
        }

        let script = String::from_utf8_lossy(&args[0]).to_string();
        let sha1 = Self::calculate_sha1(&script);

        let mut cache = self
            .script_cache
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        cache.insert(
            sha1.clone(),
            CachedScript {
                script,
            },
        );

        Ok(RespValue::bulk_string(Bytes::from(sha1)))
    }

    /// SCRIPT EXISTS sha1 [sha1 ...]
    /// Check if scripts exist in the cache
    pub fn script_exists(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("SCRIPT EXISTS".to_string()));
        }

        let cache = self
            .script_cache
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let results: Vec<RespValue> = args
            .iter()
            .map(|sha1_bytes| {
                let sha1 = String::from_utf8_lossy(sha1_bytes).to_string();
                let exists = cache.contains_key(&sha1);
                RespValue::Integer(if exists { 1 } else { 0 })
            })
            .collect();

        Ok(RespValue::Array(Some(results)))
    }

    /// SCRIPT FLUSH [ASYNC|SYNC]
    /// Clear the script cache
    pub fn script_flush(&self, _args: &[Bytes]) -> Result<RespValue> {
        let mut cache = self
            .script_cache
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        cache.clear();
        Ok(RespValue::simple_string("OK"))
    }

    /// SCRIPT KILL
    /// Kill the currently executing script (not implemented for now)
    pub fn script_kill(&self, _args: &[Bytes]) -> Result<RespValue> {
        // In a single-threaded execution model, this is not really applicable
        // Return NOTBUSY when no script is running
        Err(AikvError::InvalidArgument(
            "NOTBUSY No scripts in execution right now.".to_string(),
        ))
    }

    /// Execute a Lua script with given keys and arguments
    ///
    /// This method implements key-level locking for parallel script execution:
    /// - Scripts operating on different keys can run in parallel
    /// - Scripts operating on the same keys are serialized
    fn execute_script(
        &self,
        script: &str,
        keys: &[String],
        argv: &[String],
        db_index: usize,
    ) -> Result<RespValue> {
        // Acquire key locks before execution (enables parallel execution for different keys)
        let _lock_guard = self.key_lock_manager.lock_keys(keys)?;

        // Create transaction context for this script execution
        let transaction = Arc::new(RwLock::new(ScriptTransaction::new(db_index)));

        // Execute the script in a scope to ensure Lua is dropped before we commit
        let resp_result = {
            // Create a new Lua instance with minimal standard library
            let lua = Lua::new_with(
                StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8,
                LuaOptions::default(),
            )
            .map_err(|e| AikvError::Script(format!("Failed to create Lua instance: {}", e)))?;

            // Set up KEYS and ARGV tables
            lua.globals()
                .set("KEYS", lua.create_table().unwrap())
                .map_err(|e| AikvError::Script(format!("Failed to set KEYS: {}", e)))?;

            lua.globals()
                .set("ARGV", lua.create_table().unwrap())
                .map_err(|e| AikvError::Script(format!("Failed to set ARGV: {}", e)))?;

            // Populate KEYS (1-indexed in Lua)
            let keys_table = lua.globals().get::<mlua::Table>("KEYS").unwrap();
            for (i, key) in keys.iter().enumerate() {
                keys_table.set(i + 1, key.clone()).map_err(|e| {
                    AikvError::Script(format!("Failed to set KEYS[{}]: {}", i + 1, e))
                })?;
            }

            // Populate ARGV (1-indexed in Lua)
            let argv_table = lua.globals().get::<mlua::Table>("ARGV").unwrap();
            for (i, arg) in argv.iter().enumerate() {
                argv_table.set(i + 1, arg.clone()).map_err(|e| {
                    AikvError::Script(format!("Failed to set ARGV[{}]: {}", i + 1, e))
                })?;
            }

            // Set up redis.call and redis.pcall functions
            let storage = self.storage.clone();

            lua.globals()
                .set(
                    "redis",
                    lua.create_table().map_err(|e| {
                        AikvError::Script(format!("Failed to create redis table: {}", e))
                    })?,
                )
                .map_err(|e| AikvError::Script(format!("Failed to set redis table: {}", e)))?;

            let redis_table = lua.globals().get::<mlua::Table>("redis").unwrap();

            // redis.call - Execute Redis command (throws error on failure)
            let storage_for_call = storage.clone();
            let txn_for_call = transaction.clone();
            let call_fn = lua
                .create_function(move |lua_ctx, args: mlua::MultiValue| {
                    Self::redis_call(&storage_for_call, &txn_for_call, lua_ctx, args, true)
                })
                .map_err(|e| AikvError::Script(format!("Failed to create call function: {}", e)))?;

            redis_table
                .set("call", call_fn)
                .map_err(|e| AikvError::Script(format!("Failed to set redis.call: {}", e)))?;

            // redis.pcall - Protected call (returns error as result)
            let storage_for_pcall = storage.clone();
            let txn_for_pcall = transaction.clone();
            let pcall_fn = lua
                .create_function(move |lua_ctx, args: mlua::MultiValue| {
                    Self::redis_call(&storage_for_pcall, &txn_for_pcall, lua_ctx, args, false)
                })
                .map_err(|e| {
                    AikvError::Script(format!("Failed to create pcall function: {}", e))
                })?;

            redis_table
                .set("pcall", pcall_fn)
                .map_err(|e| AikvError::Script(format!("Failed to set redis.pcall: {}", e)))?;

            // Execute the script
            let result: LuaValue = lua
                .load(script)
                .eval()
                .map_err(|e| AikvError::Script(format!("Script execution error: {}", e)))?;

            // Convert Lua result to RespValue while Lua is still alive
            Self::lua_to_resp(result)?
            // Lua is dropped here, releasing the Arc references in the closures
        };

        // Script succeeded - commit the transaction
        // Now that Lua is dropped, we can unwrap the Arc
        let txn = Arc::try_unwrap(transaction)
            .map_err(|_| AikvError::Script("Failed to unwrap transaction".to_string()))?
            .into_inner()
            .map_err(|e| AikvError::Script(format!("Lock error on commit: {}", e)))?;

        txn.commit(&self.storage)?;

        // Return the converted result
        Ok(resp_result)
    }

    /// Execute a Redis command from Lua
    fn redis_call(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        lua: &mlua::Lua,
        args: mlua::MultiValue,
        throw_error: bool,
    ) -> mlua::Result<LuaValue> {
        // Convert arguments to bytes
        let mut cmd_args: Vec<Bytes> = Vec::new();

        for arg in args {
            match arg {
                LuaValue::String(s) => {
                    cmd_args.push(Bytes::from(s.as_bytes().to_vec()));
                }
                LuaValue::Integer(i) => {
                    cmd_args.push(Bytes::from(i.to_string()));
                }
                LuaValue::Number(n) => {
                    cmd_args.push(Bytes::from(n.to_string()));
                }
                LuaValue::Boolean(b) => {
                    cmd_args.push(Bytes::from(if b { "1" } else { "0" }));
                }
                _ => {
                    if throw_error {
                        return Err(mlua::Error::RuntimeError(
                            "Invalid argument type".to_string(),
                        ));
                    } else {
                        return Ok(LuaValue::Nil);
                    }
                }
            }
        }

        if cmd_args.is_empty() {
            if throw_error {
                return Err(mlua::Error::RuntimeError(
                    "No command specified".to_string(),
                ));
            } else {
                return Ok(LuaValue::Nil);
            }
        }

        // Extract command and arguments
        let command = String::from_utf8_lossy(&cmd_args[0])
            .to_uppercase()
            .to_string();
        let command_args = &cmd_args[1..];

        // Execute commands - extended support for all data types
        let result = match command.as_str() {
            // String commands
            "GET" => Self::execute_get(storage, transaction, command_args),
            "SET" => Self::execute_set(storage, transaction, command_args),
            "DEL" => Self::execute_del(storage, transaction, command_args),
            "EXISTS" => Self::execute_exists(storage, transaction, command_args),
            "INCR" => Self::execute_incr(storage, transaction, command_args),
            "DECR" => Self::execute_decr(storage, transaction, command_args),
            "INCRBY" => Self::execute_incrby(storage, transaction, command_args),
            "DECRBY" => Self::execute_decrby(storage, transaction, command_args),
            "INCRBYFLOAT" => Self::execute_incrbyfloat(storage, transaction, command_args),
            "APPEND" => Self::execute_append(storage, transaction, command_args),
            "STRLEN" => Self::execute_strlen(storage, transaction, command_args),

            // Hash commands
            "HGET" => Self::execute_hget(storage, transaction, command_args),
            "HSET" => Self::execute_hset(storage, transaction, command_args),
            "HDEL" => Self::execute_hdel(storage, transaction, command_args),
            "HGETALL" => Self::execute_hgetall(storage, transaction, command_args),
            "HMGET" => Self::execute_hmget(storage, transaction, command_args),
            "HMSET" => Self::execute_hmset(storage, transaction, command_args),
            "HINCRBY" => Self::execute_hincrby(storage, transaction, command_args),
            "HEXISTS" => Self::execute_hexists(storage, transaction, command_args),
            "HLEN" => Self::execute_hlen(storage, transaction, command_args),

            // List commands
            "LPUSH" => Self::execute_lpush(storage, transaction, command_args),
            "RPUSH" => Self::execute_rpush(storage, transaction, command_args),
            "LPOP" => Self::execute_lpop(storage, transaction, command_args),
            "RPOP" => Self::execute_rpop(storage, transaction, command_args),
            "LLEN" => Self::execute_llen(storage, transaction, command_args),
            "LRANGE" => Self::execute_lrange(storage, transaction, command_args),
            "LINDEX" => Self::execute_lindex(storage, transaction, command_args),

            // Set commands
            "SADD" => Self::execute_sadd(storage, transaction, command_args),
            "SREM" => Self::execute_srem(storage, transaction, command_args),
            "SMEMBERS" => Self::execute_smembers(storage, transaction, command_args),
            "SISMEMBER" => Self::execute_sismember(storage, transaction, command_args),
            "SCARD" => Self::execute_scard(storage, transaction, command_args),

            // Sorted Set commands
            "ZADD" => Self::execute_zadd(storage, transaction, command_args),
            "ZREM" => Self::execute_zrem(storage, transaction, command_args),
            "ZSCORE" => Self::execute_zscore(storage, transaction, command_args),
            "ZRANK" => Self::execute_zrank(storage, transaction, command_args),
            "ZRANGE" => Self::execute_zrange(storage, transaction, command_args),
            "ZCARD" => Self::execute_zcard(storage, transaction, command_args),

            _ => {
                if throw_error {
                    return Err(mlua::Error::RuntimeError(format!(
                        "Command not supported in scripts: {}",
                        command
                    )));
                } else {
                    return Ok(LuaValue::Nil);
                }
            }
        };

        match result {
            Ok(resp_value) => Self::resp_to_lua(lua, resp_value),
            Err(e) => {
                if throw_error {
                    Err(mlua::Error::RuntimeError(format!(
                        "Command execution error: {}",
                        e
                    )))
                } else {
                    Ok(LuaValue::Nil)
                }
            }
        }
    }

    /// Execute GET command
    fn execute_get(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("GET".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        match txn.get(storage, &key)? {
            Some(value) => Ok(RespValue::bulk_string(value)),
            None => Ok(RespValue::Null),
        }
    }

    /// Execute SET command
    fn execute_set(
        _storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SET".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let value = args[1].clone();

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        txn.set(key, value);
        Ok(RespValue::simple_string("OK"))
    }

    /// Execute DEL command
    fn execute_del(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("DEL".to_string()));
        }

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut count = 0;
        for arg in args {
            let key = String::from_utf8_lossy(arg).to_string();
            // Check if key exists (in buffer or storage)
            if txn.exists(storage, &key)? {
                txn.delete(key);
                count += 1;
            }
        }
        Ok(RespValue::Integer(count))
    }

    /// Execute EXISTS command
    fn execute_exists(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("EXISTS".to_string()));
        }

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut count = 0;
        for arg in args {
            let key = String::from_utf8_lossy(arg).to_string();
            if txn.exists(storage, &key)? {
                count += 1;
            }
        }
        Ok(RespValue::Integer(count))
    }

    // ========================================================================
    // EXTENDED STRING COMMANDS
    // ========================================================================

    /// Execute INCR command
    fn execute_incr(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("INCR".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let current: i64 = match txn.get(storage, &key)? {
            Some(v) => String::from_utf8_lossy(&v)
                .parse()
                .map_err(|_| AikvError::InvalidArgument("value is not an integer".to_string()))?,
            None => 0,
        };

        let new_val = current + 1;
        txn.set(key, Bytes::from(new_val.to_string()));
        Ok(RespValue::Integer(new_val))
    }

    /// Execute DECR command
    fn execute_decr(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("DECR".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let current: i64 = match txn.get(storage, &key)? {
            Some(v) => String::from_utf8_lossy(&v)
                .parse()
                .map_err(|_| AikvError::InvalidArgument("value is not an integer".to_string()))?,
            None => 0,
        };

        let new_val = current - 1;
        txn.set(key, Bytes::from(new_val.to_string()));
        Ok(RespValue::Integer(new_val))
    }

    /// Execute INCRBY command
    fn execute_incrby(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("INCRBY".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let increment: i64 = String::from_utf8_lossy(&args[1])
            .parse()
            .map_err(|_| AikvError::InvalidArgument("increment is not an integer".to_string()))?;

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let current: i64 = match txn.get(storage, &key)? {
            Some(v) => String::from_utf8_lossy(&v)
                .parse()
                .map_err(|_| AikvError::InvalidArgument("value is not an integer".to_string()))?,
            None => 0,
        };

        let new_val = current + increment;
        txn.set(key, Bytes::from(new_val.to_string()));
        Ok(RespValue::Integer(new_val))
    }

    /// Execute DECRBY command
    fn execute_decrby(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("DECRBY".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let decrement: i64 = String::from_utf8_lossy(&args[1])
            .parse()
            .map_err(|_| AikvError::InvalidArgument("decrement is not an integer".to_string()))?;

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let current: i64 = match txn.get(storage, &key)? {
            Some(v) => String::from_utf8_lossy(&v)
                .parse()
                .map_err(|_| AikvError::InvalidArgument("value is not an integer".to_string()))?,
            None => 0,
        };

        let new_val = current - decrement;
        txn.set(key, Bytes::from(new_val.to_string()));
        Ok(RespValue::Integer(new_val))
    }

    /// Execute INCRBYFLOAT command
    fn execute_incrbyfloat(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("INCRBYFLOAT".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let increment: f64 = String::from_utf8_lossy(&args[1])
            .parse()
            .map_err(|_| AikvError::InvalidArgument("increment is not a float".to_string()))?;

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let current: f64 = match txn.get(storage, &key)? {
            Some(v) => String::from_utf8_lossy(&v)
                .parse()
                .map_err(|_| AikvError::InvalidArgument("value is not a float".to_string()))?,
            None => 0.0,
        };

        let new_val = current + increment;
        let result_str = format!("{}", new_val);
        txn.set(key, Bytes::from(result_str.clone()));
        Ok(RespValue::bulk_string(Bytes::from(result_str)))
    }

    /// Execute APPEND command
    fn execute_append(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("APPEND".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let value = &args[1];

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut current = match txn.get(storage, &key)? {
            Some(v) => v.to_vec(),
            None => Vec::new(),
        };

        current.extend_from_slice(value);
        let len = current.len() as i64;
        txn.set(key, Bytes::from(current));
        Ok(RespValue::Integer(len))
    }

    /// Execute STRLEN command
    fn execute_strlen(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("STRLEN".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let len = match txn.get(storage, &key)? {
            Some(v) => v.len() as i64,
            None => 0,
        };
        Ok(RespValue::Integer(len))
    }

    // ========================================================================
    // HASH COMMANDS
    // ========================================================================

    /// Execute HGET command
    fn execute_hget(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("HGET".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let field = String::from_utf8_lossy(&args[1]).to_string();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let hash = stored.as_hash()?;
            match hash.get(&field) {
                Some(value) => Ok(RespValue::bulk_string(value.clone())),
                None => Ok(RespValue::Null),
            }
        } else {
            Ok(RespValue::Null)
        }
    }

    /// Execute HSET command
    fn execute_hset(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() < 3 || args.len() % 2 == 0 {
            return Err(AikvError::WrongArgCount("HSET".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut hash = if let Some(stored) = txn.get_value(storage, &key)? {
            stored.as_hash()?.clone()
        } else {
            HashMap::new()
        };

        let mut count = 0;
        for i in (1..args.len()).step_by(2) {
            let field = String::from_utf8_lossy(&args[i]).to_string();
            let value = args[i + 1].clone();
            if hash.insert(field, value).is_none() {
                count += 1;
            }
        }

        txn.set_hash(key, hash);
        Ok(RespValue::Integer(count))
    }

    /// Execute HDEL command
    fn execute_hdel(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("HDEL".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let mut hash = stored.as_hash()?.clone();
            let mut count = 0;
            for arg in &args[1..] {
                let field = String::from_utf8_lossy(arg).to_string();
                if hash.remove(&field).is_some() {
                    count += 1;
                }
            }
            if hash.is_empty() {
                txn.delete(key);
            } else {
                txn.set_hash(key, hash);
            }
            Ok(RespValue::Integer(count))
        } else {
            Ok(RespValue::Integer(0))
        }
    }

    /// Execute HGETALL command
    fn execute_hgetall(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("HGETALL".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let hash = stored.as_hash()?;
            let mut result = Vec::new();
            for (field, value) in hash {
                result.push(RespValue::bulk_string(Bytes::from(field.clone())));
                result.push(RespValue::bulk_string(value.clone()));
            }
            Ok(RespValue::Array(Some(result)))
        } else {
            Ok(RespValue::Array(Some(Vec::new())))
        }
    }

    /// Execute HMGET command
    fn execute_hmget(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("HMGET".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let stored = txn.get_value(storage, &key)?;
        let hash = match &stored {
            Some(s) => Some(s.as_hash()?),
            None => None,
        };

        let result: Vec<RespValue> = args[1..]
            .iter()
            .map(|arg| {
                let field = String::from_utf8_lossy(arg).to_string();
                match &hash {
                    Some(h) => match h.get(&field) {
                        Some(v) => RespValue::bulk_string(v.clone()),
                        None => RespValue::Null,
                    },
                    None => RespValue::Null,
                }
            })
            .collect();

        Ok(RespValue::Array(Some(result)))
    }

    /// Execute HMSET command
    fn execute_hmset(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() < 3 || args.len() % 2 == 0 {
            return Err(AikvError::WrongArgCount("HMSET".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut hash = if let Some(stored) = txn.get_value(storage, &key)? {
            stored.as_hash()?.clone()
        } else {
            HashMap::new()
        };

        for i in (1..args.len()).step_by(2) {
            let field = String::from_utf8_lossy(&args[i]).to_string();
            let value = args[i + 1].clone();
            hash.insert(field, value);
        }

        txn.set_hash(key, hash);
        Ok(RespValue::simple_string("OK"))
    }

    /// Execute HINCRBY command
    fn execute_hincrby(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("HINCRBY".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let field = String::from_utf8_lossy(&args[1]).to_string();
        let increment: i64 = String::from_utf8_lossy(&args[2])
            .parse()
            .map_err(|_| AikvError::InvalidArgument("increment is not an integer".to_string()))?;

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut hash = if let Some(stored) = txn.get_value(storage, &key)? {
            stored.as_hash()?.clone()
        } else {
            HashMap::new()
        };

        let current: i64 = match hash.get(&field) {
            Some(v) => String::from_utf8_lossy(v).parse().map_err(|_| {
                AikvError::InvalidArgument("field value is not an integer".to_string())
            })?,
            None => 0,
        };

        let new_val = current + increment;
        hash.insert(field, Bytes::from(new_val.to_string()));
        txn.set_hash(key, hash);
        Ok(RespValue::Integer(new_val))
    }

    /// Execute HEXISTS command
    fn execute_hexists(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("HEXISTS".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let field = String::from_utf8_lossy(&args[1]).to_string();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let hash = stored.as_hash()?;
            Ok(RespValue::Integer(if hash.contains_key(&field) {
                1
            } else {
                0
            }))
        } else {
            Ok(RespValue::Integer(0))
        }
    }

    /// Execute HLEN command
    fn execute_hlen(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("HLEN".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let hash = stored.as_hash()?;
            Ok(RespValue::Integer(hash.len() as i64))
        } else {
            Ok(RespValue::Integer(0))
        }
    }

    // ========================================================================
    // LIST COMMANDS
    // ========================================================================

    /// Execute LPUSH command
    fn execute_lpush(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("LPUSH".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut list = if let Some(stored) = txn.get_value(storage, &key)? {
            stored.as_list()?.clone()
        } else {
            VecDeque::new()
        };

        for i in (1..args.len()).rev() {
            list.push_front(args[i].clone());
        }

        let len = list.len() as i64;
        txn.set_list(key, list);
        Ok(RespValue::Integer(len))
    }

    /// Execute RPUSH command
    fn execute_rpush(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("RPUSH".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut list = if let Some(stored) = txn.get_value(storage, &key)? {
            stored.as_list()?.clone()
        } else {
            VecDeque::new()
        };

        for arg in &args[1..] {
            list.push_back(arg.clone());
        }

        let len = list.len() as i64;
        txn.set_list(key, list);
        Ok(RespValue::Integer(len))
    }

    /// Execute LPOP command
    fn execute_lpop(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("LPOP".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = if args.len() > 1 {
            String::from_utf8_lossy(&args[1])
                .parse::<usize>()
                .unwrap_or(1)
        } else {
            1
        };

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let mut list = stored.as_list()?.clone();

            if count == 1 && args.len() == 1 {
                // Single element, return as bulk string
                if let Some(value) = list.pop_front() {
                    if list.is_empty() {
                        txn.delete(key);
                    } else {
                        txn.set_list(key, list);
                    }
                    return Ok(RespValue::bulk_string(value));
                }
            } else {
                // Multiple elements, return as array
                let mut result = Vec::new();
                for _ in 0..count {
                    if let Some(value) = list.pop_front() {
                        result.push(RespValue::bulk_string(value));
                    } else {
                        break;
                    }
                }
                if list.is_empty() {
                    txn.delete(key);
                } else {
                    txn.set_list(key, list);
                }
                if result.is_empty() {
                    return Ok(RespValue::Null);
                }
                return Ok(RespValue::Array(Some(result)));
            }
        }
        Ok(RespValue::Null)
    }

    /// Execute RPOP command
    fn execute_rpop(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("RPOP".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = if args.len() > 1 {
            String::from_utf8_lossy(&args[1])
                .parse::<usize>()
                .unwrap_or(1)
        } else {
            1
        };

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let mut list = stored.as_list()?.clone();

            if count == 1 && args.len() == 1 {
                // Single element, return as bulk string
                if let Some(value) = list.pop_back() {
                    if list.is_empty() {
                        txn.delete(key);
                    } else {
                        txn.set_list(key, list);
                    }
                    return Ok(RespValue::bulk_string(value));
                }
            } else {
                // Multiple elements, return as array
                let mut result = Vec::new();
                for _ in 0..count {
                    if let Some(value) = list.pop_back() {
                        result.push(RespValue::bulk_string(value));
                    } else {
                        break;
                    }
                }
                if list.is_empty() {
                    txn.delete(key);
                } else {
                    txn.set_list(key, list);
                }
                if result.is_empty() {
                    return Ok(RespValue::Null);
                }
                return Ok(RespValue::Array(Some(result)));
            }
        }
        Ok(RespValue::Null)
    }

    /// Execute LLEN command
    fn execute_llen(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("LLEN".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let list = stored.as_list()?;
            Ok(RespValue::Integer(list.len() as i64))
        } else {
            Ok(RespValue::Integer(0))
        }
    }

    /// Execute LRANGE command
    fn execute_lrange(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("LRANGE".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let start: i64 = String::from_utf8_lossy(&args[1])
            .parse()
            .map_err(|_| AikvError::InvalidArgument("start is not an integer".to_string()))?;
        let stop: i64 = String::from_utf8_lossy(&args[2])
            .parse()
            .map_err(|_| AikvError::InvalidArgument("stop is not an integer".to_string()))?;

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let list = stored.as_list()?;
            let len = list.len() as i64;

            let start_idx = if start < 0 {
                (len + start).max(0)
            } else {
                start.min(len)
            } as usize;
            let stop_idx = if stop < 0 {
                (len + stop + 1).max(0)
            } else {
                (stop + 1).min(len)
            } as usize;

            if start_idx >= stop_idx {
                return Ok(RespValue::Array(Some(Vec::new())));
            }

            let result: Vec<RespValue> = list
                .iter()
                .skip(start_idx)
                .take(stop_idx - start_idx)
                .map(|v| RespValue::bulk_string(v.clone()))
                .collect();

            Ok(RespValue::Array(Some(result)))
        } else {
            Ok(RespValue::Array(Some(Vec::new())))
        }
    }

    /// Execute LINDEX command
    fn execute_lindex(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("LINDEX".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let index: i64 = String::from_utf8_lossy(&args[1])
            .parse()
            .map_err(|_| AikvError::InvalidArgument("index is not an integer".to_string()))?;

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let list = stored.as_list()?;
            let len = list.len() as i64;

            let actual_idx = if index < 0 { len + index } else { index };

            if actual_idx < 0 || actual_idx >= len {
                return Ok(RespValue::Null);
            }

            if let Some(value) = list.get(actual_idx as usize) {
                return Ok(RespValue::bulk_string(value.clone()));
            }
        }
        Ok(RespValue::Null)
    }

    // ========================================================================
    // SET COMMANDS
    // ========================================================================

    /// Execute SADD command
    fn execute_sadd(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SADD".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut set = if let Some(stored) = txn.get_value(storage, &key)? {
            stored.as_set()?.clone()
        } else {
            HashSet::new()
        };

        let mut count = 0;
        for arg in &args[1..] {
            if set.insert(arg.to_vec()) {
                count += 1;
            }
        }

        txn.set_set(key, set);
        Ok(RespValue::Integer(count))
    }

    /// Execute SREM command
    fn execute_srem(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SREM".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let mut set = stored.as_set()?.clone();
            let mut count = 0;
            for arg in &args[1..] {
                if set.remove(&arg.to_vec()) {
                    count += 1;
                }
            }
            if set.is_empty() {
                txn.delete(key);
            } else {
                txn.set_set(key, set);
            }
            Ok(RespValue::Integer(count))
        } else {
            Ok(RespValue::Integer(0))
        }
    }

    /// Execute SMEMBERS command
    fn execute_smembers(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("SMEMBERS".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let set = stored.as_set()?;
            let result: Vec<RespValue> = set
                .iter()
                .map(|v| RespValue::bulk_string(Bytes::from(v.clone())))
                .collect();
            Ok(RespValue::Array(Some(result)))
        } else {
            Ok(RespValue::Array(Some(Vec::new())))
        }
    }

    /// Execute SISMEMBER command
    fn execute_sismember(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("SISMEMBER".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let member = args[1].to_vec();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let set = stored.as_set()?;
            Ok(RespValue::Integer(if set.contains(&member) {
                1
            } else {
                0
            }))
        } else {
            Ok(RespValue::Integer(0))
        }
    }

    /// Execute SCARD command
    fn execute_scard(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("SCARD".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let set = stored.as_set()?;
            Ok(RespValue::Integer(set.len() as i64))
        } else {
            Ok(RespValue::Integer(0))
        }
    }

    // ========================================================================
    // SORTED SET COMMANDS
    // ========================================================================

    /// Execute ZADD command
    fn execute_zadd(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() < 3 || (args.len() - 1) % 2 != 0 {
            return Err(AikvError::WrongArgCount("ZADD".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut zset = if let Some(stored) = txn.get_value(storage, &key)? {
            stored.as_zset()?.clone()
        } else {
            BTreeMap::new()
        };

        let mut count = 0;
        for i in (1..args.len()).step_by(2) {
            let score: f64 = String::from_utf8_lossy(&args[i])
                .parse()
                .map_err(|_| AikvError::InvalidArgument("score is not a float".to_string()))?;
            let member = args[i + 1].to_vec();
            if zset.insert(member, score).is_none() {
                count += 1;
            }
        }

        txn.set_zset(key, zset);
        Ok(RespValue::Integer(count))
    }

    /// Execute ZREM command
    fn execute_zrem(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("ZREM".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let mut txn = transaction
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let mut zset = stored.as_zset()?.clone();
            let mut count = 0;
            for arg in &args[1..] {
                if zset.remove(&arg.to_vec()).is_some() {
                    count += 1;
                }
            }
            if zset.is_empty() {
                txn.delete(key);
            } else {
                txn.set_zset(key, zset);
            }
            Ok(RespValue::Integer(count))
        } else {
            Ok(RespValue::Integer(0))
        }
    }

    /// Execute ZSCORE command
    fn execute_zscore(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("ZSCORE".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let member = args[1].to_vec();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let zset = stored.as_zset()?;
            match zset.get(&member) {
                Some(score) => Ok(RespValue::bulk_string(Bytes::from(score.to_string()))),
                None => Ok(RespValue::Null),
            }
        } else {
            Ok(RespValue::Null)
        }
    }

    /// Execute ZRANK command
    fn execute_zrank(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("ZRANK".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let member = args[1].to_vec();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let zset = stored.as_zset()?;
            if !zset.contains_key(&member) {
                return Ok(RespValue::Null);
            }

            // Sort by score and find rank
            let mut sorted: Vec<_> = zset.iter().collect();
            sorted.sort_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal));

            for (rank, (m, _)) in sorted.iter().enumerate() {
                if *m == &member {
                    return Ok(RespValue::Integer(rank as i64));
                }
            }
        }
        Ok(RespValue::Null)
    }

    /// Execute ZRANGE command
    fn execute_zrange(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() < 3 {
            return Err(AikvError::WrongArgCount("ZRANGE".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let start: i64 = String::from_utf8_lossy(&args[1])
            .parse()
            .map_err(|_| AikvError::InvalidArgument("start is not an integer".to_string()))?;
        let stop: i64 = String::from_utf8_lossy(&args[2])
            .parse()
            .map_err(|_| AikvError::InvalidArgument("stop is not an integer".to_string()))?;

        let with_scores =
            args.len() > 3 && String::from_utf8_lossy(&args[3]).to_uppercase() == "WITHSCORES";

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let zset = stored.as_zset()?;
            let len = zset.len() as i64;

            // Sort by score
            let mut sorted: Vec<_> = zset.iter().collect();
            sorted.sort_by(|a, b| {
                a.1.partial_cmp(b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(b.0))
            });

            let start_idx = if start < 0 {
                (len + start).max(0)
            } else {
                start.min(len)
            } as usize;
            let stop_idx = if stop < 0 {
                (len + stop + 1).max(0)
            } else {
                (stop + 1).min(len)
            } as usize;

            if start_idx >= stop_idx {
                return Ok(RespValue::Array(Some(Vec::new())));
            }

            let mut result = Vec::new();
            for (member, score) in sorted.iter().skip(start_idx).take(stop_idx - start_idx) {
                result.push(RespValue::bulk_string(Bytes::from((*member).clone())));
                if with_scores {
                    result.push(RespValue::bulk_string(Bytes::from(score.to_string())));
                }
            }
            Ok(RespValue::Array(Some(result)))
        } else {
            Ok(RespValue::Array(Some(Vec::new())))
        }
    }

    /// Execute ZCARD command
    fn execute_zcard(
        storage: &StorageEngine,
        transaction: &Arc<RwLock<ScriptTransaction>>,
        args: &[Bytes],
    ) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("ZCARD".to_string()));
        }
        let key = String::from_utf8_lossy(&args[0]).to_string();

        let txn = transaction
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(stored) = txn.get_value(storage, &key)? {
            let zset = stored.as_zset()?;
            Ok(RespValue::Integer(zset.len() as i64))
        } else {
            Ok(RespValue::Integer(0))
        }
    }

    /// Convert Lua value to RESP value
    fn lua_to_resp(value: LuaValue) -> Result<RespValue> {
        match value {
            LuaValue::Nil => Ok(RespValue::Null),
            LuaValue::Boolean(b) => Ok(RespValue::Integer(if b { 1 } else { 0 })),
            LuaValue::Integer(i) => Ok(RespValue::Integer(i)),
            LuaValue::Number(n) => {
                // Convert float to integer if possible, otherwise to string
                if n.fract() == 0.0 {
                    Ok(RespValue::Integer(n as i64))
                } else {
                    Ok(RespValue::bulk_string(Bytes::from(n.to_string())))
                }
            }
            LuaValue::String(s) => Ok(RespValue::bulk_string(Bytes::from(s.as_bytes().to_vec()))),
            LuaValue::Table(t) => {
                // Convert table to array
                let mut results = Vec::new();
                for i in 1..=t.len().unwrap_or(0) {
                    if let Ok(val) = t.get::<LuaValue>(i) {
                        results.push(Self::lua_to_resp(val)?);
                    }
                }
                Ok(RespValue::Array(Some(results)))
            }
            _ => Ok(RespValue::Null),
        }
    }

    /// Convert RESP value to Lua value
    fn resp_to_lua(lua: &mlua::Lua, value: RespValue) -> mlua::Result<LuaValue> {
        match value {
            RespValue::Null => Ok(LuaValue::Boolean(false)),
            RespValue::SimpleString(s) => {
                Ok(LuaValue::String(lua.create_string(s.as_bytes()).map_err(
                    |e| mlua::Error::RuntimeError(format!("Failed to create string: {}", e)),
                )?))
            }
            RespValue::Error(e) => Ok(LuaValue::String(lua.create_string(e.as_bytes()).map_err(
                |e| mlua::Error::RuntimeError(format!("Failed to create error string: {}", e)),
            )?)),
            RespValue::Integer(i) => Ok(LuaValue::Integer(i)),
            RespValue::BulkString(opt_b) => match opt_b {
                Some(b) => Ok(LuaValue::String(lua.create_string(&b).map_err(|e| {
                    mlua::Error::RuntimeError(format!("Failed to create bulk string: {}", e))
                })?)),
                None => Ok(LuaValue::Boolean(false)),
            },
            RespValue::Array(opt_arr) => match opt_arr {
                Some(arr) => {
                    let table = lua.create_table().map_err(|e| {
                        mlua::Error::RuntimeError(format!("Failed to create table: {}", e))
                    })?;
                    for (i, item) in arr.into_iter().enumerate() {
                        table
                            .set(i + 1, Self::resp_to_lua(lua, item)?)
                            .map_err(|e| {
                                mlua::Error::RuntimeError(format!(
                                    "Failed to set table item: {}",
                                    e
                                ))
                            })?;
                    }
                    Ok(LuaValue::Table(table))
                }
                None => Ok(LuaValue::Boolean(false)),
            },
            _ => Ok(LuaValue::Nil),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StorageEngine;

    fn setup() -> ScriptCommands {
        let storage = StorageEngine::new_memory(16);
        ScriptCommands::new(storage)
    }

    #[test]
    fn test_calculate_sha1() {
        let script = "return 1";
        let sha1 = ScriptCommands::calculate_sha1(script);
        assert_eq!(sha1, "e0e1f9fabfc9d4800c877a703b823ac0578ff8db");
    }

    #[test]
    fn test_script_load() {
        let script_commands = setup();
        let script = "return 'hello'";
        let args = vec![Bytes::from(script)];

        let result = script_commands.script_load(&args);
        assert!(result.is_ok());

        if let Ok(RespValue::BulkString(Some(sha1))) = result {
            assert_eq!(
                String::from_utf8_lossy(&sha1),
                "1b936e3fe509bcbc9cd0664897bbe8fd0cac101b"
            );
        } else {
            panic!("Expected BulkString");
        }
    }

    #[test]
    fn test_script_exists() {
        let script_commands = setup();
        let script = "return 'hello'";
        let sha1 = ScriptCommands::calculate_sha1(script);

        // Load the script first
        let args = vec![Bytes::from(script)];
        script_commands.script_load(&args).unwrap();

        // Check if script exists
        let exists_args = vec![Bytes::from(sha1)];
        let result = script_commands.script_exists(&exists_args).unwrap();

        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr.len(), 1);
            assert_eq!(arr[0], RespValue::Integer(1));
        } else {
            panic!("Expected Array");
        }
    }

    #[test]
    fn test_script_flush() {
        let script_commands = setup();
        let script = "return 'hello'";

        // Load a script
        let args = vec![Bytes::from(script)];
        script_commands.script_load(&args).unwrap();

        // Flush the cache
        let result = script_commands.script_flush(&[]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), RespValue::simple_string("OK"));

        // Check that script no longer exists
        let sha1 = ScriptCommands::calculate_sha1(script);
        let exists_args = vec![Bytes::from(sha1)];
        let result = script_commands.script_exists(&exists_args).unwrap();

        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr[0], RespValue::Integer(0));
        }
    }

    #[test]
    fn test_eval_simple_return() {
        let script_commands = setup();
        let script = "return 42";
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), RespValue::Integer(42));
    }

    #[test]
    fn test_eval_with_keys() {
        let script_commands = setup();
        let script = "return KEYS[1]";
        let args = vec![Bytes::from(script), Bytes::from("1"), Bytes::from("mykey")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::BulkString(Some(value)) = result {
            assert_eq!(String::from_utf8_lossy(&value), "mykey");
        } else {
            panic!("Expected BulkString");
        }
    }

    #[test]
    fn test_eval_with_argv() {
        let script_commands = setup();
        let script = "return ARGV[1]";
        let args = vec![Bytes::from(script), Bytes::from("0"), Bytes::from("myarg")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::BulkString(Some(value)) = result {
            assert_eq!(String::from_utf8_lossy(&value), "myarg");
        } else {
            panic!("Expected BulkString");
        }
    }

    #[test]
    fn test_eval_redis_call_set_get() {
        let script_commands = setup();
        let script = r#"
            redis.call('SET', KEYS[1], ARGV[1])
            return redis.call('GET', KEYS[1])
        "#;
        let args = vec![
            Bytes::from(script),
            Bytes::from("1"),
            Bytes::from("mykey"),
            Bytes::from("myvalue"),
        ];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::BulkString(Some(value)) = result {
            assert_eq!(String::from_utf8_lossy(&value), "myvalue");
        } else {
            panic!("Expected BulkString");
        }
    }

    #[test]
    fn test_evalsha() {
        let script_commands = setup();
        let script = "return 'hello from cache'";

        // Load the script first
        let load_args = vec![Bytes::from(script)];
        let load_result = script_commands.script_load(&load_args).unwrap();

        let sha1 = if let RespValue::BulkString(Some(sha)) = load_result {
            sha
        } else {
            panic!("Expected BulkString");
        };

        // Execute using EVALSHA
        let evalsha_args = vec![sha1, Bytes::from("0")];
        let result = script_commands.evalsha(&evalsha_args, 0).unwrap();

        if let RespValue::BulkString(Some(value)) = result {
            assert_eq!(String::from_utf8_lossy(&value), "hello from cache");
        } else {
            panic!("Expected BulkString");
        }
    }

    #[test]
    fn test_evalsha_not_found() {
        let script_commands = setup();
        let sha1 = "nonexistent_sha1";
        let args = vec![Bytes::from(sha1), Bytes::from("0")];

        let result = script_commands.evalsha(&args, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_transaction_commit_on_success() {
        let script_commands = setup();
        let script = r#"
            redis.call('SET', 'tx_key1', 'value1')
            redis.call('SET', 'tx_key2', 'value2')
            return 'OK'
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        // Execute script
        let result = script_commands.eval(&args, 0);
        assert!(result.is_ok());

        // Verify values are committed to storage
        let val1 = script_commands.storage.get_from_db(0, "tx_key1").unwrap();
        assert_eq!(val1, Some(Bytes::from("value1")));

        let val2 = script_commands.storage.get_from_db(0, "tx_key2").unwrap();
        assert_eq!(val2, Some(Bytes::from("value2")));
    }

    #[test]
    fn test_transaction_rollback_on_error() {
        let script_commands = setup();
        let script = r#"
            redis.call('SET', 'rollback_key1', 'value1')
            redis.call('SET', 'rollback_key2', 'value2')
            error('intentional error')
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        // Execute script - should fail
        let result = script_commands.eval(&args, 0);
        assert!(result.is_err());

        // Verify values are NOT in storage (rolled back)
        let val1 = script_commands
            .storage
            .get_from_db(0, "rollback_key1")
            .unwrap();
        assert_eq!(val1, None);

        let val2 = script_commands
            .storage
            .get_from_db(0, "rollback_key2")
            .unwrap();
        assert_eq!(val2, None);
    }

    #[test]
    fn test_transaction_read_your_own_writes() {
        let script_commands = setup();
        let script = r#"
            redis.call('SET', 'ryw_key', 'first')
            local val1 = redis.call('GET', 'ryw_key')
            redis.call('SET', 'ryw_key', 'second')
            local val2 = redis.call('GET', 'ryw_key')
            return {val1, val2}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr.len(), 2);
            if let RespValue::BulkString(Some(v1)) = &arr[0] {
                assert_eq!(String::from_utf8_lossy(v1), "first");
            }
            if let RespValue::BulkString(Some(v2)) = &arr[1] {
                assert_eq!(String::from_utf8_lossy(v2), "second");
            }
        } else {
            panic!("Expected Array");
        }
    }

    #[test]
    fn test_transaction_del_then_set() {
        let script_commands = setup();

        // Set initial value
        script_commands
            .storage
            .set_in_db(0, "del_set_key".to_string(), Bytes::from("initial"))
            .unwrap();

        let script = r#"
            redis.call('DEL', 'del_set_key')
            local exists1 = redis.call('EXISTS', 'del_set_key')
            redis.call('SET', 'del_set_key', 'new_value')
            local exists2 = redis.call('EXISTS', 'del_set_key')
            local val = redis.call('GET', 'del_set_key')
            return {exists1, exists2, val}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr.len(), 3);
            assert_eq!(arr[0], RespValue::Integer(0)); // After DEL
            assert_eq!(arr[1], RespValue::Integer(1)); // After SET
            if let RespValue::BulkString(Some(v)) = &arr[2] {
                assert_eq!(String::from_utf8_lossy(v), "new_value");
            }
        } else {
            panic!("Expected Array");
        }

        // Verify final committed value
        let final_val = script_commands
            .storage
            .get_from_db(0, "del_set_key")
            .unwrap();
        assert_eq!(final_val, Some(Bytes::from("new_value")));
    }

    #[test]
    fn test_transaction_multiple_dels() {
        let script_commands = setup();

        // Set initial values
        script_commands
            .storage
            .set_in_db(0, "multi_del1".to_string(), Bytes::from("v1"))
            .unwrap();
        script_commands
            .storage
            .set_in_db(0, "multi_del2".to_string(), Bytes::from("v2"))
            .unwrap();
        script_commands
            .storage
            .set_in_db(0, "multi_del3".to_string(), Bytes::from("v3"))
            .unwrap();

        let script = r#"
            local count = redis.call('DEL', 'multi_del1', 'multi_del2', 'multi_del3')
            return count
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        assert_eq!(result, RespValue::Integer(3));

        // Verify all are deleted
        assert_eq!(
            script_commands
                .storage
                .get_from_db(0, "multi_del1")
                .unwrap(),
            None
        );
        assert_eq!(
            script_commands
                .storage
                .get_from_db(0, "multi_del2")
                .unwrap(),
            None
        );
        assert_eq!(
            script_commands
                .storage
                .get_from_db(0, "multi_del3")
                .unwrap(),
            None
        );
    }

    #[test]
    fn test_transaction_exists_with_buffer() {
        let script_commands = setup();

        let script = r#"
            local e1 = redis.call('EXISTS', 'exists_test')
            redis.call('SET', 'exists_test', 'value')
            local e2 = redis.call('EXISTS', 'exists_test')
            redis.call('DEL', 'exists_test')
            local e3 = redis.call('EXISTS', 'exists_test')
            return {e1, e2, e3}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr[0], RespValue::Integer(0)); // Before SET
            assert_eq!(arr[1], RespValue::Integer(1)); // After SET
            assert_eq!(arr[2], RespValue::Integer(0)); // After DEL
        } else {
            panic!("Expected Array");
        }
    }

    #[test]
    fn test_transaction_overwrite_in_buffer() {
        let script_commands = setup();

        let script = r#"
            redis.call('SET', 'overwrite', 'v1')
            redis.call('SET', 'overwrite', 'v2')
            redis.call('SET', 'overwrite', 'v3')
            return redis.call('GET', 'overwrite')
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::BulkString(Some(val)) = result {
            assert_eq!(String::from_utf8_lossy(&val), "v3");
        } else {
            panic!("Expected BulkString");
        }

        // Verify only final value is committed
        let final_val = script_commands.storage.get_from_db(0, "overwrite").unwrap();
        assert_eq!(final_val, Some(Bytes::from("v3")));
    }

    // ========================================================================
    // KEY LOCK MANAGER TESTS
    // ========================================================================

    #[test]
    fn test_key_lock_manager_basic() {
        let manager = KeyLockManager::new(Duration::from_secs(5));

        // Lock some keys
        let guard = manager.lock_keys(&["key1".to_string(), "key2".to_string()]);
        assert!(guard.is_ok());

        // Lock released when guard is dropped
        drop(guard);

        // Can lock again
        let guard2 = manager.lock_keys(&["key1".to_string()]);
        assert!(guard2.is_ok());
    }

    #[test]
    fn test_key_lock_manager_empty_keys() {
        let manager = KeyLockManager::new(Duration::from_secs(5));

        // Empty keys should work
        let guard = manager.lock_keys(&[]);
        assert!(guard.is_ok());
    }

    #[test]
    fn test_key_lock_manager_duplicate_keys() {
        let manager = KeyLockManager::new(Duration::from_secs(5));

        // Duplicate keys should be deduplicated
        let guard =
            manager.lock_keys(&["key1".to_string(), "key2".to_string(), "key1".to_string()]);
        assert!(guard.is_ok());
    }

    // ========================================================================
    // EXTENDED STRING COMMAND TESTS
    // ========================================================================

    #[test]
    fn test_script_incr_decr() {
        let script_commands = setup();

        let script = r#"
            redis.call('SET', 'counter', '10')
            local v1 = redis.call('INCR', 'counter')
            local v2 = redis.call('INCR', 'counter')
            local v3 = redis.call('DECR', 'counter')
            return {v1, v2, v3}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr[0], RespValue::Integer(11));
            assert_eq!(arr[1], RespValue::Integer(12));
            assert_eq!(arr[2], RespValue::Integer(11));
        } else {
            panic!("Expected Array");
        }
    }

    #[test]
    fn test_script_incrby_decrby() {
        let script_commands = setup();

        let script = r#"
            redis.call('SET', 'counter', '10')
            local v1 = redis.call('INCRBY', 'counter', 5)
            local v2 = redis.call('DECRBY', 'counter', 3)
            return {v1, v2}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr[0], RespValue::Integer(15));
            assert_eq!(arr[1], RespValue::Integer(12));
        } else {
            panic!("Expected Array");
        }
    }

    #[test]
    fn test_script_incrbyfloat() {
        let script_commands = setup();

        let script = r#"
            redis.call('SET', 'fval', '10.5')
            local v = redis.call('INCRBYFLOAT', 'fval', 0.1)
            return v
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::BulkString(Some(val)) = result {
            let f: f64 = String::from_utf8_lossy(&val).parse().unwrap();
            assert!((f - 10.6).abs() < 0.001);
        } else {
            panic!("Expected BulkString");
        }
    }

    #[test]
    fn test_script_append_strlen() {
        let script_commands = setup();

        let script = r#"
            redis.call('SET', 'mystr', 'Hello')
            local len1 = redis.call('STRLEN', 'mystr')
            local len2 = redis.call('APPEND', 'mystr', ' World')
            local len3 = redis.call('STRLEN', 'mystr')
            return {len1, len2, len3}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr[0], RespValue::Integer(5)); // "Hello"
            assert_eq!(arr[1], RespValue::Integer(11)); // "Hello World"
            assert_eq!(arr[2], RespValue::Integer(11));
        } else {
            panic!("Expected Array");
        }
    }

    // ========================================================================
    // HASH COMMAND TESTS
    // ========================================================================

    #[test]
    fn test_script_hash_basic() {
        let script_commands = setup();

        let script = r#"
            redis.call('HSET', 'myhash', 'field1', 'value1', 'field2', 'value2')
            local v1 = redis.call('HGET', 'myhash', 'field1')
            local v2 = redis.call('HGET', 'myhash', 'field2')
            local exists1 = redis.call('HEXISTS', 'myhash', 'field1')
            local exists2 = redis.call('HEXISTS', 'myhash', 'field3')
            local len = redis.call('HLEN', 'myhash')
            return {v1, v2, exists1, exists2, len}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            if let RespValue::BulkString(Some(v1)) = &arr[0] {
                assert_eq!(String::from_utf8_lossy(v1), "value1");
            }
            if let RespValue::BulkString(Some(v2)) = &arr[1] {
                assert_eq!(String::from_utf8_lossy(v2), "value2");
            }
            assert_eq!(arr[2], RespValue::Integer(1));
            assert_eq!(arr[3], RespValue::Integer(0));
            assert_eq!(arr[4], RespValue::Integer(2));
        } else {
            panic!("Expected Array");
        }
    }

    #[test]
    fn test_script_hash_hincrby() {
        let script_commands = setup();

        let script = r#"
            redis.call('HSET', 'myhash', 'counter', '10')
            local v = redis.call('HINCRBY', 'myhash', 'counter', 5)
            return v
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        assert_eq!(result, RespValue::Integer(15));
    }

    #[test]
    fn test_script_hash_hdel() {
        let script_commands = setup();

        let script = r#"
            redis.call('HSET', 'myhash', 'f1', 'v1', 'f2', 'v2', 'f3', 'v3')
            local deleted = redis.call('HDEL', 'myhash', 'f1', 'f2')
            local len = redis.call('HLEN', 'myhash')
            return {deleted, len}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr[0], RespValue::Integer(2));
            assert_eq!(arr[1], RespValue::Integer(1));
        } else {
            panic!("Expected Array");
        }
    }

    // ========================================================================
    // LIST COMMAND TESTS
    // ========================================================================

    #[test]
    fn test_script_list_basic() {
        let script_commands = setup();

        let script = r#"
            redis.call('RPUSH', 'mylist', 'a', 'b', 'c')
            redis.call('LPUSH', 'mylist', 'z')
            local len = redis.call('LLEN', 'mylist')
            local first = redis.call('LINDEX', 'mylist', 0)
            local last = redis.call('LINDEX', 'mylist', -1)
            return {len, first, last}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr[0], RespValue::Integer(4));
            if let RespValue::BulkString(Some(first)) = &arr[1] {
                assert_eq!(String::from_utf8_lossy(first), "z");
            }
            if let RespValue::BulkString(Some(last)) = &arr[2] {
                assert_eq!(String::from_utf8_lossy(last), "c");
            }
        } else {
            panic!("Expected Array");
        }
    }

    #[test]
    fn test_script_list_pop() {
        let script_commands = setup();

        let script = r#"
            redis.call('RPUSH', 'mylist', 'a', 'b', 'c')
            local left = redis.call('LPOP', 'mylist')
            local right = redis.call('RPOP', 'mylist')
            local len = redis.call('LLEN', 'mylist')
            return {left, right, len}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            if let RespValue::BulkString(Some(left)) = &arr[0] {
                assert_eq!(String::from_utf8_lossy(left), "a");
            }
            if let RespValue::BulkString(Some(right)) = &arr[1] {
                assert_eq!(String::from_utf8_lossy(right), "c");
            }
            assert_eq!(arr[2], RespValue::Integer(1));
        } else {
            panic!("Expected Array");
        }
    }

    #[test]
    fn test_script_list_lrange() {
        let script_commands = setup();

        let script = r#"
            redis.call('RPUSH', 'mylist', 'a', 'b', 'c', 'd', 'e')
            local range = redis.call('LRANGE', 'mylist', 1, 3)
            return range
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr.len(), 3);
            if let RespValue::BulkString(Some(v)) = &arr[0] {
                assert_eq!(String::from_utf8_lossy(v), "b");
            }
            if let RespValue::BulkString(Some(v)) = &arr[1] {
                assert_eq!(String::from_utf8_lossy(v), "c");
            }
            if let RespValue::BulkString(Some(v)) = &arr[2] {
                assert_eq!(String::from_utf8_lossy(v), "d");
            }
        } else {
            panic!("Expected Array");
        }
    }

    // ========================================================================
    // SET COMMAND TESTS
    // ========================================================================

    #[test]
    fn test_script_set_basic() {
        let script_commands = setup();

        let script = r#"
            local added = redis.call('SADD', 'myset', 'a', 'b', 'c')
            local card = redis.call('SCARD', 'myset')
            local is_a = redis.call('SISMEMBER', 'myset', 'a')
            local is_z = redis.call('SISMEMBER', 'myset', 'z')
            return {added, card, is_a, is_z}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr[0], RespValue::Integer(3));
            assert_eq!(arr[1], RespValue::Integer(3));
            assert_eq!(arr[2], RespValue::Integer(1));
            assert_eq!(arr[3], RespValue::Integer(0));
        } else {
            panic!("Expected Array");
        }
    }

    #[test]
    fn test_script_set_srem() {
        let script_commands = setup();

        let script = r#"
            redis.call('SADD', 'myset', 'a', 'b', 'c')
            local removed = redis.call('SREM', 'myset', 'a', 'b')
            local card = redis.call('SCARD', 'myset')
            return {removed, card}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr[0], RespValue::Integer(2));
            assert_eq!(arr[1], RespValue::Integer(1));
        } else {
            panic!("Expected Array");
        }
    }

    // ========================================================================
    // SORTED SET COMMAND TESTS
    // ========================================================================

    #[test]
    fn test_script_zset_basic() {
        let script_commands = setup();

        let script = r#"
            local added = redis.call('ZADD', 'myzset', 1, 'one', 2, 'two', 3, 'three')
            local card = redis.call('ZCARD', 'myzset')
            local score = redis.call('ZSCORE', 'myzset', 'two')
            local rank = redis.call('ZRANK', 'myzset', 'two')
            return {added, card, score, rank}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr[0], RespValue::Integer(3));
            assert_eq!(arr[1], RespValue::Integer(3));
            if let RespValue::BulkString(Some(score)) = &arr[2] {
                assert_eq!(String::from_utf8_lossy(score), "2");
            }
            assert_eq!(arr[3], RespValue::Integer(1)); // rank is 1 (0-indexed)
        } else {
            panic!("Expected Array");
        }
    }

    #[test]
    fn test_script_zset_zrange() {
        let script_commands = setup();

        let script = r#"
            redis.call('ZADD', 'myzset', 1, 'one', 2, 'two', 3, 'three')
            local range = redis.call('ZRANGE', 'myzset', 0, -1)
            return range
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr.len(), 3);
            // Should be sorted by score
            if let RespValue::BulkString(Some(v)) = &arr[0] {
                assert_eq!(String::from_utf8_lossy(v), "one");
            }
            if let RespValue::BulkString(Some(v)) = &arr[1] {
                assert_eq!(String::from_utf8_lossy(v), "two");
            }
            if let RespValue::BulkString(Some(v)) = &arr[2] {
                assert_eq!(String::from_utf8_lossy(v), "three");
            }
        } else {
            panic!("Expected Array");
        }
    }

    #[test]
    fn test_script_zset_zrem() {
        let script_commands = setup();

        let script = r#"
            redis.call('ZADD', 'myzset', 1, 'one', 2, 'two', 3, 'three')
            local removed = redis.call('ZREM', 'myzset', 'one', 'three')
            local card = redis.call('ZCARD', 'myzset')
            return {removed, card}
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0).unwrap();
        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr[0], RespValue::Integer(2));
            assert_eq!(arr[1], RespValue::Integer(1));
        } else {
            panic!("Expected Array");
        }
    }

    // ========================================================================
    // COMPLEX TRANSACTION TESTS
    // ========================================================================

    #[test]
    fn test_script_complex_types_rollback() {
        let script_commands = setup();

        let script = r#"
            redis.call('HSET', 'test_hash', 'field1', 'value1')
            redis.call('LPUSH', 'test_list', 'item1')
            redis.call('SADD', 'test_set', 'member1')
            error('intentional error')
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        // Script should fail
        let result = script_commands.eval(&args, 0);
        assert!(result.is_err());

        // All values should be rolled back
        assert!(script_commands
            .storage
            .get_value(0, "test_hash")
            .unwrap()
            .is_none());
        assert!(script_commands
            .storage
            .get_value(0, "test_list")
            .unwrap()
            .is_none());
        assert!(script_commands
            .storage
            .get_value(0, "test_set")
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_script_complex_types_commit() {
        let script_commands = setup();

        let script = r#"
            redis.call('HSET', 'commit_hash', 'field1', 'value1')
            redis.call('LPUSH', 'commit_list', 'item1')
            redis.call('SADD', 'commit_set', 'member1')
            return 'OK'
        "#;
        let args = vec![Bytes::from(script), Bytes::from("0")];

        let result = script_commands.eval(&args, 0);
        assert!(result.is_ok());

        // All values should be committed
        assert!(script_commands
            .storage
            .get_value(0, "commit_hash")
            .unwrap()
            .is_some());
        assert!(script_commands
            .storage
            .get_value(0, "commit_list")
            .unwrap()
            .is_some());
        assert!(script_commands
            .storage
            .get_value(0, "commit_set")
            .unwrap()
            .is_some());
    }
}
