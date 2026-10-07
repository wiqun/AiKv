# AiKv 领域词汇

## 协议层

**RESP**: Redis Serialization Protocol, AiKv 实现的 Redis 线缆协议, 同时支持 RESP2 和 RESP3.

**BulkString**: RESP 中的二进制安全字符串编码, 不做 UTF-8 假设, 可传输任意字节序列.

## 路由层

**CommandRouter**: 根据命令类型 (读/写/管理等) 将请求分派到对应处理器.

**ClusterRouter**: 在集群模式下, 根据 key 的 CRC16 哈希计算 slot, 将请求路由到正确的节点或返回 MOVED 错误.

## 存储层

**KvStorage**: 存储抽象接口, 统一内存和持久化两种后端.

**MemoryEngine**: 纯内存的 KV 存储实现, 用于开发测试或不需要持久化的场景.

**AiDbEngine**: 通过 aidb crate 实现的持久化存储, 生产推荐.

**StoredValue**: AiKv 内部对 Redis 值的编码表示, 包含类型标记和序列化后的字节.

**ValueType**: Redis 数据类型的枚举 (String, List, Set, ZSet, Hash 等).

**ExpiresCounters**: 各逻辑 DB 的进程内 TTL 写入计数. 成功写入 `expires_at = Some` 时加 1, 包括没有读过旧值的覆盖写. 这次调用已经看到旧 TTL, 或惰性过期删掉带 TTL 的值时饱和减. compaction 不改它. 重启从 0 开始, 不扫库补存量.

**estimate_entry_count**: 引擎条目估值, 等于已打开 SSTable 的 `num_entries` 与 MemTable `entry_count` 之和. 集群实现把本地各 group 的估值相加, 不扫描. `MemoryEngine` 没有 SST, 估值是全部逻辑 DB 上未过期用户 key 之和.

**DBSIZE**: 返回引擎条目估值, 不随 `SELECT` 改变. `SET NX` / `XX`, `DEL` 返回值和 `EXISTS` 仍是精确命令结果.

**键过期语义**: `TtlExpireFilter` 在 compaction 时丢弃过期值, 且不改 `ExpiresCounters`. `KvStorage::delete` 用一次存在性读取决定返回个数.

## 集群层

**Slot**: 16384 个槽位之一, 通过 CRC16(key) % 16384 计算; 与 Redis Cluster 完全兼容.

**MOVED/ASK**: 集群重定向错误; 由客户端处理 (`redis-cli -c`), 服务端不做透明转发.

**Slot 迁移**: 将 slot 从源节点迁移到目标节点的在线过程, 不中断服务.

**Gossip**: 每节点定期发送的轻量拓扑信息, 用于刷新 leader 路由缓存和集群指标; 故障判定走 MetaRaft, 非 SWIM 式 gossip.

**MetaRaft**: 管理集群元数据的 Raft 组 (控制面, 低频).

**MultiRaft**: 按 slot Group 划分的多个数据面 Raft 组 (数据面, 高吞吐).

## 写入批处理

**Batcher**: 将多个写入操作合并为一批, 减少 Raft 共识次数.

**EagerFlush**: 批处理中的阈值, 当累计操作数达到此值时不等 timeout 立即 propose.

## 可观测性

**OTLP**: OpenTelemetry Protocol, AiKv 的生产指标出口; 指标前缀为 `aikv_*`.

**INFO**: Redis 兼容的 INFO 命令输出. 默认 Keyspace 只有一行 `keys_estimate`, 再加每个逻辑 DB 的 `dbN:expires`. 不输出 `keys=` 和 `avg_ttl`. `sample_avg_ttl` 是唯一会扫描并计算平均剩余 TTL 的路径, 默认 INFO 不调用它.
