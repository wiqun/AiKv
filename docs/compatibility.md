---
name: compatibility
description: AiKv Redis 兼容范围 — RESP2/RESP3 协议边界, 已实现命令矩阵 (与 `all_commands()` 自动核对), 未支持能力与语义限制.
---

# AiKv Redis 兼容矩阵

本文是 AiKv 对外 Redis 兼容范围的**唯一权威清单**. 命令注册表 `all_commands()` (`src/command/registry.rs`) 与本文 marker 区间内的命令列表由 `tests/compatibility_docs.rs` 自动核对, 新增或删除命令时必须同步更新.

模块文档 ([04-commands-core](modules/04-commands-core.md), [05-commands-extended](modules/05-commands-extended.md), [06-cluster](modules/06-cluster.md)) 仅描述实现细节, 不再维护独立命令枚举.

---

## 协议与传输

| 能力 | 状态 | 说明 |
| :--- | :--- | :--- |
| RESP2 array framing | 支持 | 标准 `*`-prefixed 数组命令帧 |
| RESP3 array framing | 支持 | `HELLO 3` 协商后使用 RESP3 回复类型 |
| Pipeline | 支持 | 单连接多命令批量发送 |
| Telnet inline command | **不支持** | 非数组文本行 (如 `PING\r\n`) 不被接受 |
| AUTH / ACL | **未实现** | 无密码与 ACL 子系统 |
| TLS | **未实现** | 明文 TCP only |
| Streams | **未实现** | `XADD`, `XREAD` 等 |
| Pub/Sub | **未实现** | `SUBSCRIBE`, `PUBLISH` 等 |

---

## 语义与边界

- 下表列出的命令表示 AiKv **已实现路由与处理器**; 不代表 Redis 全部 option, 子命令组合与 edge case 完全等价.
- 部分命令仅支持常用参数组合; 未文档化的 Redis 扩展 flag 可能返回 `ERR` 或行为与 Redis 8.8 不一致.
- 集群模式需启用 `cluster` feature; `CLUSTER`, `READONLY`, `READWRITE`, `ASKING` 仅在集群构建中注册.

---

## 已实现命令 (与 registry 同步)

<!-- command-list:start -->

### String

`GET` `SET` `MGET` `MSET` `DEL` `EXISTS` `STRLEN` `GETRANGE` `SETRANGE` `SETBIT` `GETBIT` `BITCOUNT` `BITFIELD` `BITFIELD_RO` `APPEND` `INCR` `DECR` `INCRBY` `DECRBY` `INCRBYFLOAT` `GETDEL` `GETEX` `SETNX` `SETEX` `PSETEX`

### JSON

`JSON.GET` `JSON.MGET` `JSON.SET` `JSON.DEL` `JSON.TYPE` `JSON.STRLEN` `JSON.ARRLEN` `JSON.OBJLEN` `JSON.NUMINCRBY` `JSON.ARRAPPEND` `JSON.UPDATE` `JSON.MSET`

### Lua

`EVAL` `EVALSHA` `SCRIPT`

### Hash

`HSET` `HMSET` `HGET` `HDEL` `HEXISTS` `HLEN` `HKEYS` `HVALS` `HGETALL` `HMGET` `HSETNX` `HINCRBY` `HINCRBYFLOAT` `HSCAN`

### List

`LPUSH` `RPUSH` `LPOP` `RPOP` `LLEN` `LRANGE` `LINDEX` `LSET` `LREM` `LTRIM` `LINSERT` `LMOVE` `LPOS` `BLPOP` `BRPOP` `BLMOVE`

### Set

`SADD` `SREM` `SISMEMBER` `SMEMBERS` `SCARD` `SPOP` `SRANDMEMBER` `SUNION` `SINTER` `SDIFF` `SUNIONSTORE` `SINTERSTORE` `SDIFFSTORE` `SMOVE` `SSCAN`

### Sorted Set

`ZADD` `ZREM` `ZSCORE` `ZRANK` `ZREVRANK` `ZRANGE` `ZREVRANGE` `ZRANGEBYSCORE` `ZREVRANGEBYSCORE` `ZCARD` `ZCOUNT` `ZINCRBY` `ZSCAN` `ZPOPMIN` `ZPOPMAX` `BZPOPMIN` `BZPOPMAX` `ZRANGEBYLEX` `ZREVRANGEBYLEX` `ZLEXCOUNT` `ZINTER` `ZUNION` `ZDIFF`

### Database

`SELECT` `DBSIZE` `FLUSHDB` `FLUSHALL` `SWAPDB` `MOVE`

### Key

`EXPIRE` `EXPIREAT` `PEXPIRE` `PEXPIREAT` `TTL` `PTTL` `PERSIST` `KEYS` `SCAN` `RANDOMKEY` `RENAME` `RENAMENX` `TYPE` `COPY` `EXPIRETIME` `PEXPIRETIME` `DUMP` `RESTORE` `MIGRATE`

### Server

`INFO` `TIME` `CONFIG` `OBJECT` `CLIENT` `LATENCY` `SLOWLOG` `COMMAND`

### Connection

`PING` `ECHO` `HELLO` `QUIT` `MONITOR`

### Transaction

`MULTI` `EXEC` `DISCARD` `WATCH` `UNWATCH` `ATOM.MULTI` `ATOM.EXEC` `ATOM.DISCARD` `ATOM.WATCH` `ATOM.UNWATCH`

### Persistence

`SAVE` `BGSAVE` `LASTSAVE` `SHUTDOWN`

### Cluster (feature cluster)

`CLUSTER` `READONLY` `READWRITE` `ASKING`

<!-- command-list:end -->

---

## CLUSTER 子命令

`CLUSTER` 在 registry 中注册为单条顶层命令; 下列为 `dispatch_cluster` 可识别的子命令及其支持状态 (不在 `all_commands()` 矩阵内单独列出).

| 子命令 | 状态 | 说明 |
| :--- | :--- | :--- |
| `CLUSTER NODES` | 支持 | 节点拓扑 |
| `CLUSTER SLOTS` | 支持 | 槽位路由表 |
| `CLUSTER SHARDS` | 支持 | Redis 7+ 分片视图 |
| `CLUSTER INFO` | 支持 | 集群状态摘要 |
| `CLUSTER MYID` | 支持 | 本节点 ID |
| `CLUSTER MYSHARDID` | 支持 | 返回本节点所属数据组的 40 字符 ID; 未加入数据组时返回 `CLUSTERDOWN` |
| `CLUSTER KEYSLOT` | 支持 | Key → slot |
| `CLUSTER COUNTKEYSINSLOT` | 支持 | 扫描本节点的所有 MultiRaft 数据组, 统计映射到指定 Slot 的 Key 数量 |
| `CLUSTER GETKEYSINSLOT` | 支持 | 扫描本节点的所有 MultiRaft 数据组, 返回指定 Slot 的最多 `count` 个 Key |
| `CLUSTER MEET` | 支持 | 加入节点 |
| `CLUSTER FORGET` | 支持 | 移除节点 |
| `CLUSTER ADDSLOTS` | 支持 | 分配槽位 |
| `CLUSTER DELSLOTS` | 支持 | 回收槽位 |
| `CLUSTER SETSLOT` | 支持 | 迁移 / 稳定 / 取消 |
| `CLUSTER FAILOVER` | 支持 | 手动 failover |
| `CLUSTER REPLICATE` | 支持 | 配置副本 |
| `CLUSTER REPLICAS` | 支持 | 列出副本 |
| `CLUSTER SAVECONFIG` | 支持 | 持久化 nodes.conf |
| `CLUSTER BUMPEPOCH` | 支持 | 递增 config epoch |
| `CLUSTER SET-CONFIG-EPOCH` | 部分支持 | 当前 handler 忽略 epoch 参数并固定返回 `OK`; 不修改 config epoch, 也不实现 Redis 的完整参数校验与设置语义 |
| `CLUSTER COUNT-FAILURE-REPORTS` | 部分支持 | 当前 handler 忽略节点参数并固定返回整数 `0`; 不读取或统计 failure reports |
| `CLUSTER REBALANCE` | 支持 | 自动均衡 |
| `CLUSTER CREATEGROUP` | 支持 | AiKv 扩展: 创建数据组 |
| `CLUSTER ADD_REPLICA` | 支持 | AiKv 扩展: 添加副本 |
| `CLUSTER DEL_REPLICA` | 支持 | AiKv 扩展: 移除副本 |
| `CLUSTER GROUPSTATUS` | 支持 | AiKv 扩展: 组状态 |
| `CLUSTER RESET` | **不支持** | MetaRaft 保护元数据; 需停服清 `data_dir` 重搭 |

---

## 命令对齐门禁

`tests/command_alignment.rs` 用三份清单做差集, 对不上就失败. 运行时要加 `--features cluster`.

1. Redis 8.8.3 的顶层命令 (`tests/fixtures/redis_8_8_3_commands.txt`, 来自该 tag 的 `src/commands/*.json`) 必须落在注册表或 `redis_unsupported.txt` 里. 注册表里 Redis 没有的名字必须落在 `aikv_extensions.txt`.
2. 注册表中 Redis 允许在脚本里调用、AiKv 的 `redis.call` 白名单却没有的命令, 必须和 `script_command_gaps.txt` 完全一致. Redis 标记为 `NOSCRIPT` 的命令不进这份缺口, 只要求脚本白名单同样不收它们.
3. 业务实际发出的命令没有豁免. `business_script_commands.txt` 里 Redis 允许进脚本的名字必须在白名单中; `business_direct_commands.txt` 里的名字必须在注册表中. 扫描噪声 (例如解析器单测里的 `INC`) 记在 `business_command_noise.txt`.

刷新清单: `python3 aikv/tools/refresh_command_alignment.py`. 业务脚本或注册表有增减时要重跑, 否则这道测试失败.

这层只核对命令名字和脚本可否调用. 同一条命令的回复是否与 Redis 逐字节一致, 不在这道测试里.

## 相关文档

- 协议细节: [01-protocol.md](modules/01-protocol.md)
- 核心命令实现: [04-commands-core.md](modules/04-commands-core.md)
- 扩展命令实现: [05-commands-extended.md](modules/05-commands-extended.md)
- 集群路由: [06-cluster.md](modules/06-cluster.md)
