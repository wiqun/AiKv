//! Lua ↔ RESP 转换

use bytes::Bytes;
use mlua::{Lua, Value as LuaValue};

use crate::error::{Error, Result};
use crate::protocol::RespValue;

pub fn lua_to_resp(value: LuaValue) -> Result<RespValue> {
    match value {
        LuaValue::Nil => Ok(RespValue::Null),
        LuaValue::Boolean(b) => {
            if b {
                Ok(RespValue::Integer(1))
            } else {
                Ok(RespValue::BulkString(None))
            }
        }
        LuaValue::Integer(i) => Ok(RespValue::Integer(i)),
        LuaValue::Number(n) => {
            if !n.is_finite() {
                return Ok(RespValue::Null);
            }
            if n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 {
                Ok(RespValue::Integer(n as i64))
            } else {
                Ok(RespValue::BulkString(Some(Bytes::from(n.to_string()))))
            }
        }
        LuaValue::String(s) => Ok(RespValue::BulkString(Some(Bytes::from(
            s.as_bytes().to_vec(),
        )))),
        LuaValue::Table(t) => {
            let len = t.len().unwrap_or(0);
            let mut items = Vec::with_capacity(len as usize);
            for i in 1..=len {
                let val = t.get::<LuaValue>(i).map_err(lua_err)?;
                items.push(lua_to_resp(val)?);
            }
            Ok(RespValue::Array(Some(items)))
        }
        _ => Ok(RespValue::Null),
    }
}

pub fn resp_to_lua(lua: &Lua, value: RespValue) -> mlua::Result<LuaValue> {
    let resp3 = matches!(lua.named_registry_value::<i64>("aikv_script_resp"), Ok(3));
    match value {
        RespValue::Null => {
            if resp3 {
                Ok(LuaValue::Nil)
            } else {
                Ok(LuaValue::Boolean(false))
            }
        }
        RespValue::SimpleString(s) => Ok(LuaValue::String(lua.create_string(s.as_bytes())?)),
        RespValue::Error(e) => Ok(LuaValue::String(lua.create_string(e.as_bytes())?)),
        RespValue::Integer(i) => Ok(LuaValue::Integer(i)),
        RespValue::BulkString(opt) => match opt {
            Some(b) => Ok(LuaValue::String(lua.create_string(&b)?)),
            None => {
                if resp3 {
                    Ok(LuaValue::Nil)
                } else {
                    Ok(LuaValue::Boolean(false))
                }
            }
        },
        RespValue::Array(opt) => match opt {
            Some(arr) => {
                let table = lua.create_table()?;
                for (i, item) in arr.into_iter().enumerate() {
                    table.set(i + 1, resp_to_lua(lua, item)?)?;
                }
                Ok(LuaValue::Table(table))
            }
            None => {
                if resp3 {
                    Ok(LuaValue::Nil)
                } else {
                    Ok(LuaValue::Boolean(false))
                }
            }
        },
        _ => Ok(LuaValue::Nil),
    }
}

pub fn pcall_error_table(lua: &Lua, msg: &str) -> mlua::Result<LuaValue> {
    let table = lua.create_table()?;
    table.set("err", msg)?;
    Ok(LuaValue::Table(table))
}

fn lua_err(e: mlua::Error) -> Error {
    Error::Command(format!("ERR script error: {e}"))
}
