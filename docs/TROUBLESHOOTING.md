# AiKv 故障排查指南

## 概述

本文档提供了 AiKv 常见问题的诊断和解决方案。遇到问题时，请按照本指南进行排查。

## 快速诊断流程

```
问题发生
    │
    ▼
┌───────────────┐
│ 服务是否运行？ │──否──► 查看启动问题
└───────┬───────┘
        │是
        ▼
┌───────────────┐
│ 能否连接？     │──否──► 查看连接问题
└───────┬───────┘
        │是
        ▼
┌───────────────┐
│ 命令是否执行？ │──否──► 查看命令问题
└───────┬───────┘
        │是
        ▼
┌───────────────┐
│ 性能是否正常？ │──否──► 查看性能问题
└───────┴───────┘
```

## 启动问题

### 1. 服务无法启动

#### 症状
```bash
$ ./target/release/aikv
Error: ...
```

#### 检查步骤

**1.1 检查端口占用**
```bash
# 检查 6379 端口
lsof -i :6379
netstat -tlnp | grep 6379

# 如果端口被占用，终止占用进程或更换端口
kill <PID>
# 或
./aikv --port 6380
```

**1.2 检查配置文件**
```bash
# 验证配置文件语法
cat config.toml

# 检查常见配置错误
# - 端口范围 1-65535
# - 数据目录是否存在
# - 存储引擎名称是否正确 (memory/aidb)
```

**1.3 检查数据目录权限**
```bash
# 确保数据目录存在且可写
ls -la ./data
mkdir -p ./data
chmod 755 ./data
```

**1.4 检查依赖库**
```bash
# 检查动态库依赖
ldd ./target/release/aikv

# 如果缺少库，安装依赖
# Ubuntu/Debian
apt-get install libc6

# CentOS/RHEL
yum install glibc
```

### 2. 启动后立即退出

#### 检查日志
```bash
# 查看日志输出
./aikv --config config.toml 2>&1 | head -100

# 使用更详细的日志级别
RUST_LOG=debug ./aikv --config config.toml
```

#### 常见原因

| 错误信息 | 原因 | 解决方案 |
|----------|------|----------|
| `Address already in use` | 端口被占用 | 更换端口或终止占用进程 |
| `Permission denied` | 权限不足 | 检查文件权限或使用 sudo |
| `No such file or directory` | 路径不存在 | 创建数据目录 |
| `Invalid configuration` | 配置错误 | 检查配置文件格式 |

## 连接问题

### 1. 连接被拒绝

#### 症状
```bash
$ redis-cli -h 127.0.0.1 -p 6379
Could not connect to Redis at 127.0.0.1:6379: Connection refused
```

#### 检查步骤

**1.1 确认服务运行**
```bash
ps aux | grep aikv
pgrep aikv
```

**1.2 确认监听地址**
```bash
# 检查服务实际监听的地址
netstat -tlnp | grep aikv
ss -tlnp | grep :6379

# 如果只监听 127.0.0.1，远程无法连接
# 修改配置:
# [server]
# host = "0.0.0.0"
```

**1.3 检查防火墙**
```bash
# Ubuntu/Debian (ufw)
sudo ufw status
sudo ufw allow 6379

# CentOS/RHEL (firewalld)
sudo firewall-cmd --list-all
sudo firewall-cmd --add-port=6379/tcp --permanent
sudo firewall-cmd --reload

# iptables
sudo iptables -L -n | grep 6379
sudo iptables -A INPUT -p tcp --dport 6379 -j ACCEPT
```

**1.4 检查 SELinux**
```bash
# 查看 SELinux 状态
getenforce

# 临时禁用
setenforce 0

# 永久配置
# 编辑 /etc/selinux/config
# SELINUX=permissive
```

### 2. 连接超时

#### 症状
```bash
$ redis-cli -h 192.168.1.100 -p 6379
Could not connect to Redis at 192.168.1.100:6379: Connection timed out
```

#### 检查步骤

**2.1 网络连通性**
```bash
# 测试网络连通
ping 192.168.1.100

# 测试端口连通
telnet 192.168.1.100 6379
nc -zv 192.168.1.100 6379
```

**2.2 路由问题**
```bash
# 检查路由
traceroute 192.168.1.100
```

### 3. 连接被重置

#### 症状
```bash
Error: Connection reset by peer
```

#### 可能原因
- 达到最大连接数限制
- 客户端超时
- 服务器异常退出

#### 解决方案
```bash
# 检查当前连接数
redis-cli CLIENT LIST | wc -l

# 检查服务器状态
redis-cli INFO clients
```

## 命令执行问题

### 1. 命令返回错误

#### WRONGTYPE 错误
```bash
127.0.0.1:6379> SET mykey "hello"
OK
127.0.0.1:6379> LPUSH mykey "world"
(error) WRONGTYPE Operation against a key holding the wrong kind of value
```

**解决方案**: 检查键的类型，使用正确的命令

```bash
127.0.0.1:6379> TYPE mykey
string
```

#### ERR 语法错误
```bash
127.0.0.1:6379> SET key
(error) ERR wrong number of arguments for 'set' command
```

**解决方案**: 检查命令语法，参考 [API 文档](API.md)

### 2. 命令返回 nil

#### 可能原因
- 键不存在
- 键已过期
- 访问的字段不存在

#### 检查步骤
```bash
# 检查键是否存在
EXISTS mykey

# 检查键的 TTL
TTL mykey

# 列出所有键（小数据量）
KEYS *
```

### 3. 集群重定向

#### 症状
```bash
127.0.0.1:6379> GET mykey
(error) MOVED 12539 192.168.1.102:6379
```

**解决方案**: 使用集群模式连接

```bash
redis-cli -c -h 127.0.0.1 -p 6379
```

## 性能问题

### 1. 响应延迟高

#### 诊断步骤

**1.1 检查慢查询日志**
```bash
# 查看慢查询
SLOWLOG GET 10

# 结果示例:
# 1) 1) (integer) 1           # 日志 ID
#    2) (integer) 1638360000  # 时间戳
#    3) (integer) 50000       # 执行时间（微秒）
#    4) 1) "KEYS"             # 命令
#       2) "*"
```

**1.2 检查命令统计**
```bash
INFO commandstats
```

**1.3 检查网络延迟**
```bash
# 测试 PING 延迟
redis-benchmark -h 127.0.0.1 -p 6379 -t ping -n 10000 -q
```

#### 常见慢命令及优化

| 慢命令 | 优化建议 |
|--------|----------|
| `KEYS *` | 使用 `SCAN` 替代 |
| `SMEMBERS` (大集合) | 使用 `SSCAN` |
| `HGETALL` (大哈希) | 使用 `HSCAN` |
| `SORT` | 减少排序数据量 |
| `LRANGE 0 -1` | 限制范围 |

### 2. 内存使用过高

#### 诊断步骤

**2.1 检查内存使用**
```bash
INFO memory
```

**2.2 查找大 Key**
```bash
# 使用 redis-cli --bigkeys
redis-cli --bigkeys

# 或使用 DEBUG OBJECT
DEBUG OBJECT mykey
```

**2.3 检查键数量**
```bash
INFO keyspace
DBSIZE
```

#### 解决方案
- 清理无用数据
- 设置合理的 TTL
- 使用更紧凑的数据结构
- 考虑集群分片

### 3. CPU 使用率高

#### 诊断步骤

**3.1 查看进程状态**
```bash
top -p $(pgrep aikv)
htop
```

**3.2 检查命令频率**
```bash
# 使用 MONITOR 观察命令（注意：生产环境慎用）
MONITOR

# Ctrl+C 退出
```

**3.3 检查热点 Key**
```bash
# 如果有热点 Key，考虑:
# - 增加本地缓存
# - 使用读副本
# - 分片热点数据
```

## 数据问题

### 1. 数据丢失

#### 可能原因
- 使用内存存储引擎未持久化
- 服务异常退出
- 磁盘故障

#### 预防措施
```toml
# 使用 AiDb 持久化存储
[storage]
engine = "aidb"
data_dir = "/data/aikv"
```

### 2. 数据损坏

#### 症状
```
Error: Corrupted data file
```

#### 恢复步骤

**2.1 检查磁盘**
```bash
# 检查磁盘健康
smartctl -a /dev/sda

# 检查文件系统
fsck /dev/sda1
```

**2.2 从备份恢复**
```bash
# 停止服务
systemctl stop aikv

# 恢复数据
rm -rf /data/aikv/*
tar -xzf backup.tar.gz -C /data/aikv/

# 启动服务
systemctl start aikv
```

### 3. 过期键未删除

#### 说明
AiKv 使用惰性删除 + 定期清理策略，过期键可能不会立即删除。

#### 手动触发清理
```bash
# 使用 SCAN 遍历并检查
SCAN 0 COUNT 1000
```

## 集群问题

### 1. 集群状态异常

#### 检查集群状态
```bash
CLUSTER INFO
# cluster_state:ok 表示正常
# cluster_state:fail 表示异常
```

#### 常见问题

| 问题 | 原因 | 解决方案 |
|------|------|----------|
| `cluster_slots_ok < 16384` | 槽位未完全分配 | 分配缺失槽位 |
| `cluster_known_nodes` 减少 | 节点失联 | 检查网络和节点状态 |

### 2. 节点失联

#### 检查步骤
```bash
# 查看节点状态
CLUSTER NODES

# 标记为 fail 的节点需要检查:
# - 网络连通性
# - 节点进程状态
# - 防火墙配置
```

### 3. 槽迁移失败

#### 症状
```bash
CLUSTER SETSLOT ... error
```

#### 解决方案
```bash
# 检查迁移状态
CLUSTER NODES | grep migrating
CLUSTER NODES | grep importing

# 取消迁移（如需要）
CLUSTER SETSLOT <slot> STABLE
```

## 日志分析

### 日志级别

| 级别 | 说明 | 使用场景 |
|------|------|----------|
| error | 错误信息 | 生产环境 |
| warn | 警告信息 | 生产环境 |
| info | 一般信息 | 正常运行 |
| debug | 调试信息 | 问题排查 |
| trace | 详细跟踪 | 深度调试 |

### 调整日志级别

```bash
# 动态调整
CONFIG SET loglevel debug

# 配置文件
[logging]
level = "debug"
```

### 常见日志模式

```
# 正常启动
INFO  aikv: Starting AiKv server on 127.0.0.1:6379

# 连接建立
DEBUG aikv: New connection from 192.168.1.100:45678

# 慢查询
WARN  aikv: Slow command: KEYS * took 500ms

# 错误
ERROR aikv: Failed to persist data: disk full
```

## 获取帮助

### 收集诊断信息

在报告问题前，请收集以下信息：

```bash
# 1. 版本信息
./aikv --version

# 2. 配置信息（隐藏敏感信息）
cat config.toml

# 3. 服务器信息
redis-cli INFO

# 4. 慢查询日志
redis-cli SLOWLOG GET 20

# 5. 系统信息
uname -a
free -h
df -h

# 6. 错误日志
tail -100 /var/log/aikv/aikv.log
```

### 报告问题

1. GitHub Issues: https://github.com/Genuineh/AiKv/issues
2. 提供详细的问题描述
3. 附上诊断信息
4. 描述复现步骤

---

## 集群问题

### 1. 集群初始化卡住 - "Waiting for the cluster to join"

#### 症状
```bash
$ redis-cli --cluster create 127.0.0.1:6379 127.0.0.1:6380 127.0.0.1:6381 ...
>>> Performing hash slots allocation on 6 nodes...
...
Waiting for the cluster to join
...................................................................................^C
```

#### 原因分析

这是因为 AiKv 使用 **Multi-Raft 共识** 而非 Redis gossip 协议来同步集群状态。详见 [CLUSTER_BUS_ANALYSIS.md](CLUSTER_BUS_ANALYSIS.md)。

**技术说明:**
- AiKv 正确报告 `cluster_enabled:1`
- CLUSTER 命令（MEET, ADDSLOTS, NODES 等）已实现
- AiKv 使用 AiDb 的 Multi-Raft 进行状态同步
- **不需要端口 16379** - 这是设计选择，不是缺陷

#### 解决方案: 使用手动方式初始化集群

AiKv 集群使用 Raft 共识而非 gossip 协议，请使用以下方式初始化：

```bash
# 1. 启动 AiKv 节点（使用 aikv --help 查看可用选项）
aikv --host 127.0.0.1 --port 6379
aikv --host 127.0.0.1 --port 6380
aikv --host 127.0.0.1 --port 6381

# 2. 手动添加节点到集群（通过 CLUSTER MEET）
redis-cli -p 6379 CLUSTER MEET 127.0.0.1 6380
redis-cli -p 6379 CLUSTER MEET 127.0.0.1 6381

# 3. 分配槽位（使用循环以确保跨 shell 兼容性）
for i in $(seq 0 5460); do redis-cli -p 6379 CLUSTER ADDSLOTS $i; done
for i in $(seq 5461 10922); do redis-cli -p 6380 CLUSTER ADDSLOTS $i; done
for i in $(seq 10923 16383); do redis-cli -p 6381 CLUSTER ADDSLOTS $i; done

# 4. 验证集群状态
redis-cli -p 6379 CLUSTER INFO
redis-cli -p 6379 CLUSTER NODES
```

> **当前限制**: `redis-cli --cluster create` 命令暂不支持，因为它依赖 gossip 协议
> 进行节点发现。请使用上述手动方式配置集群。

#### 验证步骤

```bash
# 1. 检查集群模式是否启用
redis-cli -p 6379 INFO cluster
# 应该显示: cluster_enabled:1

# 2. 检查 CLUSTER INFO
redis-cli -p 6379 CLUSTER INFO
# cluster_state:ok (如果所有槽位已分配)

# 3. 检查 CLUSTER NODES 输出
redis-cli -p 6379 CLUSTER NODES
# 应该显示所有节点
```

**相关文档:**
- [CLUSTER_BUS_ANALYSIS.md](CLUSTER_BUS_ANALYSIS.md) - Multi-Raft 集群方案详解
- [TODO.md](../TODO.md) - 项目路线图

### 2. CLUSTER MEET 后节点不同步

#### 症状
```bash
# 在节点 6379 执行
redis-cli -p 6379 CLUSTER MEET 127.0.0.1 6380
OK

# 但是 CLUSTER NODES 仍然只显示部分节点
redis-cli -p 6379 CLUSTER NODES
# 只显示本节点和手动添加的节点
```

#### 原因

当前版本中，`CLUSTER MEET` 更新本地状态并通过 MetaRaft 提议节点加入。如果 Raft 共识尚未完成或节点间 Raft RPC 不可达，状态同步会延迟。

#### 解决方案

1. **确保 Raft RPC 端口可达**:
   ```bash
   # 检查 Raft 端口连通性
   nc -zv 127.0.0.1 50051
   nc -zv 127.0.0.1 50052
   ```

2. **等待 Raft 共识完成**:
   ```bash
   # 稍等几秒后重试
   sleep 2
   redis-cli -p 6379 CLUSTER NODES
   ```

3. **手动在所有节点执行 MEET**:
   ```bash
   # 确保双向可见
   redis-cli -p 6379 CLUSTER MEET 127.0.0.1 6380
   redis-cli -p 6380 CLUSTER MEET 127.0.0.1 6379
   ```

---

**最后更新**: 2025-12-08  
**版本**: v0.1.0  
**维护者**: @Genuineh, @copilot
