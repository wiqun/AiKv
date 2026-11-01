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

/// 有序集合成员直接进 Lua 表, 不再包一层 `RespValue`.
pub fn member_list_to_lua(lua: &Lua, rows: Vec<(Vec<u8>, f64)>) -> mlua::Result<LuaValue> {
    let table = lua.create_table_with_capacity(rows.len(), 0)?;
    for (i, (member, _)) in rows.into_iter().enumerate() {
        table.raw_set(i + 1, lua.create_string(member)?)?;
    }
    Ok(LuaValue::Table(table))
}

pub fn resp_to_lua(lua: &Lua, value: RespValue) -> mlua::Result<LuaValue> {
    // 注册表标志只读一次: 数组里每个元素都重复读会让 32 条结果的转换多花约 40%.
    let resp3 = matches!(lua.named_registry_value::<i64>("aikv_script_resp"), Ok(3));
    resp_to_lua_with(lua, value, resp3)
}

fn resp_to_lua_with(lua: &Lua, value: RespValue, resp3: bool) -> mlua::Result<LuaValue> {
    match value {
        RespValue::Null => Ok(null_value(resp3)),
        RespValue::SimpleString(s) => Ok(LuaValue::String(lua.create_string(s.as_bytes())?)),
        RespValue::Error(e) => Ok(LuaValue::String(lua.create_string(e.as_bytes())?)),
        RespValue::Integer(i) => Ok(LuaValue::Integer(i)),
        RespValue::BulkString(opt) => match opt {
            Some(b) => Ok(LuaValue::String(lua.create_string(&b)?)),
            None => Ok(null_value(resp3)),
        },
        RespValue::Array(opt) => match opt {
            Some(arr) => {
                let table = lua.create_table_with_capacity(arr.len(), 0)?;
                for (i, item) in arr.into_iter().enumerate() {
                    table.raw_set(i + 1, resp_to_lua_with(lua, item, resp3)?)?;
                }
                Ok(LuaValue::Table(table))
            }
            None => Ok(null_value(resp3)),
        },
        _ => Ok(LuaValue::Nil),
    }
}

fn null_value(resp3: bool) -> LuaValue {
    if resp3 {
        LuaValue::Nil
    } else {
        LuaValue::Boolean(false)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> Lua {
        Lua::new()
    }

    fn sample() -> RespValue {
        RespValue::Array(Some(vec![
            RespValue::BulkString(Some(Bytes::from_static(b"a"))),
            RespValue::BulkString(None),
            RespValue::Integer(7),
            RespValue::Array(Some(vec![
                RespValue::BulkString(Some(Bytes::from_static(b"nested"))),
                RespValue::Null,
            ])),
            RespValue::Array(None),
        ]))
    }

    fn describe(v: &LuaValue) -> String {
        match v {
            LuaValue::Nil => "nil".into(),
            LuaValue::Boolean(b) => format!("bool:{b}"),
            LuaValue::Integer(i) => format!("int:{i}"),
            LuaValue::String(s) => format!("str:{}", s.to_str().unwrap()),
            LuaValue::Table(t) => {
                let len = t.raw_len();
                let mut parts = Vec::new();
                for i in 1..=len {
                    parts.push(describe(&t.raw_get::<LuaValue>(i).unwrap()));
                }
                format!("[{}]", parts.join(","))
            }
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn resp2_nulls_become_false_at_every_depth() {
        let lua = vm();
        let got = resp_to_lua(&lua, sample()).unwrap();
        assert_eq!(
            describe(&got),
            "[str:a,bool:false,int:7,[str:nested,bool:false],bool:false]"
        );
    }

    #[test]
    fn resp3_nulls_become_nil_at_every_depth() {
        let lua = vm();
        lua.set_named_registry_value("aikv_script_resp", 3i64)
            .unwrap();
        let got = resp_to_lua(&lua, sample()).unwrap();
        // nil 在表里表现为空洞, `raw_len` 在第一个 nil 处截断: 只检查第一个元素与长度语义.
        let LuaValue::Table(t) = got else {
            panic!("expected table")
        };
        assert_eq!(describe(&t.raw_get::<LuaValue>(1).unwrap()), "str:a");
        assert!(matches!(t.raw_get::<LuaValue>(2).unwrap(), LuaValue::Nil));
        assert_eq!(describe(&t.raw_get::<LuaValue>(3).unwrap()), "int:7");
        let LuaValue::Table(inner) = t.raw_get::<LuaValue>(4).unwrap() else {
            panic!("nested")
        };
        assert_eq!(
            describe(&inner.raw_get::<LuaValue>(1).unwrap()),
            "str:nested"
        );
        assert!(matches!(
            inner.raw_get::<LuaValue>(2).unwrap(),
            LuaValue::Nil
        ));
        assert!(matches!(t.raw_get::<LuaValue>(5).unwrap(), LuaValue::Nil));
    }
}
