pub mod json;
pub mod string;

use self::json::JsonCommands;
use self::string::StringCommands;
use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::StorageAdapter;
use bytes::Bytes;

/// Command executor
pub struct CommandExecutor {
    string_commands: StringCommands,
    json_commands: JsonCommands,
}

impl CommandExecutor {
    pub fn new(storage: StorageAdapter) -> Self {
        Self {
            string_commands: StringCommands::new(storage.clone()),
            json_commands: JsonCommands::new(storage),
        }
    }

    pub fn execute(&self, command: &str, args: &[Bytes]) -> Result<RespValue> {
        match command.to_uppercase().as_str() {
            // String commands
            "GET" => self.string_commands.get(args),
            "SET" => self.string_commands.set(args),
            "DEL" => self.string_commands.del(args),
            "EXISTS" => self.string_commands.exists(args),
            "MGET" => self.string_commands.mget(args),
            "MSET" => self.string_commands.mset(args),
            "STRLEN" => self.string_commands.strlen(args),
            "APPEND" => self.string_commands.append(args),

            // JSON commands
            "JSON.GET" => self.json_commands.json_get(args),
            "JSON.SET" => self.json_commands.json_set(args),
            "JSON.DEL" => self.json_commands.json_del(args),
            "JSON.TYPE" => self.json_commands.json_type(args),
            "JSON.STRLEN" => self.json_commands.json_strlen(args),
            "JSON.ARRLEN" => self.json_commands.json_arrlen(args),
            "JSON.OBJLEN" => self.json_commands.json_objlen(args),

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
}
