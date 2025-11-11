use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::StorageAdapter;
use bytes::Bytes;
use serde_json::{json, Value as JsonValue};

/// JSON command handler
pub struct JsonCommands {
    storage: StorageAdapter,
}

impl JsonCommands {
    pub fn new(storage: StorageAdapter) -> Self {
        Self {
            storage,
        }
    }

    /// JSON.GET key [path]
    pub fn json_get(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("JSON.GET".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let path = if args.len() > 1 {
            String::from_utf8_lossy(&args[1]).to_string()
        } else {
            "$".to_string()
        };

        match self.storage.get(&key)? {
            Some(value) => {
                let json: JsonValue = serde_json::from_slice(&value)?;

                let result = if path == "$" || path == "." {
                    json
                } else {
                    // Simple path extraction (full JSONPath would be more complex)
                    self.extract_json_path(&json, &path)?
                };

                let json_string = serde_json::to_string(&result)?;
                Ok(RespValue::bulk_string(json_string))
            },
            None => Ok(RespValue::null_bulk_string()),
        }
    }

    /// JSON.SET key path value [NX|XX]
    pub fn json_set(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() < 3 {
            return Err(AikvError::WrongArgCount("JSON.SET".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let path = String::from_utf8_lossy(&args[1]).to_string();
        let value_str = String::from_utf8_lossy(&args[2]).to_string();

        // Parse options
        let mut nx = false;
        let mut xx = false;
        for i in 3..args.len() {
            let option = String::from_utf8_lossy(&args[i]).to_uppercase();
            match option.as_str() {
                "NX" => nx = true,
                "XX" => xx = true,
                _ => {},
            }
        }

        // Check conditions
        let exists = self.storage.exists(&key)?;
        if nx && exists {
            return Ok(RespValue::null_bulk_string());
        }
        if xx && !exists {
            return Ok(RespValue::null_bulk_string());
        }

        // Parse the new value
        let new_value: JsonValue = serde_json::from_str(&value_str)?;

        let result_json = if path == "$" || path == "." {
            // Root path - replace entire value
            new_value
        } else {
            // Get existing value or create empty object
            let mut json = match self.storage.get(&key)? {
                Some(existing) => serde_json::from_slice(&existing)?,
                None => json!({}),
            };

            // Set value at path (simplified)
            self.set_json_path(&mut json, &path, new_value)?;
            json
        };

        let json_bytes = Bytes::from(serde_json::to_vec(&result_json)?);
        self.storage.set(key, json_bytes)?;

        Ok(RespValue::ok())
    }

    /// JSON.DEL key [path]
    pub fn json_del(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("JSON.DEL".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let path = if args.len() > 1 {
            String::from_utf8_lossy(&args[1]).to_string()
        } else {
            "$".to_string()
        };

        if path == "$" || path == "." {
            // Delete entire key
            if self.storage.delete(&key)? {
                Ok(RespValue::integer(1))
            } else {
                Ok(RespValue::integer(0))
            }
        } else {
            // Delete specific path
            match self.storage.get(&key)? {
                Some(value) => {
                    let mut json: JsonValue = serde_json::from_slice(&value)?;

                    if self.delete_json_path(&mut json, &path)? {
                        let json_bytes = Bytes::from(serde_json::to_vec(&json)?);
                        self.storage.set(key, json_bytes)?;
                        Ok(RespValue::integer(1))
                    } else {
                        Ok(RespValue::integer(0))
                    }
                },
                None => Ok(RespValue::integer(0)),
            }
        }
    }

    /// JSON.TYPE key [path]
    pub fn json_type(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("JSON.TYPE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let path = if args.len() > 1 {
            String::from_utf8_lossy(&args[1]).to_string()
        } else {
            "$".to_string()
        };

        match self.storage.get(&key)? {
            Some(value) => {
                let json: JsonValue = serde_json::from_slice(&value)?;

                let target = if path == "$" || path == "." {
                    &json
                } else {
                    &self.extract_json_path(&json, &path)?
                };

                let type_name = match target {
                    JsonValue::Null => "null",
                    JsonValue::Bool(_) => "boolean",
                    JsonValue::Number(_) => "number",
                    JsonValue::String(_) => "string",
                    JsonValue::Array(_) => "array",
                    JsonValue::Object(_) => "object",
                };

                Ok(RespValue::simple_string(type_name))
            },
            None => Ok(RespValue::null_bulk_string()),
        }
    }

    /// JSON.STRLEN key [path]
    pub fn json_strlen(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("JSON.STRLEN".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let path = if args.len() > 1 {
            String::from_utf8_lossy(&args[1]).to_string()
        } else {
            "$".to_string()
        };

        match self.storage.get(&key)? {
            Some(value) => {
                let json: JsonValue = serde_json::from_slice(&value)?;

                let target = if path == "$" || path == "." {
                    &json
                } else {
                    &self.extract_json_path(&json, &path)?
                };

                if let JsonValue::String(s) = target {
                    Ok(RespValue::integer(s.len() as i64))
                } else {
                    Ok(RespValue::null_bulk_string())
                }
            },
            None => Ok(RespValue::null_bulk_string()),
        }
    }

    /// JSON.ARRLEN key [path]
    pub fn json_arrlen(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("JSON.ARRLEN".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let path = if args.len() > 1 {
            String::from_utf8_lossy(&args[1]).to_string()
        } else {
            "$".to_string()
        };

        match self.storage.get(&key)? {
            Some(value) => {
                let json: JsonValue = serde_json::from_slice(&value)?;

                let target = if path == "$" || path == "." {
                    &json
                } else {
                    &self.extract_json_path(&json, &path)?
                };

                if let JsonValue::Array(arr) = target {
                    Ok(RespValue::integer(arr.len() as i64))
                } else {
                    Ok(RespValue::null_bulk_string())
                }
            },
            None => Ok(RespValue::null_bulk_string()),
        }
    }

    /// JSON.OBJLEN key [path]
    pub fn json_objlen(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("JSON.OBJLEN".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let path = if args.len() > 1 {
            String::from_utf8_lossy(&args[1]).to_string()
        } else {
            "$".to_string()
        };

        match self.storage.get(&key)? {
            Some(value) => {
                let json: JsonValue = serde_json::from_slice(&value)?;

                let target = if path == "$" || path == "." {
                    &json
                } else {
                    &self.extract_json_path(&json, &path)?
                };

                if let JsonValue::Object(obj) = target {
                    Ok(RespValue::integer(obj.len() as i64))
                } else {
                    Ok(RespValue::null_bulk_string())
                }
            },
            None => Ok(RespValue::null_bulk_string()),
        }
    }

    // Helper methods for path operations (simplified JSONPath)

    fn extract_json_path(&self, json: &JsonValue, path: &str) -> Result<JsonValue> {
        // Remove leading $ or .
        let path = path.trim_start_matches('$').trim_start_matches('.');

        if path.is_empty() {
            return Ok(json.clone());
        }

        // Simple path like "name" or "user.name"
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = json;

        for part in parts {
            if let JsonValue::Object(obj) = current {
                current = obj.get(part).ok_or_else(|| {
                    AikvError::InvalidArgument(format!("Path not found: {}", part))
                })?;
            } else {
                return Err(AikvError::InvalidArgument(format!(
                    "Cannot traverse non-object at: {}",
                    part
                )));
            }
        }

        Ok(current.clone())
    }

    fn set_json_path(&self, json: &mut JsonValue, path: &str, value: JsonValue) -> Result<()> {
        // Remove leading $ or .
        let path = path.trim_start_matches('$').trim_start_matches('.');

        if path.is_empty() {
            *json = value;
            return Ok(());
        }

        // Simple path like "name" or "user.name"
        let parts: Vec<&str> = path.split('.').collect();

        if !json.is_object() {
            *json = json!({}); // Convert to object if not already
        }

        let mut current = json;

        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                // Last part - set the value
                if let JsonValue::Object(obj) = current {
                    obj.insert(part.to_string(), value);
                    break; // Exit after inserting
                }
            } else {
                // Intermediate part - ensure object exists
                if let JsonValue::Object(obj) = current {
                    current = obj.entry(part.to_string()).or_insert_with(|| json!({}));
                }
            }
        }

        Ok(())
    }

    fn delete_json_path(&self, json: &mut JsonValue, path: &str) -> Result<bool> {
        // Remove leading $ or .
        let path = path.trim_start_matches('$').trim_start_matches('.');

        if path.is_empty() {
            return Ok(false);
        }

        // Simple path like "name" or "user.name"
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = json;

        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                // Last part - delete the key
                if let JsonValue::Object(obj) = current {
                    return Ok(obj.remove(*part).is_some());
                }
                return Ok(false);
            } else {
                // Intermediate part
                if let JsonValue::Object(obj) = current {
                    if let Some(next) = obj.get_mut(*part) {
                        current = next;
                    } else {
                        return Ok(false);
                    }
                } else {
                    return Ok(false);
                }
            }
        }

        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> JsonCommands {
        JsonCommands::new(StorageAdapter::new())
    }

    #[test]
    fn test_json_set_get() {
        let cmd = setup();

        let json_str = r#"{"name":"John","age":30}"#;
        cmd.json_set(&[Bytes::from("user"), Bytes::from("$"), Bytes::from(json_str)])
            .unwrap();

        let result = cmd.json_get(&[Bytes::from("user")]).unwrap();
        if let RespValue::BulkString(Some(data)) = result {
            let json: JsonValue = serde_json::from_slice(&data).unwrap();
            assert_eq!(json["name"], "John");
            assert_eq!(json["age"], 30);
        } else {
            panic!("Expected bulk string");
        }
    }

    #[test]
    fn test_json_type() {
        let cmd = setup();

        cmd.json_set(&[
            Bytes::from("user"),
            Bytes::from("$"),
            Bytes::from(r#"{"name":"John","age":30,"active":true}"#),
        ])
        .unwrap();

        let result = cmd
            .json_type(&[Bytes::from("user"), Bytes::from("$.name")])
            .unwrap();
        assert_eq!(result, RespValue::simple_string("string"));

        let result = cmd
            .json_type(&[Bytes::from("user"), Bytes::from("$.age")])
            .unwrap();
        assert_eq!(result, RespValue::simple_string("number"));
    }

    #[test]
    fn test_json_arrlen() {
        let cmd = setup();

        cmd.json_set(&[
            Bytes::from("arr"),
            Bytes::from("$"),
            Bytes::from("[1,2,3,4,5]"),
        ])
        .unwrap();

        let result = cmd.json_arrlen(&[Bytes::from("arr")]).unwrap();
        assert_eq!(result, RespValue::integer(5));
    }

    #[test]
    fn test_json_objlen() {
        let cmd = setup();

        cmd.json_set(&[
            Bytes::from("user"),
            Bytes::from("$"),
            Bytes::from(r#"{"name":"John","age":30}"#),
        ])
        .unwrap();

        let result = cmd.json_objlen(&[Bytes::from("user")]).unwrap();
        assert_eq!(result, RespValue::integer(2));
    }
}
