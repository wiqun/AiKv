//! Redis 脚本 `cmsgpack.pack` / `cmsgpack.unpack`.
//!
//! 连续对象拼在同一段字节里. `unpack` 把这段里的每个对象作为多个返回值.

use mlua::{Lua, MultiValue, Table, Value};

use crate::error::{Error, Result};

pub fn install(lua: &Lua) -> Result<()> {
    let lib = lua
        .create_table()
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    let map_err = |e: mlua::Error| Error::Command(format!("ERR sandbox setup failed: {e}"));
    lib.set(
        "pack",
        lua.create_function(|lua, args: MultiValue| {
            let mut out = Vec::new();
            for value in args {
                write_value(lua, &value, &mut out)?;
            }
            lua.create_string(out)
        })
        .map_err(map_err)?,
    )
    .map_err(map_err)?;
    lib.set(
        "unpack",
        lua.create_function(|lua, data: mlua::String| {
            let bytes = data.as_bytes();
            let bytes = bytes.as_ref();
            let mut cursor = 0;
            let mut values = Vec::new();
            if bytes.is_empty() {
                return Err(mlua::Error::runtime("Missing bytes in input"));
            }
            while cursor < bytes.len() {
                let (value, next) = read_value(lua, bytes, cursor)?;
                cursor = next;
                values.push(value);
            }
            Ok(MultiValue::from_vec(values))
        })
        .map_err(map_err)?,
    )
    .map_err(map_err)?;
    lua.globals().set("cmsgpack", lib).map_err(map_err)?;
    Ok(())
}

fn write_value(lua: &Lua, value: &Value, out: &mut Vec<u8>) -> mlua::Result<()> {
    match value {
        Value::Nil => out.push(0xc0),
        Value::Boolean(false) => out.push(0xc2),
        Value::Boolean(true) => out.push(0xc3),
        Value::Integer(i) => write_int(out, *i),
        Value::Number(n) => {
            if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
                write_int(out, *n as i64);
            } else {
                out.push(0xcb);
                out.extend_from_slice(&n.to_bits().to_be_bytes());
            }
        }
        Value::String(s) => write_bytes(out, s.as_bytes().as_ref()),
        Value::Table(table) => write_table(lua, table, out)?,
        _ => return Err(mlua::Error::runtime("unsupported type")),
    }
    Ok(())
}

fn write_int(out: &mut Vec<u8>, value: i64) {
    if (-32..128).contains(&value) {
        out.push(value as u8);
    } else if (i8::MIN as i64..=i8::MAX as i64).contains(&value) {
        out.push(0xd0);
        out.push(value as u8);
    } else if (i16::MIN as i64..=i16::MAX as i64).contains(&value) {
        out.push(0xd1);
        out.extend_from_slice(&(value as i16).to_be_bytes());
    } else if (i32::MIN as i64..=i32::MAX as i64).contains(&value) {
        out.push(0xd2);
        out.extend_from_slice(&(value as i32).to_be_bytes());
    } else {
        out.push(0xd3);
        out.extend_from_slice(&value.to_be_bytes());
    }
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = bytes.len();
    if len < 32 {
        out.push(0xa0 | len as u8);
    } else if len <= u8::MAX as usize {
        out.push(0xd9);
        out.push(len as u8);
    } else if len <= u16::MAX as usize {
        out.push(0xda);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0xdb);
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
    out.extend_from_slice(bytes);
}

fn write_table(lua: &Lua, table: &Table, out: &mut Vec<u8>) -> mlua::Result<()> {
    let mut pairs = Vec::new();
    let mut max_index = 0i64;
    let mut count = 0i64;
    let mut object = false;
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        match int_key(&key) {
            Some(index) if index >= 1 => {
                max_index = max_index.max(index);
                count += 1;
                pairs.push((key, value));
            }
            _ => {
                object = true;
                pairs.push((key, value));
            }
        }
    }
    if !object && max_index == count {
        write_len(out, count as usize, 0x90, 0xdc, 0xdd);
        for index in 1..=count {
            let value: Value = table.get(index)?;
            write_value(lua, &value, out)?;
        }
        return Ok(());
    }
    write_len(out, pairs.len(), 0x80, 0xde, 0xdf);
    for (key, value) in pairs {
        match key {
            Value::String(_) | Value::Integer(_) | Value::Number(_) => write_value(lua, &key, out)?,
            _ => return Err(mlua::Error::runtime("unsupported map key")),
        }
        write_value(lua, &value, out)?;
    }
    Ok(())
}

fn write_len(out: &mut Vec<u8>, len: usize, fix: u8, wide16: u8, wide32: u8) {
    if len < 16 {
        out.push(fix | len as u8);
    } else if len <= u16::MAX as usize {
        out.push(wide16);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(wide32);
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
}

fn int_key(key: &Value) -> Option<i64> {
    match key {
        Value::Integer(i) => Some(*i),
        Value::Number(n) if n.fract() == 0.0 => Some(*n as i64),
        _ => None,
    }
}

fn read_value(lua: &Lua, bytes: &[u8], index: usize) -> mlua::Result<(Value, usize)> {
    let marker = *bytes
        .get(index)
        .ok_or_else(|| mlua::Error::runtime("Missing bytes in input"))?;
    if marker <= 0x7f {
        return Ok((Value::Integer(i64::from(marker)), index + 1));
    }
    if (0xe0..=0xff).contains(&marker) {
        return Ok((Value::Integer(marker as i8 as i64), index + 1));
    }
    if (0xa0..=0xbf).contains(&marker) {
        return read_raw(lua, bytes, index + 1, (marker & 0x1f) as usize);
    }
    if (0x90..=0x9f).contains(&marker) {
        return read_array(lua, bytes, index + 1, (marker & 0x0f) as usize);
    }
    if (0x80..=0x8f).contains(&marker) {
        return read_map(lua, bytes, index + 1, (marker & 0x0f) as usize);
    }
    match marker {
        0xc0 => Ok((Value::Nil, index + 1)),
        0xc2 => Ok((Value::Boolean(false), index + 1)),
        0xc3 => Ok((Value::Boolean(true), index + 1)),
        0xc4 | 0xc5 | 0xc6 | 0xd9 | 0xda | 0xdb => {
            let (len, next) = read_len(bytes, index)?;
            read_raw(lua, bytes, next, len)
        }
        0xca => {
            let raw = read_n(bytes, index + 1, 4)?;
            let n = f32::from_bits(u32::from_be_bytes(raw.try_into().unwrap())) as f64;
            Ok((Value::Number(n), index + 5))
        }
        0xcb => {
            let raw = read_n(bytes, index + 1, 8)?;
            let n = f64::from_bits(u64::from_be_bytes(raw.try_into().unwrap()));
            Ok((Value::Number(n), index + 9))
        }
        0xcc => Ok((
            Value::Integer(i64::from(need(bytes, index + 1)?)),
            index + 2,
        )),
        0xcd => {
            let raw = read_n(bytes, index + 1, 2)?;
            Ok((
                Value::Integer(i64::from(u16::from_be_bytes(raw.try_into().unwrap()))),
                index + 3,
            ))
        }
        0xce => {
            let raw = read_n(bytes, index + 1, 4)?;
            Ok((
                Value::Integer(i64::from(u32::from_be_bytes(raw.try_into().unwrap()))),
                index + 5,
            ))
        }
        0xcf => {
            let raw = read_n(bytes, index + 1, 8)?;
            let n = u64::from_be_bytes(raw.try_into().unwrap());
            if n > i64::MAX as u64 {
                Ok((Value::Number(n as f64), index + 9))
            } else {
                Ok((Value::Integer(n as i64), index + 9))
            }
        }
        0xd0 => Ok((
            Value::Integer(i64::from(need(bytes, index + 1)? as i8)),
            index + 2,
        )),
        0xd1 => {
            let raw = read_n(bytes, index + 1, 2)?;
            Ok((
                Value::Integer(i64::from(i16::from_be_bytes(raw.try_into().unwrap()))),
                index + 3,
            ))
        }
        0xd2 => {
            let raw = read_n(bytes, index + 1, 4)?;
            Ok((
                Value::Integer(i64::from(i32::from_be_bytes(raw.try_into().unwrap()))),
                index + 5,
            ))
        }
        0xd3 => {
            let raw = read_n(bytes, index + 1, 8)?;
            Ok((
                Value::Integer(i64::from_be_bytes(raw.try_into().unwrap())),
                index + 9,
            ))
        }
        0xdc => {
            let len = u16::from_be_bytes(read_n(bytes, index + 1, 2)?.try_into().unwrap()) as usize;
            read_array(lua, bytes, index + 3, len)
        }
        0xdd => {
            let len = u32::from_be_bytes(read_n(bytes, index + 1, 4)?.try_into().unwrap()) as usize;
            read_array(lua, bytes, index + 5, len)
        }
        0xde => {
            let len = u16::from_be_bytes(read_n(bytes, index + 1, 2)?.try_into().unwrap()) as usize;
            read_map(lua, bytes, index + 3, len)
        }
        0xdf => {
            let len = u32::from_be_bytes(read_n(bytes, index + 1, 4)?.try_into().unwrap()) as usize;
            read_map(lua, bytes, index + 5, len)
        }
        _ => Err(mlua::Error::runtime("unsupported msgpack type")),
    }
}

fn read_len(bytes: &[u8], index: usize) -> mlua::Result<(usize, usize)> {
    let marker = need(bytes, index)?;
    match marker {
        0xc4 | 0xd9 => Ok((need(bytes, index + 1)? as usize, index + 2)),
        0xc5 | 0xda => {
            let len = u16::from_be_bytes(read_n(bytes, index + 1, 2)?.try_into().unwrap());
            Ok((len as usize, index + 3))
        }
        0xc6 | 0xdb => {
            let len = u32::from_be_bytes(read_n(bytes, index + 1, 4)?.try_into().unwrap());
            Ok((len as usize, index + 5))
        }
        _ => Err(mlua::Error::runtime("bad length")),
    }
}

fn read_raw(lua: &Lua, bytes: &[u8], index: usize, len: usize) -> mlua::Result<(Value, usize)> {
    let slice = bytes
        .get(index..index + len)
        .ok_or_else(|| mlua::Error::runtime("Missing bytes in input"))?;
    Ok((Value::String(lua.create_string(slice)?), index + len))
}

fn read_array(
    lua: &Lua,
    bytes: &[u8],
    mut index: usize,
    len: usize,
) -> mlua::Result<(Value, usize)> {
    let table = lua.create_table()?;
    for slot in 1..=len {
        let (value, next) = read_value(lua, bytes, index)?;
        table.set(slot, value)?;
        index = next;
    }
    Ok((Value::Table(table), index))
}

fn read_map(lua: &Lua, bytes: &[u8], mut index: usize, len: usize) -> mlua::Result<(Value, usize)> {
    let table = lua.create_table()?;
    for _ in 0..len {
        let (key, next) = read_value(lua, bytes, index)?;
        let (value, next) = read_value(lua, bytes, next)?;
        match key {
            Value::String(s) => {
                let text = String::from_utf8_lossy(s.as_bytes().as_ref()).into_owned();
                table.set(text, value)?;
            }
            Value::Integer(i) => table.set(i, value)?,
            Value::Number(n) => table.set(n, value)?,
            _ => return Err(mlua::Error::runtime("unsupported map key")),
        }
        index = next;
    }
    Ok((Value::Table(table), index))
}

fn need(bytes: &[u8], index: usize) -> mlua::Result<u8> {
    bytes
        .get(index)
        .copied()
        .ok_or_else(|| mlua::Error::runtime("Missing bytes in input"))
}

fn read_n(bytes: &[u8], index: usize, len: usize) -> mlua::Result<Vec<u8>> {
    bytes
        .get(index..index + len)
        .map(|s| s.to_vec())
        .ok_or_else(|| mlua::Error::runtime("Missing bytes in input"))
}
