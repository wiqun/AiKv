//! Redis 脚本附带库: `bit` / `cjson` / `struct` / `cmsgpack`, 以及 `math.random` 和 `os.clock`.
//!
//! 对齐 Redis 8.8 的 `script_lua.c` 加载集合. `os` 只留 `clock`, 不提供 `execute` / `getenv`.

mod bit;
mod cjson;
mod cmsgpack;
mod random;
mod struct_fmt;

use mlua::Lua;

use crate::error::{Error, Result};

pub fn install(lua: &Lua) -> Result<()> {
    bit::install(lua)?;
    cjson::install(lua)?;
    struct_fmt::install(lua)?;
    cmsgpack::install(lua)?;
    random::install(lua)?;
    install_os_clock(lua)?;
    Ok(())
}

fn install_os_clock(lua: &Lua) -> Result<()> {
    let os = lua
        .create_table()
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    let clock = lua
        .create_function(|_, ()| Ok(process_cpu_seconds()))
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    os.set("clock", clock)
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    lua.globals()
        .set("os", os)
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    Ok(())
}

fn process_cpu_seconds() -> f64 {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return 0.0;
    };
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return 0.0;
    };
    let mut fields = rest.split_whitespace();
    let utime: u64 = fields.nth(11).and_then(|s| s.parse().ok()).unwrap_or(0);
    let stime: u64 = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (utime + stime) as f64 / 100.0
}
