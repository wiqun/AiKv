use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::{BatchOp, StorageEngine};
use bytes::Bytes;
use mlua::{Lua, LuaOptions, StdLib, Value as LuaValue};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

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
#[derive(Debug)]
struct ScriptTransaction {
    /// Database index for this transaction
    db_index: usize,
    /// Write buffer: key -> operation
    write_buffer: HashMap<String, BatchOp>,
}

impl ScriptTransaction {
    /// Create a new transaction context for a database
    fn new(db_index: usize) -> Self {
        Self {
            db_index,
            write_buffer: HashMap::new(),
        }
    }

    /// Read a value, checking buffer first, then storage
    ///
    /// This implements "read your own writes" semantics - if a key was set
    /// or deleted in this transaction, return that state.
    fn get(&self, storage: &StorageEngine, key: &str) -> Result<Option<Bytes>> {
        // Check write buffer first
        if let Some(op) = self.write_buffer.get(key) {
            match op {
                BatchOp::Set(value) => return Ok(Some(value.clone())),
                BatchOp::Delete => return Ok(None),
            }
        }

        // Fall back to storage
        storage.get_from_db(self.db_index, key)
    }

    /// Write a value to the buffer
    fn set(&mut self, key: String, value: Bytes) {
        self.write_buffer.insert(key, BatchOp::Set(value));
    }

    /// Mark a key for deletion in the buffer
    fn delete(&mut self, key: String) {
        self.write_buffer.insert(key, BatchOp::Delete);
    }

    /// Check if a key exists, considering the buffer
    fn exists(&self, storage: &StorageEngine, key: &str) -> Result<bool> {
        // Check write buffer first
        if let Some(op) = self.write_buffer.get(key) {
            match op {
                BatchOp::Set(_) => return Ok(true),
                BatchOp::Delete => return Ok(false),
            }
        }

        // Fall back to storage
        storage.exists_in_db(self.db_index, key)
    }

    /// Commit the transaction - apply all buffered operations to storage atomically
    ///
    /// This method uses write_batch() which provides:
    /// - For MemoryAdapter: In-memory atomicity within a single lock
    /// - For AiDbStorageEngine: True atomic batch writes via AiDb's WriteBatch
    ///   with WAL durability guarantees (all operations written to WAL first,
    ///   single fsync, atomic recovery on crash)
    fn commit(self, storage: &StorageEngine) -> Result<()> {
        if self.write_buffer.is_empty() {
            return Ok(());
        }

        // Convert HashMap to Vec for write_batch
        let operations: Vec<(String, BatchOp)> = self.write_buffer.into_iter().collect();

        // Use write_batch for atomic commit
        storage.write_batch(self.db_index, operations)?;

        Ok(())
    }

    // Note: rollback() is implicit - just drop the transaction without calling commit()
}

/// Script command handler
pub struct ScriptCommands {
    storage: StorageEngine,
    script_cache: Arc<RwLock<HashMap<String, CachedScript>>>,
}

impl ScriptCommands {
    pub fn new(storage: StorageEngine) -> Self {
        Self {
            storage,
            script_cache: Arc::new(RwLock::new(HashMap::new())),
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
    fn execute_script(
        &self,
        script: &str,
        keys: &[String],
        argv: &[String],
        db_index: usize,
    ) -> Result<RespValue> {
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

        // Execute simple string commands
        let result = match command.as_str() {
            "GET" => Self::execute_get(storage, transaction, command_args),
            "SET" => Self::execute_set(storage, transaction, command_args),
            "DEL" => Self::execute_del(storage, transaction, command_args),
            "EXISTS" => Self::execute_exists(storage, transaction, command_args),
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
}
