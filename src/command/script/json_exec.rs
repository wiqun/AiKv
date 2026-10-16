//! Lua 脚本内 JSON 命令子集的执行: 由 `execute::dispatch` 按命令名转发, 支持 10 条
//! JSON.* 命令 (GET/MGET/SET/DEL/TYPE/STRLEN/ARRLEN/OBJLEN/NUMINCRBY/ARRAPPEND).
//!
//! JSON 文档以 `serde_json::Value` 序列化 bytes 存于 String KV (与 `json.rs` 一致).
//! 读写均经 `ScriptTransaction`: 读用 `txn.get` (写缓冲优先), 写用 `txn.set_string` 缓冲
//! 待脚本结束后统一 commit.
//!
//! 与 `json.rs` 的差异: 精简子集, 参数解析简化 — JSON.SET 支持 NX/XX, 无 XE/TTL;
//! 其余命令不支持选项; 路径缺省 `$`.

use bytes::Bytes;
use serde_json::Value as JsonValue;

use crate::command::jsonpath::JsonPathEngine;
use crate::command::router;
use crate::command::script::transaction::ScriptTransaction;
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::ScriptIo;

fn invalid_json(e: serde_json::Error) -> Error {
    Error::Command(format!("ERR invalid JSON: {e}"))
}

fn json_type_name(v: &JsonValue) -> &'static str {
    match v {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

pub fn exec_json_get(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(router::wrong_args("JSON.GET", ""));
    }
    let path = args
        .get(1)
        .map(|b| String::from_utf8_lossy(b).to_string())
        .unwrap_or_else(|| "$".to_string());

    let raw = match txn.get(storage, &args[0])? {
        Some(v) => v,
        None => return Ok(router::nil_bulk()),
    };

    let json_val: JsonValue = serde_json::from_slice(&raw).map_err(invalid_json)?;
    let extracted = if path == "$" || path == "." {
        json_val
    } else {
        let engine = JsonPathEngine;
        engine.extract(&json_val, &path)?
    };

    let json_string = serde_json::to_string(&extracted).map_err(invalid_json)?;
    Ok(router::bulk(json_string.into_bytes()))
}

pub fn exec_json_mget(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(router::wrong_args("JSON.MGET", ""));
    }
    let path = String::from_utf8_lossy(&args[args.len() - 1]).to_string();
    let keys = &args[..args.len() - 1];

    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        let raw = match txn.get(storage, key)? {
            Some(v) => v,
            None => {
                out.push(RespValue::Null);
                continue;
            }
        };
        let json_val: JsonValue = serde_json::from_slice(&raw).map_err(invalid_json)?;
        let extracted = if path == "$" || path == "." {
            json_val
        } else {
            let engine = JsonPathEngine;
            match engine.extract(&json_val, &path) {
                Ok(v) => v,
                Err(_) => {
                    out.push(RespValue::Null);
                    continue;
                }
            }
        };
        let json_string = serde_json::to_string(&extracted).map_err(invalid_json)?;
        out.push(router::bulk(json_string.into_bytes()));
    }
    Ok(RespValue::Array(Some(out)))
}

pub fn exec_json_set(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 {
        return Err(router::wrong_args("JSON.SET", ""));
    }
    let key = args[0].to_vec();
    let path = String::from_utf8_lossy(&args[1]).to_string();
    let value_str = String::from_utf8_lossy(&args[2]).to_string();

    let mut nx = false;
    let mut xx = false;
    for arg in args.iter().skip(3) {
        match String::from_utf8_lossy(arg).to_ascii_uppercase().as_str() {
            "NX" => nx = true,
            "XX" => xx = true,
            _ => {}
        }
    }

    if nx && xx {
        return Err(Error::Command(
            "ERR NX and XX are mutually exclusive".into(),
        ));
    }

    let exists = txn.exists(storage, &args[0])?;
    if nx && exists {
        return Ok(router::nil_bulk());
    }
    if xx && !exists {
        return Ok(router::nil_bulk());
    }

    let new_value: JsonValue = serde_json::from_str(&value_str).map_err(invalid_json)?;

    let result_json = if path == "$" || path == "." {
        new_value
    } else {
        let mut json_doc: JsonValue = match txn.get(storage, &args[0])? {
            Some(raw) => serde_json::from_slice(&raw).map_err(invalid_json)?,
            None => serde_json::json!({}),
        };
        let engine = JsonPathEngine;
        engine.set(&mut json_doc, &path, new_value)?;
        json_doc
    };

    let json_bytes = serde_json::to_vec(&result_json).map_err(invalid_json)?;
    txn.set_string(key, json_bytes);
    Ok(router::ok())
}

pub fn exec_json_del(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(router::wrong_args("JSON.DEL", ""));
    }
    let key = &args[0];
    let path = args
        .get(1)
        .map(|b| String::from_utf8_lossy(b).to_string())
        .unwrap_or_else(|| "$".to_string());

    if path == "$" || path == "." {
        let exists = txn.exists(storage, key)?;
        if exists {
            txn.delete(key.to_vec());
            Ok(router::integer(1))
        } else {
            Ok(router::integer(0))
        }
    } else {
        let raw = match txn.get(storage, key)? {
            Some(v) => v,
            None => return Ok(router::integer(0)),
        };
        let mut json_val: JsonValue = serde_json::from_slice(&raw).map_err(invalid_json)?;
        let engine = JsonPathEngine;
        let count = engine.delete(&mut json_val, &path)?;
        if count > 0 {
            let json_bytes = serde_json::to_vec(&json_val).map_err(invalid_json)?;
            txn.set_string(key.to_vec(), json_bytes);
        }
        Ok(router::integer(count as i64))
    }
}

pub fn exec_json_type(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(router::wrong_args("JSON.TYPE", ""));
    }
    let path = args
        .get(1)
        .map(|b| String::from_utf8_lossy(b).to_string())
        .unwrap_or_else(|| "$".to_string());

    let raw = match txn.get(storage, &args[0])? {
        Some(v) => v,
        None => return Ok(router::nil_bulk()),
    };

    let json_val: JsonValue = serde_json::from_slice(&raw).map_err(invalid_json)?;
    let target = if path == "$" || path == "." {
        json_val
    } else {
        let engine = JsonPathEngine;
        match engine.extract(&json_val, &path) {
            Ok(v) => v,
            Err(_) => return Ok(router::nil_bulk()),
        }
    };
    let type_name = json_type_name(&target);
    Ok(RespValue::SimpleString(type_name.into()))
}

pub fn exec_json_strlen(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(router::wrong_args("JSON.STRLEN", ""));
    }
    let path = args
        .get(1)
        .map(|b| String::from_utf8_lossy(b).to_string())
        .unwrap_or_else(|| "$".to_string());

    let raw = match txn.get(storage, &args[0])? {
        Some(v) => v,
        None => return Ok(router::nil_bulk()),
    };

    let json_val: JsonValue = serde_json::from_slice(&raw).map_err(invalid_json)?;

    if path == "$" || path == "." {
        return match &json_val {
            JsonValue::String(s) => Ok(router::integer(s.len() as i64)),
            _ => Ok(router::nil_bulk()),
        };
    }
    let engine = JsonPathEngine;
    match engine.extract(&json_val, &path) {
        Ok(v) => match &v {
            JsonValue::String(s) => Ok(router::integer(s.len() as i64)),
            _ => Ok(router::nil_bulk()),
        },
        Err(_) => Ok(router::nil_bulk()),
    }
}

pub fn exec_json_arrlen(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(router::wrong_args("JSON.ARRLEN", ""));
    }
    let path = args
        .get(1)
        .map(|b| String::from_utf8_lossy(b).to_string())
        .unwrap_or_else(|| "$".to_string());

    let raw = match txn.get(storage, &args[0])? {
        Some(v) => v,
        None => return Ok(router::nil_bulk()),
    };

    let json_val: JsonValue = serde_json::from_slice(&raw).map_err(invalid_json)?;
    let target = if path == "$" || path == "." {
        json_val
    } else {
        let engine = JsonPathEngine;
        match engine.extract(&json_val, &path) {
            Ok(v) => v,
            Err(_) => return Ok(router::nil_bulk()),
        }
    };
    match &target {
        JsonValue::Array(arr) => Ok(router::integer(arr.len() as i64)),
        _ => Ok(router::nil_bulk()),
    }
}

pub fn exec_json_objlen(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(router::wrong_args("JSON.OBJLEN", ""));
    }
    let path = args
        .get(1)
        .map(|b| String::from_utf8_lossy(b).to_string())
        .unwrap_or_else(|| "$".to_string());

    let raw = match txn.get(storage, &args[0])? {
        Some(v) => v,
        None => return Ok(router::nil_bulk()),
    };

    let json_val: JsonValue = serde_json::from_slice(&raw).map_err(invalid_json)?;
    let target = if path == "$" || path == "." {
        json_val
    } else {
        let engine = JsonPathEngine;
        match engine.extract(&json_val, &path) {
            Ok(v) => v,
            Err(_) => return Ok(router::nil_bulk()),
        }
    };
    match &target {
        JsonValue::Object(obj) => Ok(router::integer(obj.len() as i64)),
        _ => Ok(router::nil_bulk()),
    }
}

pub fn exec_json_numincrby(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(router::wrong_args("JSON.NUMINCRBY", ""));
    }
    let key = &args[0];
    let path = String::from_utf8_lossy(&args[1]).to_string();
    let incr_str = String::from_utf8_lossy(&args[2]).to_string();

    let incr: f64 = incr_str
        .parse()
        .map_err(|_| Error::Command(format!("ERR Invalid increment: {incr_str}")))?;

    if path == "$" || path == "." {
        return Err(Error::Command("ERR Cannot increment root".into()));
    }

    let raw = match txn.get(storage, key)? {
        Some(v) => v,
        None => return Ok(router::nil_bulk()),
    };

    let mut json_val: JsonValue = serde_json::from_slice(&raw).map_err(invalid_json)?;

    let path_normalized = path
        .trim_start_matches('$')
        .trim_start_matches('.')
        .to_string();
    let parts = JsonPathEngine::split_path_parts(&path_normalized);
    let engine = JsonPathEngine;
    engine.incr(&mut json_val, &parts, incr)?;

    let result = engine.extract(&json_val, &path)?;
    if !result.as_f64().map(|n| n.is_finite()).unwrap_or(true) {
        return Err(Error::Command("ERR result is not a finite number".into()));
    }

    let json_bytes = serde_json::to_vec(&json_val).map_err(invalid_json)?;
    txn.set_string(key.to_vec(), json_bytes);
    let json_string = serde_json::to_string(&result).map_err(invalid_json)?;
    Ok(router::bulk(json_string.into_bytes()))
}

pub fn exec_json_arrappend(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 {
        return Err(router::wrong_args("JSON.ARRAPPEND", ""));
    }
    let key = &args[0];
    let path = String::from_utf8_lossy(&args[1]).to_string();

    if path == "$" || path == "." {
        return Err(Error::Command("ERR Cannot append to root".into()));
    }

    let mut values = Vec::new();
    for arg in args.iter().skip(2) {
        let val_str = String::from_utf8_lossy(arg);
        let val: JsonValue = serde_json::from_str(&val_str).map_err(invalid_json)?;
        values.push(val);
    }

    let raw = match txn.get(storage, key)? {
        Some(v) => v,
        None => return Ok(router::nil_bulk()),
    };

    let mut json_val: JsonValue = serde_json::from_slice(&raw).map_err(invalid_json)?;

    let path_normalized = path
        .trim_start_matches('$')
        .trim_start_matches('.')
        .to_string();
    let parts = JsonPathEngine::split_path_parts(&path_normalized);
    let engine = JsonPathEngine;
    engine.append(&mut json_val, &parts, &values)?;

    let extracted = engine.extract(&json_val, &path)?;
    let len = match &extracted {
        JsonValue::Array(arr) => arr.len() as i64,
        _ => return Ok(router::nil_bulk()),
    };

    let json_bytes = serde_json::to_vec(&json_val).map_err(invalid_json)?;
    txn.set_string(key.to_vec(), json_bytes);
    Ok(router::integer(len))
}
