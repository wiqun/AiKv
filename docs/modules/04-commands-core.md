---
name: aikv-commands-core
description: AiKv 核心 Redis 命令 — String/Hash/List/Set/ZSet/Key/Database 处理器、CommandRouter 分发中枢、CommandRegistry 元数据表与 KeyLock 并发锁. 修改 src/command/{string,hash,list,set,zset/,key,database,registry,router/} 时查阅.
---

# AiKv Commands Core (核心命令层)

## 何时读本文

- 修改 `src/command/{router/, registry.rs, string.rs, hash.rs, list.rs, set.rs, zset/, key.rs, database.rs, scan_util.rs}` 源码;
- 新增或修改 String, Hash, List, Set, Sorted Set, Key 或 Database 相关 Redis 命令;
- 排查 WRONGTYPE 报错、命令注册表与路由分发不一致、`COMMAND GETKEYS` 提取错误、并发写数据竞争与死锁;
- **不覆盖**: `KvStorage` 接口与底层 Subkey 编码规则 → [storage.md](03-storage.md);
- **不覆盖**: JSON / Lua / 阻塞队列 / MIGRATE / 持久化与服务端管理命令 → [commands-extended.md](05-commands-extended.md);
- **不覆盖**: 集群 MOVED/ASK 重定向判定与 CLUSTER 子命令 → [cluster.md](06-cluster.md).

> **命令清单**: 已实现命令以 [compatibility.md](../compatibility.md) 为准 (与 `all_commands()` 自动核对); 下文仅保留数据结构分组索引.

---

## 代码地图

| 文件路径 | 模块核心职责 | 公共接口与核心入口 |
| :--- | :--- | :--- |
| [`src/command/mod.rs`](../../src/command/mod.rs) | 命令模块根; 子模块组织与对外导出 | `CommandRouter`, `CommandRegistry` |
| [`src/command/router/`](../../src/command/router/) | `CommandRouter` 分发中枢、4096 桶 `KeyLock` 写锁与 metrics 埋点 | `CommandRouter::execute_with_client`, `execute_inner` |
| [`src/command/registry.rs`](../../src/command/registry.rs) | Redis 命令元数据表 (`COMMAND_TABLE`)、`CommandFlags` 与参数规则 | `CommandRegistry::lookup`, `CommandFlags` |
| [`src/command/string.rs`](../../src/command/string.rs) | String 系列命令 (GET, SET, MGET, MSET, INCR, INCRBYFLOAT, GETRANGE 等) | `string::dispatch` |
| [`src/command/hash.rs`](../../src/command/hash.rs) | Hash 系列命令 (HGET, HSET, HDEL, HGETALL, HINCRBY, HEXISTS 等) | `hash::dispatch` |
| [`src/command/list.rs`](../../src/command/list.rs) | List 系列命令 (LPUSH, RPUSH, LPOP, RPOP, LRANGE, LLEN, LINDEX 等) | `list::dispatch` |
| [`src/command/set.rs`](../../src/command/set.rs) | Set 系列命令 (SADD, SREM, SMEMBERS, SISMEMBER, SCARD, SINTER 等) | `set::dispatch` |
| [`src/command/zset/`](../../src/command/zset/) | Sorted Set 系列命令 (ZADD, ZREM, ZRANGE, ZSCORE, ZCARD, ZINCRBY 等); 子键布局路径见 `layout.rs` | `zset::dispatch` |
| [`src/command/script/transaction/zset_view.rs`](../../src/command/script/transaction/zset_view.rs) | Lua 脚本内 ZSET 子键视图 (不整表展开) | `zset_view` |
| [`src/command/key.rs`](../../src/command/key.rs) | Key 通用命令 (DEL, EXISTS, TYPE, EXPIRE, PEXPIRE, TTL, PTTL, PERSIST) | `key::dispatch` |
| [`src/command/database.rs`](../../src/command/database.rs) | DB 管理命令 (SELECT, FLUSHDB, FLUSHALL, DBSIZE, SWAPDB) | `database::dispatch` |
| [`src/command/scan_util.rs`](../../src/command/scan_util.rs) | SCAN / HSCAN / SSCAN / ZSCAN 游标编码与前缀迭代器辅助 | `encode_cursor`, `decode_cursor` |

---

## 关键 Invariants (勿破坏规则)

- **`KeyLock` 死锁防护与并发隔离**:
  - `KeyLock` 内部采用 **4096** 个独立分桶互斥锁 (`tokio::sync::Mutex<()>`);
  - **单 Key 写**: `KeyLock::lock(key)` 加锁 (如 SET NX/XX, INCR, HSET, LPUSH 等);
  - **双 Key 命令**: `KeyLock::lock_two(k1, k2)` 按照 Key 字节序比较升序获取锁 (如 RENAME, COPY, LMOVE, SMOVE);
  - **多 Key / Lua 事务**: `KeyLock::lock_keys_sorted(keys)` 自动去重并按字典序升序排序加锁, 彻底杜绝并发死锁.
- **双维护一致性约束 (Registry ↔ Router)**:
  - 新增命令必须**同时更新** `registry.rs` 中的 `COMMAND_TABLE` 和 `router/mod.rs` 中的 `execute_inner` (或子领域 dispatch match);
  - 严禁在 `registry.rs` 中声明但在 `router/` 中遗漏分发, 避免出现 `COMMAND` 命令可查但执行报未知命令的 Bug.
- **WRONGTYPE 强制校验**:
  - 对已存在的 Key 执行不匹配的数据结构操作时, 必须立即返回 `-WRONGTYPE Operation against a key holding the wrong kind of value\r\n`;
  - 严禁静默覆盖已有不同类型的 Key (除非是 `SET` 强制覆盖).
- **空容器自动删除**:
  - 当通过 `HDEL`, `LPOP`, `SREM`, `ZREM` 等操作移除最后一个元素导致集合为空时, 必须同时物理删除其元数据 Key, 保持与 Redis 规范一致 (即 `EXISTS key` 返回 0).

---

## 命令分发与加锁执行流

```mermaid
sequenceDiagram
    participant Conn as Connection
    participant Router as CommandRouter
    participant Cluster as ClusterRouter
    participant KL as KeyLock (4096 桶)
    participant Handler as 业务 Handler (如 string.rs)
    participant Storage as KvStorage

    Conn->>Router: execute_with_client(cmd, args)

    opt 集群模式 feature = "cluster"
        Router->>Cluster: cluster_route(cmd, args)
        alt 跨槽位错误
            Cluster-->>Conn: -CROSSSLOT Keys in request don't hash to the same slot
        else 槽位重定向
            Cluster-->>Conn: -MOVED / -ASK
        end
    end

    Router->>Router: execute_inner(cmd, args)

    opt 属于写入或多 Key 命令
        Router->>KL: 按 Key 字典序升序获取分桶锁
        KL-->>Router: LockGuard
    end

    Router->>Handler: dispatch(args, storage)
    Handler->>Storage: get / put / delete
    Storage-->>Handler: 返回数据或状态
    Handler-->>Router: 构造 RespValue 结果

    opt 释放锁
        Router->>KL: drop(LockGuard)
    end

    Router->>Router: 记录 commandstats / errorstats 指标
    Router-->>Conn: RespValue 响应
```

---

## 核心数据结构支持列表

完整命令名见 [compatibility.md](../compatibility.md). 下表按数据结构分组, 仅列 registry 内已实现命令.

| 数据结构 | 已实现命令 (registry) |
| :--- | :--- |
| **String** | `GET`, `SET`, `MGET`, `MSET`, `DEL`, `EXISTS`, `STRLEN`, `GETRANGE`, `SETRANGE`, `SETBIT`, `GETBIT`, `APPEND`, `INCR`, `DECR`, `INCRBY`, `DECRBY`, `INCRBYFLOAT`, `GETDEL`, `GETEX`, `SETNX`, `SETEX`, `PSETEX` |
| **Hash** | `HSET`, `HMSET`, `HGET`, `HDEL`, `HEXISTS`, `HLEN`, `HKEYS`, `HVALS`, `HGETALL`, `HMGET`, `HSETNX`, `HINCRBY`, `HINCRBYFLOAT`, `HSCAN` |
| **List** | `LPUSH`, `RPUSH`, `LPOP`, `RPOP`, `LLEN`, `LRANGE`, `LINDEX`, `LSET`, `LREM`, `LTRIM`, `LINSERT`, `LMOVE`, `LPOS`, `BLPOP`, `BRPOP`, `BLMOVE` |
| **Set** | `SADD`, `SREM`, `SISMEMBER`, `SMEMBERS`, `SCARD`, `SPOP`, `SRANDMEMBER`, `SUNION`, `SINTER`, `SDIFF`, `SUNIONSTORE`, `SINTERSTORE`, `SDIFFSTORE`, `SMOVE`, `SSCAN` |
| **ZSet** | `ZADD`, `ZREM`, `ZSCORE`, `ZRANK`, `ZREVRANK`, `ZRANGE`, `ZREVRANGE`, `ZRANGEBYSCORE`, `ZREVRANGEBYSCORE`, `ZCARD`, `ZCOUNT`, `ZINCRBY`, `ZSCAN`, `ZPOPMIN`, `ZPOPMAX`, `BZPOPMIN`, `BZPOPMAX`, `ZRANGEBYLEX`, `ZREVRANGEBYLEX`, `ZLEXCOUNT`, `ZINTER`, `ZUNION`, `ZDIFF` |
| **Key** | `DEL`, `EXISTS`, `TYPE`, `EXPIRE`, `PEXPIRE`, `EXPIREAT`, `PEXPIREAT`, `TTL`, `PTTL`, `PERSIST`, `EXPIRETIME`, `PEXPIRETIME`, `RENAME`, `RENAMENX`, `COPY`, `KEYS`, `SCAN`, `RANDOMKEY`, `DUMP`, `RESTORE`, `MIGRATE` |
| **DB** | `SELECT`, `FLUSHDB`, `FLUSHALL`, `DBSIZE`, `SWAPDB`, `MOVE` |

---

## 大集合 (Hash / Set) 键级语义

物理布局与 GC 见 [03-storage.md §3](03-storage.md). 本节只列命令层可观察行为.

| 操作 | 行为 |
| :--- | :--- |
| `DEL` / `UNLINK` | 大集合只删 header, O(1). 旧 subkey 由 compaction 回收 |
| 惰性过期 / compaction TTL | 只删 header |
| `RENAME` / `RENAMENX` / `COPY` | 目标获得新版本 subkey 集合; 目标原有内容被覆盖 |
| `MOVE` | 跨库迁移大集合, 目标库可读出全部元素 |
| `SWAPDB` | 两侧大集合整体交换, 非原子 (见 03-storage.md §3.4) |
| `DUMP` / `RESTORE` / `MIGRATE` | 序列化前展开为完整内联值, payload 格式不变 |
| `SUNIONSTORE` / `SINTERSTORE` / `SDIFFSTORE` | 目标已存在且不是 Set 时直接覆盖 (与 Redis 一致); 结果为空时删除目标, 返回 0 |
| `SMOVE` | 目标 Set 增长超过阈值时转为 subkey 布局 (经 `put_value`) |
| `OBJECT ENCODING` | 内联集合返回 `listpack`; subkey 布局的大集合返回 `hashtable` |
| `HSCAN` / `SSCAN` | 游标越界返回游标 0 与空页, 不 panic |
| `KEYS` / `SCAN` / `RANDOMKEY` | 只返回用户 key, 不返回 subkey |

## 有序集合 (ZSet) 键级语义

物理布局见 [03-storage.md §3](03-storage.md) ZSet 小节. 下表区分子键布局上的增量路径与整表路径.

| 命令 | 子键布局 (大集合) | 内联 (≤ 64) |
| :--- | :--- | :--- |
| `ZADD` / `ZREM` / `ZSCORE` / `ZCARD` | 只读写相关 `Z`/`R` 与 header, 不展开整表 | 整值读写 |
| `ZINCRBY` | 只读该成员旧分数, 增量写入 | 整值写回 |
| `ZPOPMIN` / `ZPOPMAX` (含 `BZPOP*`) | 只从 `R` 索引两端取成员并删除 | 整值 |
| `ZRANGE` / `ZREVRANGE` / `ZRANGEBYSCORE` / `ZREVRANGEBYSCORE` (含 `LIMIT`) | 按名次或分数区间前缀扫描, 只取窗口内成员 | 整表内存排序 |
| `ZCOUNT` | 分数区间扫描计数 (O(区间内成员数)) | 整表 |
| `ZRANK` / `ZREVRANK` / `ZRANGEBYLEX` / `ZREVRANGEBYLEX` / `ZLEXCOUNT` / `ZSCAN` / `ZINTER` / `ZUNION` / `ZDIFF` | 经 `get_expanded` 展开整表, O(N) | 整表 |
| `DUMP` / `RESTORE` / `MIGRATE` / `RENAME` / `COPY` / `MOVE` / `SWAPDB` | 序列化前展开为内联整值; 目标按阈值重新决定布局 | 同左 |
| `KEYS` | 只返回用户 key, 不返回 `Z`/`R` 子键 (有测试) | 同左 |
| `DBSIZE` | **已知问题**: 经 `estimate_entry_count` 计入物理条目, 子键布局的集合会被多计 (100 成员的 ZSET 计为 201, `adapter.rs` `len` → `aidb.rs` `estimate_entry_count`); Hash/Set 同机制, 推断同样受影响 (未单独测) |

脚本 (`EVAL`) 内的 ZSET 读写在子键布局上经 `zset_view` 直接访问存储: 写入累积为成员级 `ZSetDelta`, 同一 burst 内的后续脚本可见; 读取为存储区间与增量的有序合并. 内联 ZSET 仍在脚本缓冲中整表操作 (成员数 ≤ 64).
