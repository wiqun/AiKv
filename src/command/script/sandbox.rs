//! Lua 沙箱: StdLib 裁剪 + 危险全局函数封印
//!
//! mlua 创建 VM 时总会加载 `luaopen_base` (spec 中的 BASE); 此处再显式启用 TABLE/STRING/MATH.
//! 运行时是 Lua 5.1, 与 Redis 脚本一致, 没有 5.3 以后的 `utf8` 库.
//! `os` 只提供 `clock`. `cjson` / `bit` / `struct` / `cmsgpack` 和 Redis 的 `math.random` 一并装上.

use mlua::{Lua, LuaOptions, StdLib, Value as LuaValue};

use super::libs;
use crate::error::{Error, Result};

const DISABLED_GLOBALS: &[&str] = &[
    "load",
    "loadfile",
    "dofile",
    "require",
    "rawget",
    "rawset",
    "setmetatable",
    "getmetatable",
    "collectgarbage",
];

pub fn new_sandbox_lua() -> Result<Lua> {
    let lua = Lua::new_with(
        StdLib::TABLE | StdLib::STRING | StdLib::MATH,
        LuaOptions::default(),
    )
    .map_err(|e| Error::Command(format!("ERR failed to create Lua: {e}")))?;

    harden_globals(&lua)?;
    libs::install(&lua)?;
    Ok(lua)
}

fn harden_globals(lua: &Lua) -> Result<()> {
    let globals = lua.globals();
    for name in DISABLED_GLOBALS {
        globals
            .set(*name, LuaValue::Nil)
            .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    }

    let string_table: mlua::Table = globals
        .get("string")
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    string_table
        .set("dump", LuaValue::Nil)
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;

    Ok(())
}
