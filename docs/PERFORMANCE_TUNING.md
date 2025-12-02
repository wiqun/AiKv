# AiKv 性能调优指南

## 概述

本文档提供了 AiKv 性能调优的最佳实践和建议。通过合理的配置和优化，可以最大化 AiKv 的吞吐量并降低延迟。

## 性能基准

### 参考性能数据

在标准硬件上（4 核 CPU，16GB 内存，SSD）的性能基准：

| 操作 | 内存存储 | AiDb 存储 |
|------|----------|-----------|
| SET | ~80,000 ops/s | ~60,000 ops/s |
| GET | ~100,000 ops/s | ~80,000 ops/s |
| MSET (10 keys) | ~15,000 ops/s | ~10,000 ops/s |
| MGET (10 keys) | ~25,000 ops/s | ~20,000 ops/s |
| LPUSH | ~75,000 ops/s | ~55,000 ops/s |
| HSET | ~70,000 ops/s | ~50,000 ops/s |

### 延迟分布

| 百分位 | 目标值 |
|--------|--------|
| P50 | < 1ms |
| P99 | < 5ms |
| P99.9 | < 10ms |

## 系统层面优化

### 1. 操作系统配置

#### 文件描述符限制

```bash
# 查看当前限制
ulimit -n

# 临时修改
ulimit -n 65535

# 永久修改 (/etc/security/limits.conf)
* soft nofile 65535
* hard nofile 65535
```

#### TCP 参数优化

```bash
# 启用 TCP 重用
sysctl -w net.ipv4.tcp_tw_reuse=1

# 增加最大连接队列
sysctl -w net.core.somaxconn=65535

# 增加 SYN 队列长度
sysctl -w net.ipv4.tcp_max_syn_backlog=65535

# TCP 缓冲区大小
sysctl -w net.core.rmem_max=16777216
sysctl -w net.core.wmem_max=16777216

# 永久生效 (/etc/sysctl.conf)
echo "net.ipv4.tcp_tw_reuse=1" >> /etc/sysctl.conf
echo "net.core.somaxconn=65535" >> /etc/sysctl.conf
sysctl -p
```

#### 内存配置

```bash
# 禁用透明大页（推荐）
echo never > /sys/kernel/mm/transparent_hugepage/enabled

# 调整虚拟内存参数
sysctl -w vm.overcommit_memory=1
```

### 2. 硬件选择

#### CPU
- **推荐**: 高主频 CPU，现代多核处理器
- **核心数**: 根据并发连接数选择，一般 4-8 核足够
- **注意**: AiKv 单节点主要依赖单核性能

#### 内存
- **推荐**: DDR4 或更高
- **容量**: 至少为数据集大小的 2 倍
- **ECC**: 生产环境推荐使用 ECC 内存

#### 存储
- **强烈推荐**: NVMe SSD
- **IOPS**: 至少 10,000 IOPS
- **注意**: HDD 会严重影响 AiDb 持久化性能

#### 网络
- **推荐**: 千兆或万兆网卡
- **延迟**: 局域网延迟 < 1ms

## 应用层面优化

### 1. 存储引擎选择

| 场景 | 推荐引擎 | 原因 |
|------|----------|------|
| 纯缓存 | memory | 最高性能，无持久化开销 |
| 生产数据 | aidb | 持久化保证，性能优秀 |
| 开发测试 | memory | 快速启动，无磁盘占用 |
| 集群部署 | aidb | 数据安全，支持恢复 |

### 2. 连接优化

#### 使用连接池

```python
# Python 示例
import redis

pool = redis.ConnectionPool(
    host='localhost',
    port=6379,
    max_connections=100,
    socket_timeout=5,
    socket_connect_timeout=5
)
r = redis.Redis(connection_pool=pool)
```

```java
// Java (Jedis) 示例
JedisPoolConfig config = new JedisPoolConfig();
config.setMaxTotal(100);
config.setMaxIdle(20);
config.setMinIdle(5);
config.setTestOnBorrow(true);

JedisPool pool = new JedisPool(config, "localhost", 6379);
```

#### 连接数建议

| 应用规模 | 推荐连接池大小 |
|----------|----------------|
| 小型 | 10-50 |
| 中型 | 50-200 |
| 大型 | 200-1000 |

### 3. 命令优化

#### 使用批量命令

```bash
# 避免多次往返
# 低效方式
SET key1 value1
SET key2 value2
SET key3 value3

# 高效方式
MSET key1 value1 key2 value2 key3 value3
```

#### 使用管道 (Pipelining)

```python
# Python 示例
pipe = r.pipeline()
for i in range(1000):
    pipe.set(f'key{i}', f'value{i}')
pipe.execute()  # 一次网络往返执行所有命令
```

#### 避免大 Key

| 数据类型 | 建议大小限制 |
|----------|--------------|
| String | < 1MB |
| List | < 10,000 元素 |
| Hash | < 10,000 字段 |
| Set | < 10,000 成员 |
| ZSet | < 10,000 成员 |

#### 合理使用 KEYS 命令

```bash
# 避免在生产环境使用 KEYS *
# 使用 SCAN 代替
SCAN 0 MATCH pattern:* COUNT 100
```

### 4. 数据结构优化

#### String vs Hash

```bash
# 存储用户信息

# 方式 1: 多个 String（键数量多）
SET user:1:name "Alice"
SET user:1:email "alice@example.com"
SET user:1:age "30"

# 方式 2: Hash（推荐，减少键数量）
HSET user:1 name "Alice" email "alice@example.com" age "30"
```

#### 使用整数 ID

```bash
# 使用整数 ID 而非 UUID
# 好: user:1000
# 避免: user:550e8400-e29b-41d4-a716-446655440000
```

### 5. TTL 策略

#### 合理设置过期时间

```bash
# 避免大量 Key 同时过期
# 添加随机偏移
SET session:1 value1 EX 3600        # 固定 1 小时
SET session:2 value2 EX 3660        # 1 小时 + 1 分钟
SET session:3 value3 EX 3720        # 1 小时 + 2 分钟
```

#### 惰性删除 vs 定期删除

AiKv 使用混合策略：
- **惰性删除**: 访问时检查并删除过期键
- **定期清理**: 后台周期性清理过期键

## 监控和诊断

### 1. 慢查询日志

```bash
# 配置慢查询阈值（微秒）
CONFIG SET slowlog-log-slower-than 10000

# 设置慢查询日志最大长度
CONFIG SET slowlog-max-len 128

# 查看慢查询日志
SLOWLOG GET 10

# 获取慢查询日志长度
SLOWLOG LEN

# 重置慢查询日志
SLOWLOG RESET
```

### 2. INFO 命令

```bash
# 查看服务器信息
INFO

# 查看特定部分
INFO server
INFO clients
INFO memory
INFO stats
INFO replication
INFO cpu
INFO cluster
INFO keyspace
```

### 3. Prometheus 指标

关键指标监控：

```prometheus
# 命令延迟
aikv_commands_duration_avg_us

# 每秒操作数
aikv_ops_per_second

# 连接数
aikv_connected_clients

# 内存使用
aikv_used_memory_bytes

# 缓存命中率
aikv_keyspace_hits_total / (aikv_keyspace_hits_total + aikv_keyspace_misses_total)
```

### 4. 性能测试工具

```bash
# redis-benchmark 基础测试
redis-benchmark -h 127.0.0.1 -p 6379 -t set,get -n 100000 -q

# 并发测试
redis-benchmark -h 127.0.0.1 -p 6379 -c 50 -t set,get -n 100000 -q

# 管道测试
redis-benchmark -h 127.0.0.1 -p 6379 -P 16 -t set,get -n 100000 -q

# 大 value 测试
redis-benchmark -h 127.0.0.1 -p 6379 -d 1024 -t set,get -n 100000 -q
```

## 集群优化

### 1. 槽位分布

```bash
# 检查槽位分布
CLUSTER SLOTS

# 确保槽位均匀分布
# 每个主节点应该持有约 16384/N 个槽位
```

### 2. 使用哈希标签

```bash
# 确保相关 Key 在同一槽位
SET {user:1000}:name "Alice"
SET {user:1000}:email "alice@example.com"
SET {user:1000}:profile "..."
```

### 3. 避免跨槽操作

```bash
# 以下命令要求所有 Key 在同一槽位
MGET key1 key2 key3

# 使用哈希标签确保同槽
MGET {app}:key1 {app}:key2 {app}:key3
```

## 常见性能问题

### 1. 高延迟

**可能原因**:
- 网络延迟
- 命令复杂度高（如 KEYS *）
- 大 Value 操作
- 磁盘 I/O 瓶颈

**解决方案**:
- 检查网络连接
- 使用 SCAN 代替 KEYS
- 拆分大 Value
- 使用 SSD

### 2. 内存不足

**可能原因**:
- 数据量超过内存
- 内存碎片
- 大量过期键未清理

**解决方案**:
- 增加内存或使用集群
- 定期重启清理碎片
- 调整过期策略

### 3. CPU 使用率高

**可能原因**:
- 复杂命令（SORT, 大范围 ZRANGE）
- 频繁 JSON 解析
- Lua 脚本执行

**解决方案**:
- 优化查询模式
- 预计算复杂结果
- 优化 Lua 脚本

### 4. 连接被拒绝

**可能原因**:
- 达到最大连接数
- 文件描述符限制
- TCP 队列满

**解决方案**:
- 增加 max_connections
- 增加 ulimit
- 使用连接池

## 配置示例

### 高性能配置

```toml
[server]
host = "0.0.0.0"
port = 6379

[storage]
engine = "memory"
databases = 16

[logging]
level = "warn"

[slowlog]
log-slower-than = 10000
max-len = 128
```

### 生产环境配置

```toml
[server]
host = "0.0.0.0"
port = 6379

[storage]
engine = "aidb"
data_dir = "/data/aikv"
databases = 16

[logging]
level = "info"

[slowlog]
log-slower-than = 10000
max-len = 256
```

## 总结

1. **选择合适的存储引擎**: 根据场景选择 memory 或 aidb
2. **使用连接池**: 避免频繁创建销毁连接
3. **使用批量命令**: MSET/MGET 代替多次 SET/GET
4. **使用管道**: 减少网络往返
5. **避免大 Key**: 拆分大 Value 和大集合
6. **监控慢查询**: 定期检查 SLOWLOG
7. **合理设置 TTL**: 避免同时过期
8. **使用 SSD**: AiDb 存储必须使用 SSD

---

**最后更新**: 2025-12-02  
**版本**: v0.1.0  
**维护者**: @Genuineh, @copilot
