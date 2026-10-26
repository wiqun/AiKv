//! Redis 脚本 `struct.pack` / `struct.unpack` / `struct.size`.
//!
//! 格式与 Roberto Ierusalimschy 的 `struct` 库一致: `<` 小端, `>` 大端, `bBhHiIlLfdscx`.

use mlua::{Lua, MultiValue, Value};

use crate::error::{Error, Result};

pub fn install(lua: &Lua) -> Result<()> {
    let st = lua
        .create_table()
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    let map_err = |e: mlua::Error| Error::Command(format!("ERR sandbox setup failed: {e}"));
    st.set(
        "pack",
        lua.create_function(|lua, args: MultiValue| pack(lua, args))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    st.set(
        "unpack",
        lua.create_function(|lua, args: MultiValue| unpack(lua, args))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    st.set(
        "size",
        lua.create_function(|_, fmt: mlua::String| {
            let bytes = fmt.as_bytes();
            let text = std::str::from_utf8(bytes.as_ref()).unwrap_or("");
            Ok(layout(text)?.0 as i64)
        })
        .map_err(map_err)?,
    )
    .map_err(map_err)?;
    lua.globals().set("struct", st).map_err(map_err)?;
    Ok(())
}

#[derive(Clone, Copy)]
enum Kind {
    Pad,
    Int { signed: bool, width: usize },
    Float { width: usize },
    Bytes,
    ZString,
}

struct Field {
    big: bool,
    kind: Kind,
    count: usize,
}

fn layout(fmt: &str) -> mlua::Result<(usize, Vec<Field>)> {
    let mut big = false;
    let mut fields = Vec::new();
    let bytes = fmt.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b' ' => i += 1,
            b'<' => {
                big = false;
                i += 1;
            }
            b'>' => {
                big = true;
                i += 1;
            }
            b'=' => {
                big = cfg!(target_endian = "big");
                i += 1;
            }
            b'!' => {
                i += 1;
                let (_, next) = read_count(bytes, i, 1);
                i = next;
            }
            ch => {
                let kind = match ch {
                    b'x' => Kind::Pad,
                    b'b' => Kind::Int {
                        signed: true,
                        width: 1,
                    },
                    b'B' => Kind::Int {
                        signed: false,
                        width: 1,
                    },
                    b'h' => Kind::Int {
                        signed: true,
                        width: 2,
                    },
                    b'H' => Kind::Int {
                        signed: false,
                        width: 2,
                    },
                    b'i' => Kind::Int {
                        signed: true,
                        width: 4,
                    },
                    b'I' => Kind::Int {
                        signed: false,
                        width: 4,
                    },
                    b'l' => Kind::Int {
                        signed: true,
                        width: 8,
                    },
                    b'L' => Kind::Int {
                        signed: false,
                        width: 8,
                    },
                    b'f' => Kind::Float { width: 4 },
                    b'd' => Kind::Float { width: 8 },
                    b'c' => Kind::Bytes,
                    b's' => Kind::ZString,
                    _ => return Err(mlua::Error::runtime("invalid format option")),
                };
                i += 1;
                let default = if matches!(kind, Kind::Bytes) { 0 } else { 1 };
                let (count, next) = read_count(bytes, i, default);
                if matches!(kind, Kind::Bytes) && count == 0 {
                    return Err(mlua::Error::runtime("missing size for format option 'c'"));
                }
                i = next;
                fields.push(Field { big, kind, count });
            }
        }
    }
    let mut size = 0usize;
    for field in &fields {
        let width = match field.kind {
            Kind::Pad | Kind::Bytes => 1,
            Kind::ZString => return Err(mlua::Error::runtime("option 's' has no fixed size")),
            Kind::Int { width, .. } | Kind::Float { width } => width,
        };
        size += width * field.count;
    }
    Ok((size, fields))
}

fn read_count(bytes: &[u8], mut i: usize, default: usize) -> (usize, usize) {
    if i >= bytes.len() || !bytes[i].is_ascii_digit() {
        return (default, i);
    }
    let mut n = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        n = n * 10 + (bytes[i] - b'0') as usize;
        i += 1;
    }
    (n, i)
}

fn pack(lua: &Lua, args: MultiValue) -> mlua::Result<mlua::String> {
    let mut iter = args.into_iter();
    let fmt = string_of(
        iter.next()
            .ok_or_else(|| mlua::Error::runtime("wrong number of arguments"))?,
    )?;
    let (_, fields) = layout(&fmt)?;
    let mut out = Vec::new();
    for field in fields {
        match field.kind {
            Kind::Pad => out.resize(out.len() + field.count, 0),
            Kind::Int { signed, width } => {
                for _ in 0..field.count {
                    let value = as_i64(
                        iter.next()
                            .ok_or_else(|| mlua::Error::runtime("missing argument"))?,
                    )?;
                    write_int(&mut out, value, width, signed, field.big);
                }
            }
            Kind::Float { width } => {
                for _ in 0..field.count {
                    let value = as_f64(
                        iter.next()
                            .ok_or_else(|| mlua::Error::runtime("missing argument"))?,
                    )?;
                    write_float(&mut out, value, width, field.big);
                }
            }
            Kind::Bytes => {
                let text = string_of(
                    iter.next()
                        .ok_or_else(|| mlua::Error::runtime("missing argument"))?,
                )?;
                let bytes = text.as_bytes();
                if bytes.len() > field.count {
                    out.extend_from_slice(&bytes[..field.count]);
                } else {
                    out.extend_from_slice(bytes);
                    out.resize(out.len() + field.count - bytes.len(), 0);
                }
            }
            Kind::ZString => {
                let text = string_of(
                    iter.next()
                        .ok_or_else(|| mlua::Error::runtime("missing argument"))?,
                )?;
                out.extend_from_slice(text.as_bytes());
                out.push(0);
            }
        }
    }
    lua.create_string(out)
}

fn unpack(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let mut iter = args.into_iter();
    let fmt = string_of(
        iter.next()
            .ok_or_else(|| mlua::Error::runtime("wrong number of arguments"))?,
    )?;
    let data = string_of(
        iter.next()
            .ok_or_else(|| mlua::Error::runtime("wrong number of arguments"))?,
    )?;
    let pos = match iter.next() {
        Some(value) => as_i64(value)?.max(1) as usize,
        None => 1,
    };
    let bytes = data.as_bytes();
    if pos == 0 || pos > bytes.len() + 1 {
        return Err(mlua::Error::runtime("data string too short"));
    }
    let mut cursor = pos - 1;
    let (_, fields) = layout(&fmt)?;
    let mut values = Vec::new();
    for field in fields {
        match field.kind {
            Kind::Pad => {
                cursor += field.count;
                if cursor > bytes.len() {
                    return Err(mlua::Error::runtime("data string too short"));
                }
            }
            Kind::Int { signed, width } => {
                for _ in 0..field.count {
                    if cursor + width > bytes.len() {
                        return Err(mlua::Error::runtime("data string too short"));
                    }
                    let value = read_int(&bytes[cursor..cursor + width], signed, field.big);
                    cursor += width;
                    values.push(Value::Integer(value));
                }
            }
            Kind::Float { width } => {
                for _ in 0..field.count {
                    if cursor + width > bytes.len() {
                        return Err(mlua::Error::runtime("data string too short"));
                    }
                    let value = read_float(&bytes[cursor..cursor + width], field.big);
                    cursor += width;
                    values.push(Value::Number(value));
                }
            }
            Kind::Bytes => {
                if cursor + field.count > bytes.len() {
                    return Err(mlua::Error::runtime("data string too short"));
                }
                let slice = &bytes[cursor..cursor + field.count];
                cursor += field.count;
                values.push(Value::String(lua.create_string(slice)?));
            }
            Kind::ZString => {
                let end = bytes[cursor..]
                    .iter()
                    .position(|b| *b == 0)
                    .ok_or_else(|| mlua::Error::runtime("unfinished string"))?;
                let slice = &bytes[cursor..cursor + end];
                cursor += end + 1;
                values.push(Value::String(lua.create_string(slice)?));
            }
        }
    }
    values.push(Value::Integer((cursor + 1) as i64));
    Ok(MultiValue::from_vec(values))
}

fn write_int(out: &mut Vec<u8>, value: i64, width: usize, _signed: bool, big: bool) {
    let raw = value as u64;
    let bytes = raw.to_le_bytes();
    let slice = if big {
        let be = raw.to_be_bytes();
        let start = 8 - width;
        be[start..].to_vec()
    } else {
        bytes[..width].to_vec()
    };
    out.extend_from_slice(&slice);
}

fn read_int(bytes: &[u8], signed: bool, big: bool) -> i64 {
    let mut raw = [0u8; 8];
    if big {
        let start = 8 - bytes.len();
        raw[start..].copy_from_slice(bytes);
        let value = u64::from_be_bytes(raw) as i64;
        if signed {
            sign_extend(value, bytes.len())
        } else {
            value
        }
    } else {
        raw[..bytes.len()].copy_from_slice(bytes);
        let value = i64::from_le_bytes(raw);
        if signed {
            sign_extend(value, bytes.len())
        } else {
            value & mask(bytes.len()) as i64
        }
    }
}

fn sign_extend(value: i64, width: usize) -> i64 {
    let bits = width * 8;
    let shift = 64 - bits;
    (value << shift) >> shift
}

fn mask(width: usize) -> u64 {
    if width >= 8 {
        u64::MAX
    } else {
        (1u64 << (width * 8)) - 1
    }
}

fn write_float(out: &mut Vec<u8>, value: f64, width: usize, big: bool) {
    if width == 4 {
        let bits = (value as f32).to_bits();
        if big {
            out.extend_from_slice(&bits.to_be_bytes());
        } else {
            out.extend_from_slice(&bits.to_le_bytes());
        }
    } else if big {
        out.extend_from_slice(&value.to_bits().to_be_bytes());
    } else {
        out.extend_from_slice(&value.to_bits().to_le_bytes());
    }
}

fn read_float(bytes: &[u8], big: bool) -> f64 {
    if bytes.len() == 4 {
        let mut raw = [0u8; 4];
        raw.copy_from_slice(bytes);
        let bits = if big {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        };
        f32::from_bits(bits) as f64
    } else {
        let mut raw = [0u8; 8];
        raw.copy_from_slice(bytes);
        let bits = if big {
            u64::from_be_bytes(raw)
        } else {
            u64::from_le_bytes(raw)
        };
        f64::from_bits(bits)
    }
}

fn string_of(value: Value) -> mlua::Result<String> {
    match value {
        Value::String(s) => Ok(String::from_utf8_lossy(s.as_bytes().as_ref()).into_owned()),
        Value::Integer(i) => Ok(i.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err(mlua::Error::runtime("string expected")),
    }
}

fn as_i64(value: Value) -> mlua::Result<i64> {
    match value {
        Value::Integer(i) => Ok(i),
        Value::Number(n) => Ok(n as i64),
        Value::String(s) => std::str::from_utf8(s.as_bytes().as_ref())
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .ok_or_else(|| mlua::Error::runtime("number expected")),
        _ => Err(mlua::Error::runtime("number expected")),
    }
}

fn as_f64(value: Value) -> mlua::Result<f64> {
    if let Value::Number(n) = value {
        Ok(n)
    } else {
        Ok(as_i64(value)? as f64)
    }
}
