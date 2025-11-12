pub mod database;
pub mod json;
pub mod key;
pub mod server;
pub mod string;

use self::database::DatabaseCommands;
use self::json::JsonCommands;
use self::key::KeyCommands;
use self::server::ServerCommands;
use self::string::StringCommands;
use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::StorageAdapter;
use bytes::Bytes;

/// Command executor with database context
pub struct CommandExecutor {
    string_commands: StringCommands,
    json_commands: JsonCommands,
    database_commands: DatabaseCommands,
    key_commands: KeyCommands,
    server_commands: ServerCommands,
}

impl CommandExecutor {
    pub fn new(storage: StorageAdapter) -> Self {
        Self {
            string_commands: StringCommands::new(storage.clone()),
            json_commands: JsonCommands::new(storage.clone()),
            database_commands: DatabaseCommands::new(storage.clone()),
            key_commands: KeyCommands::new(storage.clone()),
            server_commands: ServerCommands::new(),
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

            // Utility commands
            "PING" => Ok(RespValue::simple_string("PONG")),
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
