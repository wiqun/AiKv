//! Lua 沙箱: StdLib 裁剪 + 危险全局函数封印
//!
//! mlua 创建 VM 时总会加载 `luaopen_base` (spec 中的 BASE); 此处再显式启用 TABLE/STRING/MATH/UTF8.

use mlua::{Lua, LuaOptions, StdLib, Table, Value as LuaValue};

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
        StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8,
        LuaOptions::default(),
    )
    .map_err(|e| Error::Command(format!("ERR failed to create Lua: {e}")))?;

    harden_globals(&lua)?;
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

    let math_table: mlua::Table = globals
        .get("math")
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    math_table
        .set(
            "randomseed",
            lua.create_function(|_, _: mlua::MultiValue| Ok(()))
                .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?,
        )
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;

    Ok(())
}

const SHA_REGISTRY: &str = "aikv_sha";

struct IdleLua {
    lua: Lua,
    base_globals: std::collections::HashSet<String>,
    generation: u64,
}

pub(crate) struct LuaPool {
    idle: std::sync::Mutex<Vec<IdleLua>>,
    capacity: usize,
    #[cfg_attr(not(test), allow(dead_code))]
    created: std::sync::atomic::AtomicUsize,
}

pub(crate) struct PooledLua<'a> {
    inner: Option<IdleLua>,
    pool: &'a LuaPool,
}

impl LuaPool {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            idle: std::sync::Mutex::new(Vec::new()),
            capacity: capacity.max(1),
            created: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub(crate) fn with_available_parallelism() -> Self {
        let capacity = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Self::new(capacity)
    }

    #[cfg(test)]
    pub(crate) fn created(&self) -> usize {
        self.created.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn checkout(&self, generation: u64) -> Result<PooledLua<'_>> {
        let popped = self.idle.lock().unwrap().pop();
        if let Some(mut item) = popped {
            if item.generation != generation {
                let _ = item.lua.unset_named_registry_value(SHA_REGISTRY);
                item.generation = generation;
            }
            return Ok(PooledLua {
                inner: Some(item),
                pool: self,
            });
        }

        let lua = new_sandbox_lua()?;
        let base_globals = prepare_reusable_vm(&lua)?;
        self.created
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(PooledLua {
            inner: Some(IdleLua {
                lua,
                base_globals,
                generation,
            }),
            pool: self,
        })
    }
}

impl std::ops::Deref for PooledLua<'_> {
    type Target = Lua;

    fn deref(&self) -> &Lua {
        &self
            .inner
            .as_ref()
            .expect("pooled lua still checked out")
            .lua
    }
}

impl Drop for PooledLua<'_> {
    fn drop(&mut self) {
        let Some(idle) = self.inner.take() else {
            return;
        };
        if reset_for_reuse(&idle.lua, &idle.base_globals).is_err() {
            return;
        }
        let mut slot = self.pool.idle.lock().unwrap();
        if slot.len() < self.pool.capacity {
            slot.push(idle);
        }
    }
}

pub(crate) async fn run_chunk(
    lua: &Lua,
    sha: Option<&str>,
    script: &str,
    timeout: std::time::Duration,
) -> Result<LuaValue> {
    let func = if let Some(sha) = sha {
        let table = sha_table(lua)?;
        if let Ok(func) = table.get::<mlua::Function>(sha) {
            func
        } else {
            let func = lua
                .load(script)
                .into_function()
                .map_err(|e| Error::Command(format!("ERR Error running script: {e}")))?;
            table
                .set(sha, func.clone())
                .map_err(|e| Error::Command(format!("ERR Error running script: {e}")))?;
            func
        }
    } else {
        lua.load(script)
            .into_function()
            .map_err(|e| Error::Command(format!("ERR Error running script: {e}")))?
    };
    // `Lua::set_hook` 挂在主状态上。`call_async` 在协程里跑脚本，mlua 发现
    // hook 不属于这条协程就会把 hook 拆掉，死循环因此停不下来。
    let thread = lua
        .create_thread(func)
        .map_err(|e| Error::Command(format!("ERR Error running script: {e}")))?;
    let started = std::time::Instant::now();
    thread.set_hook(
        mlua::HookTriggers::new().every_nth_instruction(10_000),
        move |_lua, _debug| {
            if started.elapsed() >= timeout {
                return Err(mlua::Error::RuntimeError("Script timed out".to_string()));
            }
            Ok(mlua::VmState::Continue)
        },
    );
    let result = thread.into_async::<LuaValue>(()).await;
    lua.remove_hook();
    result.map_err(|e| Error::Command(format!("ERR Error running script: {e}")))
}

fn sha_table(lua: &Lua) -> Result<Table> {
    if let Ok(table) = lua.named_registry_value::<Table>(SHA_REGISTRY) {
        return Ok(table);
    }
    let table = lua
        .create_table()
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    lua.set_named_registry_value(SHA_REGISTRY, table.clone())
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    Ok(table)
}

fn prepare_reusable_vm(lua: &Lua) -> Result<std::collections::HashSet<String>> {
    // mlua 的异步函数依赖全局 `coroutine`。库只加载一次；重置时不能把它清掉。
    lua.create_async_function(|_, _: mlua::MultiValue| async { Ok(()) })
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    if let Ok(coroutine) = lua.globals().get::<Table>("coroutine") {
        lua.set_named_registry_value("aikv_coroutine", coroutine)
            .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    }
    capture_globals(lua)
}

fn capture_globals(lua: &Lua) -> Result<std::collections::HashSet<String>> {
    let mut names = std::collections::HashSet::new();
    for pair in lua.globals().pairs::<String, LuaValue>() {
        let (key, _) =
            pair.map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
        names.insert(key);
    }
    Ok(names)
}

fn reset_for_reuse(lua: &Lua, base_globals: &std::collections::HashSet<String>) -> Result<()> {
    let globals = lua.globals();
    let mut extra = Vec::new();
    for pair in globals.pairs::<String, LuaValue>() {
        let (key, _) =
            pair.map_err(|e| Error::Command(format!("ERR sandbox reset failed: {e}")))?;
        if !base_globals.contains(&key) {
            extra.push(key);
        }
    }
    for key in extra {
        globals
            .set(key, LuaValue::Nil)
            .map_err(|e| Error::Command(format!("ERR sandbox reset failed: {e}")))?;
    }
    for name in ["KEYS", "ARGV", "redis"] {
        globals
            .set(name, LuaValue::Nil)
            .map_err(|e| Error::Command(format!("ERR sandbox reset failed: {e}")))?;
    }
    if let Ok(coroutine) = lua.named_registry_value::<Table>("aikv_coroutine") {
        globals
            .set("coroutine", coroutine)
            .map_err(|e| Error::Command(format!("ERR sandbox reset failed: {e}")))?;
    }
    lua.gc_collect()
        .map_err(|e| Error::Command(format!("ERR sandbox reset failed: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_reuses_one_vm_and_clears_globals() {
        let pool = LuaPool::new(1);
        {
            let lua = pool.checkout(0).unwrap();
            lua.load("_G.leaked = 7").exec().unwrap();
            lua.create_async_function(|_, _: mlua::MultiValue| async { Ok(()) })
                .unwrap();
        }
        {
            let lua = pool.checkout(0).unwrap();
            let value: LuaValue = lua.load("return leaked").eval().unwrap();
            assert!(matches!(value, LuaValue::Nil));
            let err = lua.load("return load('return 1')").exec().unwrap_err();
            assert!(err.to_string().contains("nil") || err.to_string().contains("call"));
            lua.create_async_function(|_, _: mlua::MultiValue| async { Ok(()) })
                .unwrap();
        }
        assert_eq!(pool.created(), 1);
    }

    #[test]
    fn tight_loop_hits_timeout_hook() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let lua = new_sandbox_lua().unwrap();
            let started = std::time::Instant::now();
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                run_chunk(
                    &lua,
                    None,
                    "while true do end",
                    std::time::Duration::from_millis(200),
                ),
            )
            .await;
            let err = result.expect("hook did not stop the script").unwrap_err();
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "elapsed {:?}",
                started.elapsed()
            );
            assert!(err.to_string().contains("Script timed out"), "{err}");
        });
    }
}
