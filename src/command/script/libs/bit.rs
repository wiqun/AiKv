//! Lua BitOp, 32 位. 与 Redis 脚本里的 `bit` 库同一套函数名.

use mlua::{Lua, MultiValue, Value};

use crate::error::{Error, Result};

pub fn install(lua: &Lua) -> Result<()> {
    let bit = lua
        .create_table()
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    let map_err = |e: mlua::Error| Error::Command(format!("ERR sandbox setup failed: {e}"));

    bit.set(
        "tobit",
        lua.create_function(|_, n: Value| Ok(signed(bits_of(n)?)))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    bit.set(
        "tohex",
        lua.create_function(|_, args: MultiValue| tohex(args))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    bit.set(
        "bnot",
        lua.create_function(|_, n: Value| Ok(signed(!bits_of(n)?)))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    bit.set(
        "band",
        lua.create_function(|_, args: MultiValue| fold_bits(args, |a, b| a & b))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    bit.set(
        "bor",
        lua.create_function(|_, args: MultiValue| fold_bits(args, |a, b| a | b))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    bit.set(
        "bxor",
        lua.create_function(|_, args: MultiValue| fold_bits(args, |a, b| a ^ b))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    bit.set(
        "lshift",
        lua.create_function(|_, args: MultiValue| shift(args, false, false))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    bit.set(
        "rshift",
        lua.create_function(|_, args: MultiValue| shift(args, true, false))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    bit.set(
        "arshift",
        lua.create_function(|_, args: MultiValue| shift(args, true, true))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    bit.set(
        "rol",
        lua.create_function(|_, args: MultiValue| rotate(args, false))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    bit.set(
        "ror",
        lua.create_function(|_, args: MultiValue| rotate(args, true))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    bit.set(
        "bswap",
        lua.create_function(|_, n: Value| Ok(signed(bits_of(n)?.swap_bytes())))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;

    lua.globals().set("bit", bit).map_err(map_err)?;
    Ok(())
}

fn bits_of(value: Value) -> mlua::Result<u32> {
    let n = match value {
        Value::Integer(i) => i as f64,
        Value::Number(n) => n,
        Value::String(s) => std::str::from_utf8(s.as_bytes().as_ref())
            .ok()
            .and_then(|t| t.trim().parse::<f64>().ok())
            .unwrap_or(0.0),
        Value::Nil => 0.0,
        _ => {
            return Err(mlua::Error::runtime("number expected"));
        }
    };
    Ok(n as i64 as u32)
}

fn signed(bits: u32) -> f64 {
    bits as i32 as f64
}

fn fold_bits(args: MultiValue, op: fn(u32, u32) -> u32) -> mlua::Result<f64> {
    let mut iter = args.into_iter();
    let Some(first) = iter.next() else {
        return Err(mlua::Error::runtime("wrong number of arguments"));
    };
    let mut acc = bits_of(first)?;
    for arg in iter {
        acc = op(acc, bits_of(arg)?);
    }
    Ok(signed(acc))
}

fn shift(args: MultiValue, right: bool, arithmetic: bool) -> mlua::Result<f64> {
    let mut iter = args.into_iter();
    let value = bits_of(
        iter.next()
            .ok_or_else(|| mlua::Error::runtime("wrong number of arguments"))?,
    )?;
    let count = bits_of(
        iter.next()
            .ok_or_else(|| mlua::Error::runtime("wrong number of arguments"))?,
    )? as i32;
    let (right, n) = if count < 0 {
        (!right, (-count) as u32)
    } else {
        (right, count as u32)
    };
    if n >= 32 {
        let filled = if arithmetic && right && (value as i32) < 0 {
            u32::MAX
        } else {
            0
        };
        return Ok(signed(filled));
    }
    let shifted = if right {
        if arithmetic {
            ((value as i32) >> n) as u32
        } else {
            value >> n
        }
    } else {
        value << n
    };
    Ok(signed(shifted))
}

fn rotate(args: MultiValue, right: bool) -> mlua::Result<f64> {
    let mut iter = args.into_iter();
    let value = bits_of(
        iter.next()
            .ok_or_else(|| mlua::Error::runtime("wrong number of arguments"))?,
    )?;
    let count = (bits_of(
        iter.next()
            .ok_or_else(|| mlua::Error::runtime("wrong number of arguments"))?,
    )? & 31) as u32;
    let count = if right { count } else { 32 - count };
    let rotated = value.rotate_right(count);
    Ok(signed(rotated))
}

fn tohex(args: MultiValue) -> mlua::Result<String> {
    let mut iter = args.into_iter();
    let value = bits_of(
        iter.next()
            .ok_or_else(|| mlua::Error::runtime("wrong number of arguments"))?,
    )?;
    let mut n = match iter.next() {
        Some(v) => bits_of(v)? as i32,
        None => 8,
    };
    let digits = if n < 0 {
        n = -n;
        "0123456789ABCDEF"
    } else {
        "0123456789abcdef"
    };
    let n = (n as usize).min(8);
    let mut out = String::new();
    for i in (0..n).rev() {
        let nib = ((value >> (i * 4)) & 0xf) as usize;
        out.push(digits.as_bytes()[nib] as char);
    }
    Ok(out)
}
