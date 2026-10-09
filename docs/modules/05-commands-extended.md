---
name: aikv-commands-extended
description: AiKv 扩展 Redis 命令 — JSON/JSONPath 引擎、Lua EVAL/SCRIPT 沙箱、BlockingRegistry 阻塞队列 (BLPOP/BRPOP)、MIGRATE 槽位数据网络同步与服务端管理命令 (INFO/CONFIG/CLIENT/COMMAND/SAVE). 修改 src/command/{json,jsonpath,script,script/execute,blocking,migrate,persistence,server} 时查阅.
---

# AiKv Commands Extended (扩展命令层)

## 何时读本文

- 修改 `src/command/{json.rs, jsonpath/, jsonpath_util.rs, script.rs, script/, script/execute/, blocking.rs, migrate.rs, persistence.rs, server.rs}` 源码;
- 新增或修改 JSON.*, Lua 脚本 (`EVAL` / `EVALSHA` / `SCRIPT`), 阻塞列表/ZSet (`BLPOP` / `BRPOP` / `BZPOPMIN`), MIGRATE 槽位数据同步, 或服务端管理命令 (`INFO` / `CONFIG` / `CLIENT` / `SAVE`);
- 排查 JSONPath 表达式求值异常、Lua 沙箱超时与 `redis.call` 写入回滚、阻塞队列假死或漏唤醒、MIGRATE 迁移同步失败;
- **不覆盖**: 核心数据结构命令与 `KeyLock` 基础机制 → [commands-core.md](04-commands-core.md);
- **不覆盖**: ATOM 事务处理 (`MULTI` / `EXEC`) → [server.md](02-server.md);
- **不覆盖**: 集群 MOVED/ASK 路由与 CLUSTER 子命令 → [cluster.md](06-cluster.md);
- **不覆盖**: Slowlog 环形缓冲、Latency 直方图与 OTel 监控管道 → [observability.md](07-observability.md).

> **命令清单**: 已实现命令以 [compatibility.md](../compatibility.md) 为准 (与 `all_commands()` 自动核对); 下文仅保留扩展命令分组索引.

---

## 代码地图

| 文件路径 | 模块核心职责 | 公共接口与核心入口 |
| :--- | :--- | :--- |
| [`src/command/json.rs`](../../src/command/json.rs) | RedisJSON 兼容命令处理器 (JSON.GET, JSON.SET, JSON.DEL, JSON.TYPE 等) | `json::dispatch` |
| [`src/command/jsonpath/`](../../src/command/jsonpath/) | JSONPath 语法解析器与 AST 求值引擎 (支持 `$` 根路径、属性递归与切片) | `JsonPathEngine` (`extract` / `set` / `delete` / `incr` / `append` / `split_path_parts`) |
| [`src/command/jsonpath_util.rs`](../../src/command/jsonpath_util.rs) | JSON 与 RESP 之间的类型转换辅助工具 | `json_to_resp`, `resp_to_json` |
| [`src/command/script.rs`](../../src/command/script.rs) | Lua 脚本命令入口 (`EVAL`, `EVALSHA`, `SCRIPT LOAD`, `SCRIPT EXISTS`, `SCRIPT FLUSH`) | `script::dispatch` |
| [`src/command/script/`](../../src/command/script/) | Lua 5.4 沙箱执行环境、`redis.call` 桥接与 `ScriptTransaction` 批处理 | `ScriptEngine`, `ScriptTransaction` |
| [`src/command/script/execute/`](../../src/command/script/execute/) | Lua `redis.call` / `redis.pcall` 异步执行: 集群同 slot 校验与按类型 `exec_*` | `redis_call_async` |
| [`src/command/blocking.rs`](../../src/command/blocking.rs) | `BlockingRegistry` 阻塞等待队列、超时管理与跨连接键写入唤醒机制 | `BlockingRegistry`, `block_on_keys`, `notify_keys` |
| [`src/command/migrate.rs`](../../src/command/migrate.rs) | `MIGRATE` / `RESTORE` 命令, 槽位迁移数据网络传输与原子导入 | `migrate::dispatch` |
| [`src/command/persistence.rs`](../../src/command/persistence.rs) | 持久化管理命令 (`SAVE`, `BGSAVE`, `LASTSAVE`, `SHUTDOWN`) | `persistence::dispatch` |
| [`src/command/server.rs`](../../src/command/server.rs) | 管理与反射命令 (`INFO`, `CONFIG`, `CLIENT`, `COMMAND`, `OBJECT`, `SLOWLOG`, `LATENCY`) | `server::dispatch` |

---

## 关键 Invariants (勿破坏规则)

- **JSON 存储与 JSONPath 原语**:
  - 全量 JSON 文档以合法 UTF-8 字符串保存在 `StoredValue { value_type: ValueType::String, .. }` 中;
  - `JSON.SET` 针对不存在的 Key 创建新文档, 针对已有文档根据 Path 局部修改;
  - `JSON.GET` 支持多 Path 查询, 返回标准格式化 JSON 字符串.
- **Lua 沙箱与原子事务提交 (`ScriptTransaction`)**:
  - 采用 `mlua` 运行独立 Lua 5.4 虚拟机, 裁剪 `os`, `io`, `debug` 等不安全库;
  - `EVAL` 执行前必须提取所有参数 Key, 并调用 `KeyLock::lock_keys_sorted_with_timeout(keys, 30s)` 获取锁;
  - 脚本可以访问未写进 KEYS 的 key。集群构建下, 这些 key 必须与第一把声明 key 同 slot; 没有声明 key 时, 以脚本里第一次访问的 key 为槽位锚点;
  - `redis.call` 写操作收集在 `ScriptTransaction` 的 `WriteBatch` 中. 脚本成功时一次 `apply_writes` 提交; 集群下由凑批器按最多 512 笔拆成若干次 Raft. 抛出异常或超时时丢弃修改实现回滚. 声明 key 完全相同的排队脚本可以合成一次 `apply_writes`, 客户端在该次提交成功后才收到成功回复.
- **`BlockingRegistry` 唤醒机制**:
  - 当客户端执行 `BLPOP`, `BRPOP`, `BZPOPMIN` 等命令且目标 Key 为空时, 将连接注册至 `BlockingRegistry` 对应 Key 的等待通道中, 挂起当前 Task;
  - 任何写命令 (如 `LPUSH`, `RPUSH`, `ZADD`) 成功写入后, 必须调用 `BlockingRegistry::notify_keys(&[key])` 唤醒等待队列头部的连接.
- **持久化 Checkpoint 映射**:
  - `SAVE`: 同步执行 MemTable Flush 并调用底层 AiDb `DB::create_checkpoint()`, 成功后返回 `+OK`;
  - `BGSAVE`: 异步 `tokio::task::spawn_blocking` 执行 Checkpoint, 成功后更新 `LASTSAVE` 时间戳, 避免阻塞主处理流程;
  - `MemoryEngine` 模式下执行 `SAVE` / `BGSAVE` 必须明确报错 `ERR Persistence not supported on memory engine`.

---

## 扩展命令执行与阻塞唤醒流程

```mermaid
flowchart TD
    subgraph BlockingFlow [Blocking 阻塞与唤醒交互]
        ClientA[Client A: BLPOP key 10] --> CheckEmpty{key 存在且非空?}
        CheckEmpty -->|否| RegWait[注册至 BlockingRegistry 并在 oneshot 挂起]
        ClientB[Client B: LPUSH key "val"] --> WriteStore[写入底层存储]
        WriteStore --> Notify[BlockingRegistry::notify_keys("key")]
        Notify --> RegWait
        RegWait --> Wakeup[Client A 唤醒并消费元素]
        Wakeup --> OutA[返回 [key, val]]
    end

    subgraph LuaFlow [Lua 脚本原子执行流程]
        EvalReq[EVAL script 2 k1 k2 arg1] --> LockKeys[KeyLock 字典序锁定 [k1, k2] 30s 超时]
        LockKeys --> Sandbox[Lua 5.4 沙箱加载脚本与参数]
        Sandbox --> ExecLoop[执行 Lua 字节码]
        ExecLoop -->|redis.call| TxBatch[ScriptTransaction 收集 WriteBatch]
        ExecLoop -->|执行成功| Commit[原子 Commit 至 KvStorage 并释放锁]
        ExecLoop -->|执行失败 / 超时| Rollback[丢弃 WriteBatch 释放锁并报错]
    end
```

---

## 核心扩展命令一览

完整命令名见 [compatibility.md](../compatibility.md). 下表按分组列出 registry 内已实现顶层命令; 子命令 (如 `SCRIPT LOAD`, `CONFIG GET`) 见各模块实现, 不在 registry 单独注册.

| 命令分组 | 已实现顶层命令 (registry) |
| :--- | :--- |
| **JSON** | `JSON.SET`, `JSON.GET`, `JSON.DEL`, `JSON.TYPE`, `JSON.NUMINCRBY`, `JSON.STRLEN`, `JSON.ARRAPPEND`, `JSON.ARRLEN`, `JSON.OBJLEN`, `JSON.UPDATE`, `JSON.MGET`, `JSON.MSET` |
| **Scripting** | `EVAL`, `EVALSHA`, `SCRIPT` |
| **Blocking** | `BLPOP`, `BRPOP`, `BLMOVE`, `BZPOPMIN`, `BZPOPMAX` |
| **Cluster & Sync** | `MIGRATE`, `RESTORE`, `DUMP` |
| **Server & Admin** | `INFO`, `TIME`, `CONFIG`, `OBJECT`, `CLIENT`, `LATENCY`, `SLOWLOG`, `COMMAND`, `SAVE`, `BGSAVE`, `LASTSAVE`, `SHUTDOWN` |
