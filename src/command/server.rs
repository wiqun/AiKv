use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Client info structure
#[derive(Clone, Debug)]
pub struct ClientInfo {
    pub id: usize,
    pub name: Option<String>,
    pub addr: String,
}

/// Server command handler
pub struct ServerCommands {
    clients: Arc<RwLock<HashMap<usize, ClientInfo>>>,
    config: Arc<RwLock<HashMap<String, String>>>,
}

impl ServerCommands {
    pub fn new() -> Self {
        let mut default_config = HashMap::new();
        default_config.insert("server".to_string(), "aikv".to_string());
        default_config.insert("version".to_string(), "0.1.0".to_string());
        default_config.insert("port".to_string(), "6379".to_string());
        default_config.insert("databases".to_string(), "16".to_string());

        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(RwLock::new(default_config)),
        }
    }

    /// INFO \[section\] - Get server information
    pub fn info(&self, args: &[Bytes]) -> Result<RespValue> {
        let section = if args.is_empty() {
            "default"
        } else {
            &String::from_utf8_lossy(&args[0])
        };

        let mut info_lines = Vec::new();

        match section.to_lowercase().as_str() {
            "server" | "default" => {
                info_lines.push("# Server".to_string());
                info_lines.push("redis_version:0.1.0".to_string());
                info_lines.push("redis_mode:standalone".to_string());
                info_lines.push("os:Linux".to_string());
                info_lines.push("arch_bits:64".to_string());
                info_lines.push("process_id:1".to_string());
            }
            "clients" => {
                info_lines.push("# Clients".to_string());
                let clients = self
                    .clients
                    .read()
                    .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;
                info_lines.push(format!("connected_clients:{}", clients.len()));
            }
            "memory" => {
                info_lines.push("# Memory".to_string());
                info_lines.push("used_memory:1024000".to_string());
                info_lines.push("used_memory_human:1.00M".to_string());
            }
            "stats" => {
                info_lines.push("# Stats".to_string());
                info_lines.push("total_connections_received:0".to_string());
                info_lines.push("total_commands_processed:0".to_string());
            }
            _ => {
                return Err(AikvError::InvalidArgument(format!(
                    "ERR unknown section '{}'",
                    section
                )));
            }
        }

        let info_str = info_lines.join("\r\n");
        Ok(RespValue::bulk_string(info_str))
    }

    /// CONFIG GET parameter - Get configuration value
    pub fn config_get(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("CONFIG GET".to_string()));
        }

        let parameter = String::from_utf8_lossy(&args[0]).to_string();
        let config = self
            .config
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut results = Vec::new();

        // Support wildcard matching
        if parameter == "*" {
            for (key, value) in config.iter() {
                results.push(RespValue::bulk_string(key.clone()));
                results.push(RespValue::bulk_string(value.clone()));
            }
        } else if let Some(value) = config.get(&parameter) {
            results.push(RespValue::bulk_string(parameter.clone()));
            results.push(RespValue::bulk_string(value.clone()));
        }

        Ok(RespValue::array(results))
    }

    /// CONFIG SET parameter value - Set configuration value
    pub fn config_set(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("CONFIG SET".to_string()));
        }

        let parameter = String::from_utf8_lossy(&args[0]).to_string();
        let value = String::from_utf8_lossy(&args[1]).to_string();

        let mut config = self
            .config
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        // For safety, only allow certain parameters to be set
        match parameter.as_str() {
            "server" | "version" | "port" => {
                return Err(AikvError::InvalidArgument(
                    "ERR configuration parameter is read-only".to_string(),
                ));
            }
            _ => {}
        }

        config.insert(parameter, value);
        Ok(RespValue::ok())
    }

    /// TIME - Return the current server time
    pub fn time(&self, _args: &[Bytes]) -> Result<RespValue> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| AikvError::Storage(format!("Time error: {}", e)))?;

        let seconds = now.as_secs();
        let microseconds = now.subsec_micros();

        Ok(RespValue::array(vec![
            RespValue::bulk_string(seconds.to_string()),
            RespValue::bulk_string(microseconds.to_string()),
        ]))
    }

    /// CLIENT LIST - List all client connections
    pub fn client_list(&self, _args: &[Bytes]) -> Result<RespValue> {
        let clients = self
            .clients
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut client_lines = Vec::new();
        for (id, client) in clients.iter() {
            let name = client
                .name
                .as_ref()
                .map(|n| format!(" name={}", n))
                .unwrap_or_default();
            client_lines.push(format!("id={} addr={}{}", id, client.addr, name));
        }

        let client_str = client_lines.join("\n");
        Ok(RespValue::bulk_string(client_str))
    }

    /// CLIENT SETNAME name - Set client name
    pub fn client_setname(&self, args: &[Bytes], client_id: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("CLIENT SETNAME".to_string()));
        }

        let name = String::from_utf8_lossy(&args[0]).to_string();

        let mut clients = self
            .clients
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(client) = clients.get_mut(&client_id) {
            client.name = Some(name);
        }

        Ok(RespValue::ok())
    }

    /// CLIENT GETNAME - Get client name
    pub fn client_getname(&self, _args: &[Bytes], client_id: usize) -> Result<RespValue> {
        let clients = self
            .clients
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(client) = clients.get(&client_id) {
            if let Some(name) = &client.name {
                return Ok(RespValue::bulk_string(name.clone()));
            }
        }

        Ok(RespValue::null_bulk_string())
    }

    /// Register a client
    pub fn register_client(&self, id: usize, addr: String) -> Result<()> {
        let mut clients = self
            .clients
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        clients.insert(
            id,
            ClientInfo {
                id,
                name: None,
                addr,
            },
        );
        Ok(())
    }

    /// Unregister a client
    pub fn unregister_client(&self, id: usize) -> Result<()> {
        let mut clients = self
            .clients
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        clients.remove(&id);
        Ok(())
    }
}

impl Default for ServerCommands {
    fn default() -> Self {
        Self::new()
    }
}
