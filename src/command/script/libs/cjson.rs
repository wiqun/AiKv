//! Redis 脚本 `cjson`. 覆盖 encode/decode、`cjson.null` 和稀疏数组默认拒绝.

use mlua::{Lua, Table, Value};

use crate::error::{Error, Result};

const MAX_DEPTH: usize = 1000;
const SPARSE_SAFE: i64 = 10;
const SPARSE_RATIO: i64 = 2;

pub fn install(lua: &Lua) -> Result<()> {
    let cjson = lua
        .create_table()
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    let null_value = lua
        .create_table()
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    let map_err = |e: mlua::Error| Error::Command(format!("ERR sandbox setup failed: {e}"));
    cjson.set("null", null_value).map_err(map_err)?;
    cjson
        .set(
            "encode",
            lua.create_function(|lua, value: Value| encode_value(lua, &value, 0))
                .map_err(map_err)?,
        )
        .map_err(map_err)?;
    cjson
        .set(
            "decode",
            lua.create_function(|lua, text: mlua::String| {
                decode_text(lua, text.as_bytes().as_ref())
            })
            .map_err(map_err)?,
        )
        .map_err(map_err)?;
    lua.globals().set("cjson", cjson).map_err(map_err)?;
    Ok(())
}

fn encode_value(lua: &Lua, value: &Value, depth: usize) -> mlua::Result<String> {
    if depth > MAX_DEPTH {
        return Err(mlua::Error::runtime("Cannot serialise, excessive nesting"));
    }
    let mut out = String::new();
    write_value(lua, value, depth, &mut out)?;
    Ok(out)
}

fn write_value(lua: &Lua, value: &Value, depth: usize, out: &mut String) -> mlua::Result<()> {
    if is_null(lua, value)? {
        out.push_str("null");
        return Ok(());
    }
    match value {
        Value::Nil => Err(mlua::Error::runtime("Cannot serialise nil")),
        Value::Boolean(true) => {
            out.push_str("true");
            Ok(())
        }
        Value::Boolean(false) => {
            out.push_str("false");
            Ok(())
        }
        Value::Integer(i) => {
            out.push_str(&i.to_string());
            Ok(())
        }
        Value::Number(n) => {
            if !n.is_finite() {
                return Err(mlua::Error::runtime("Cannot serialise number"));
            }
            if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
                out.push_str(&(*n as i64).to_string());
            } else {
                out.push_str(&n.to_string());
            }
            Ok(())
        }
        Value::String(s) => {
            out.push('"');
            for byte in s.as_bytes().as_ref().iter().copied() {
                match byte {
                    b'"' => out.push_str("\\\""),
                    b'\\' => out.push_str("\\\\"),
                    b'\n' => out.push_str("\\n"),
                    b'\r' => out.push_str("\\r"),
                    b'\t' => out.push_str("\\t"),
                    0x20..=0x7e => out.push(byte as char),
                    _ => out.push_str(&format!("\\u{byte:04x}")),
                }
            }
            out.push('"');
            Ok(())
        }
        Value::Table(table) => write_table(lua, table, depth, out),
        _ => Err(mlua::Error::runtime("Cannot serialise value")),
    }
}

fn is_null(lua: &Lua, value: &Value) -> mlua::Result<bool> {
    let Value::Table(table) = value else {
        return Ok(false);
    };
    let cjson: Table = lua.globals().get("cjson")?;
    let null_value: Value = cjson.get("null")?;
    let Value::Table(null_table) = null_value else {
        return Ok(false);
    };
    Ok(table.to_pointer() == null_table.to_pointer())
}

fn write_table(lua: &Lua, table: &Table, depth: usize, out: &mut String) -> mlua::Result<()> {
    let mut object_keys = Vec::new();
    let mut max_index = 0i64;
    let mut count = 0i64;
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        if matches!(value, Value::Nil) {
            continue;
        }
        match int_key(&key) {
            Some(index) if index >= 1 => {
                max_index = max_index.max(index);
                count += 1;
            }
            _ => object_keys.push((key, value)),
        }
    }
    if object_keys.is_empty() && count > 0 {
        if max_index > SPARSE_SAFE && max_index > count * SPARSE_RATIO {
            return Err(mlua::Error::runtime(
                "Cannot serialise table: excessively sparse array",
            ));
        }
        out.push('[');
        for index in 1..=max_index {
            if index > 1 {
                out.push(',');
            }
            match table.get::<Value>(index)? {
                Value::Nil => out.push_str("null"),
                value => write_value(lua, &value, depth + 1, out)?,
            }
        }
        out.push(']');
        return Ok(());
    }
    out.push('{');
    let mut first = true;
    for index in 1..=max_index {
        let value: Value = table.get(index)?;
        if matches!(value, Value::Nil) {
            continue;
        }
        if !first {
            out.push(',');
        }
        first = false;
        write_value(
            lua,
            &Value::String(lua.create_string(index.to_string())?),
            depth + 1,
            out,
        )?;
        out.push(':');
        write_value(lua, &value, depth + 1, out)?;
    }
    for (key, value) in object_keys {
        if !first {
            out.push(',');
        }
        first = false;
        let name = match key {
            Value::String(s) => s,
            Value::Integer(i) => lua.create_string(i.to_string())?,
            Value::Number(n) => lua.create_string(n.to_string())?,
            _ => return Err(mlua::Error::runtime("Cannot serialise table key")),
        };
        write_value(lua, &Value::String(name), depth + 1, out)?;
        out.push(':');
        write_value(lua, &value, depth + 1, out)?;
    }
    out.push('}');
    Ok(())
}

fn int_key(key: &Value) -> Option<i64> {
    match key {
        Value::Integer(i) => Some(*i),
        Value::Number(n) if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 => {
            Some(*n as i64)
        }
        _ => None,
    }
}

fn decode_text(lua: &Lua, bytes: &[u8]) -> mlua::Result<Value> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| mlua::Error::runtime("Expected value but found invalid UTF-8"))?;
    let mut parser = Parser { text, index: 0 };
    let value = parser.parse_value(lua)?;
    parser.skip_ws();
    if parser.index != parser.text.len() {
        return Err(mlua::Error::runtime("Trailing data"));
    }
    Ok(value)
}

struct Parser<'a> {
    text: &'a str,
    index: usize,
}

impl<'a> Parser<'a> {
    fn parse_value(&mut self, lua: &Lua) -> mlua::Result<Value> {
        self.skip_ws();
        let Some(ch) = self.peek() else {
            return Err(mlua::Error::runtime("Expected value but found T_END"));
        };
        match ch {
            'n' => self.consume_literal("null", null_value(lua)?),
            't' => self.consume_literal("true", Value::Boolean(true)),
            'f' => self.consume_literal("false", Value::Boolean(false)),
            '"' => Ok(Value::String(lua.create_string(self.parse_string()?)?)),
            '[' => self.parse_array(lua),
            '{' => self.parse_object(lua),
            '-' | '0'..='9' => self.parse_number(lua),
            _ => Err(mlua::Error::runtime("Expected value")),
        }
    }

    fn consume_literal(&mut self, literal: &str, value: Value) -> mlua::Result<Value> {
        if self.text[self.index..].starts_with(literal) {
            self.index += literal.len();
            Ok(value)
        } else {
            Err(mlua::Error::runtime("Expected value"))
        }
    }

    fn parse_array(&mut self, lua: &Lua) -> mlua::Result<Value> {
        self.bump();
        let table = lua.create_table()?;
        self.skip_ws();
        if self.peek() == Some(']') {
            self.bump();
            return Ok(Value::Table(table));
        }
        let mut index = 1;
        loop {
            table.set(index, self.parse_value(lua)?)?;
            index += 1;
            self.skip_ws();
            match self.peek() {
                Some(',') => self.bump(),
                Some(']') => {
                    self.bump();
                    break;
                }
                _ => return Err(mlua::Error::runtime("Expected comma or array end")),
            }
        }
        Ok(Value::Table(table))
    }

    fn parse_object(&mut self, lua: &Lua) -> mlua::Result<Value> {
        self.bump();
        let table = lua.create_table()?;
        self.skip_ws();
        if self.peek() == Some('}') {
            self.bump();
            return Ok(Value::Table(table));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err(mlua::Error::runtime("Expected object key"));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(':') {
                return Err(mlua::Error::runtime("Expected colon"));
            }
            self.bump();
            let value = self.parse_value(lua)?;
            if let Ok(index) = key.parse::<i64>() {
                if index >= 1 {
                    table.set(index, value)?;
                } else {
                    table.set(key, value)?;
                }
            } else {
                table.set(key, value)?;
            }
            self.skip_ws();
            match self.peek() {
                Some(',') => self.bump(),
                Some('}') => {
                    self.bump();
                    break;
                }
                _ => return Err(mlua::Error::runtime("Expected comma or object end")),
            }
        }
        Ok(Value::Table(table))
    }

    fn parse_string(&mut self) -> mlua::Result<String> {
        self.bump();
        let mut out = String::new();
        while let Some(ch) = self.peek() {
            self.bump();
            match ch {
                '"' => return Ok(out),
                '\\' => {
                    let esc = self
                        .peek()
                        .ok_or_else(|| mlua::Error::runtime("Bad escape"))?;
                    self.bump();
                    match esc {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{0008}'),
                        'f' => out.push('\u{000c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let hex = self
                                .text
                                .get(self.index..self.index + 4)
                                .ok_or_else(|| mlua::Error::runtime("Bad unicode escape"))?;
                            let code = u32::from_str_radix(hex, 16)
                                .map_err(|_| mlua::Error::runtime("Bad unicode escape"))?;
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                            self.index += 4;
                        }
                        _ => return Err(mlua::Error::runtime("Bad escape")),
                    }
                }
                other => out.push(other),
            }
        }
        Err(mlua::Error::runtime("Unfinished string"))
    }

    fn parse_number(&mut self, lua: &Lua) -> mlua::Result<Value> {
        let start = self.index;
        if self.peek() == Some('-') {
            self.bump();
        }
        while matches!(self.peek(), Some('0'..='9' | '.' | 'e' | 'E' | '+' | '-')) {
            self.bump();
        }
        let text = &self.text[start..self.index];
        let number: f64 = text
            .parse()
            .map_err(|_| mlua::Error::runtime("Expected number"))?;
        if number.fract() == 0.0 && number >= i64::MIN as f64 && number <= i64::MAX as f64 {
            Ok(Value::Integer(number as i64))
        } else {
            let _ = lua;
            Ok(Value::Number(number))
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\n' | '\r' | '\t')) {
            self.bump();
        }
    }

    fn peek(&self) -> Option<char> {
        self.text[self.index..].chars().next()
    }

    fn bump(&mut self) {
        if let Some(ch) = self.peek() {
            self.index += ch.len_utf8();
        }
    }
}

fn null_value(lua: &Lua) -> mlua::Result<Value> {
    let cjson: Table = lua.globals().get("cjson")?;
    cjson.get("null")
}
