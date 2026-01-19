# AiKv 最佳实践

## 概述

本文档提供了使用 AiKv 的最佳实践建议，帮助您构建高性能、可靠的应用程序。

## 键命名规范

### 1. 使用命名空间

使用冒号 `:` 分隔的命名空间有助于组织和管理键。

```bash
# 推荐格式: <业务>:<对象类型>:<ID>:<属性>

# 用户信息
user:1000:profile
user:1000:settings
user:1000:sessions

# 订单信息
order:20231201:12345:status
order:20231201:12345:items

# 缓存键
cache:api:users:list
cache:page:home:zh-CN
```

### 2. 键名长度

- **推荐长度**: 50-200 字节
- **避免**: 过长的键名浪费内存
- **避免**: 过短的键名降低可读性

```bash
# 好
user:1000:profile

# 避免（太短，不易理解）
u:1k:p

# 避免（太长）
user_information_record:1000:complete_profile_data_with_all_details
```

### 3. 使用有意义的名称

```bash
# 好
session:abc123def456
product:sku:LAPTOP-001

# 避免
temp1
data
key123
```

## 数据类型选择

### String

**适用场景**:
- 简单的键值对
- 计数器
- 缓存序列化对象

```bash
# 简单值
SET user:1000:name "Alice"

# 计数器
INCR page:views:home

# 带过期的缓存
SET cache:token:abc123 "user_data" EX 3600
```

### Hash

**适用场景**:
- 对象属性
- 需要部分更新的数据

```bash
# 用户对象
HSET user:1000 name "Alice" email "alice@example.com" age 30

# 部分更新
HSET user:1000 last_login "2023-12-01"

# 读取单个字段
HGET user:1000 name
```

**优势**:
- 减少键的数量
- 支持部分读写
- 内存效率高

### List

**适用场景**:
- 消息队列
- 时间线
- 最近 N 个记录

```bash
# 消息队列（生产者）
LPUSH queue:tasks "task_data"

# 消息队列（消费者）
RPOP queue:tasks

# 最近 10 条通知
LPUSH notifications:user:1000 "new_message"
LTRIM notifications:user:1000 0 9
```

### Set

**适用场景**:
- 标签
- 好友关系
- 去重

```bash
# 标签系统
SADD article:1000:tags "python" "programming" "tutorial"

# 好友关系
SADD friends:user:1000 "user:1001" "user:1002"

# 检查关系
SISMEMBER friends:user:1000 "user:1001"
```

### Sorted Set (ZSet)

**适用场景**:
- 排行榜
- 时间排序的数据
- 带权重的队列

```bash
# 游戏排行榜
ZADD leaderboard 1000 "player:1" 950 "player:2" 900 "player:3"

# 获取前 10 名
ZREVRANGE leaderboard 0 9 WITHSCORES

# 延迟队列
ZADD delayed:queue 1701388800 "task:1"  # 到期时间戳
```

### JSON

**适用场景**:
- 复杂嵌套对象
- 需要路径查询的数据

```bash
# 存储 JSON 对象
JSON.SET user:1000 $ '{"name":"Alice","address":{"city":"Beijing"}}'

# 路径查询
JSON.GET user:1000 $.address.city
```

## 过期时间管理

### 1. 设置合理的 TTL

```bash
# 会话：短期
SET session:abc123 "data" EX 1800  # 30 分钟

# 缓存：中期
SET cache:api:users "data" EX 3600  # 1 小时

# 限流计数器
SET rate:ip:192.168.1.1 1 EX 60  # 1 分钟
```

### 2. 避免大量同时过期

```python
import random

# 添加随机偏移避免集中过期
base_ttl = 3600
jitter = random.randint(0, 300)  # 0-5 分钟随机
r.set(key, value, ex=base_ttl + jitter)
```

### 3. 使用 EXPIREAT 处理定时任务

```bash
# 在指定时间过期（如午夜）
EXPIREAT daily:stats 1701446400
```

## 批量操作

### 1. 使用 MSET/MGET

```bash
# 批量设置
MSET user:1:name "Alice" user:2:name "Bob" user:3:name "Charlie"

# 批量获取
MGET user:1:name user:2:name user:3:name
```

### 2. 使用管道 (Pipeline)

```python
# Python 示例
pipe = r.pipeline()
for i in range(1000):
    pipe.set(f'key:{i}', f'value:{i}')
results = pipe.execute()
```

```java
// Java (Jedis) 示例
Pipeline pipe = jedis.pipelined();
for (int i = 0; i < 1000; i++) {
    pipe.set("key:" + i, "value:" + i);
}
pipe.sync();
```

### 3. 批量大小建议

| 操作类型 | 建议批量大小 |
|----------|--------------|
| 简单读写 | 100-1000 |
| 复杂操作 | 50-100 |
| 网络延迟高 | 增加批量大小 |

## Lua 脚本最佳实践

### 1. 保持脚本简短

```lua
-- 好：原子计数和返回
local current = redis.call('GET', KEYS[1])
if current then
    return redis.call('INCR', KEYS[1])
else
    redis.call('SET', KEYS[1], 1)
    return 1
end
```

### 2. 使用 KEYS 和 ARGV

```lua
-- KEYS[1] 传递键名，ARGV[1] 传递参数
local value = redis.call('GET', KEYS[1])
if value == ARGV[1] then
    return redis.call('DEL', KEYS[1])
end
return 0
```

```bash
# 调用
EVAL "上面的脚本" 1 mykey expected_value
```

### 3. 预加载脚本

```bash
# 预加载获取 SHA1
SCRIPT LOAD "return redis.call('GET', KEYS[1])"
# 返回: "a42059b356c875f0717db19a51f6aaa9161e77a2"

# 使用 EVALSHA 调用
EVALSHA a42059b356c875f0717db19a51f6aaa9161e77a2 1 mykey
```

## 集群最佳实践

### 1. 使用哈希标签

```bash
# 确保相关键在同一槽位
SET {user:1000}:profile "..."
SET {user:1000}:settings "..."
SET {user:1000}:sessions "..."

# 现在可以使用 MGET
MGET {user:1000}:profile {user:1000}:settings
```

### 2. 避免跨槽操作

```bash
# 错误：不同槽位的键
MGET user:1 user:2 user:3

# 正确：使用哈希标签
MGET {group1}:user:1 {group1}:user:2 {group1}:user:3
```

### 3. 使用 aikv-tool 部署集群（推荐）

**推荐方式**: 使用 aikv-tool 一键部署 6 节点集群（3 主 3 从）

```bash
# 1. 安装 aikv-tool
cd aikv-toolchain && cargo install --path . && cd ..

# 2. 一键部署集群
aikv-tool cluster setup

# 3. 查看集群状态
aikv-tool cluster status

# 4. 连接使用
redis-cli -c -h 127.0.0.1 -p 6379
```

**优势**:
- ✅ 自动生成配置文件和 Docker Compose
- ✅ 自动构建镜像
- ✅ 自动初始化 MetaRaft 成员
- ✅ 自动分配 16384 槽位
- ✅ 自动配置主从复制

### 4. 集群管理命令

```bash
# 查看集群状态
aikv-tool cluster status

# 查看集群日志
aikv-tool cluster logs
aikv-tool cluster logs -f  # 实时日志

# 重启集群
aikv-tool cluster restart

# 停止集群（保留数据）
aikv-tool cluster stop

# 停止集群并清理数据
aikv-tool cluster stop -v
```

### 5. 处理重定向

```python
# Python redis-py-cluster 自动处理
from rediscluster import RedisCluster

startup_nodes = [{"host": "127.0.0.1", "port": "6379"}]
rc = RedisCluster(startup_nodes=startup_nodes, decode_responses=True)
```

## 错误处理

### 1. 捕获特定错误

```python
import redis

try:
    r.set('key', 'value')
except redis.ConnectionError:
    # 连接错误，重试或报警
    pass
except redis.TimeoutError:
    # 超时，考虑增加超时时间
    pass
except redis.ResponseError as e:
    if 'WRONGTYPE' in str(e):
        # 类型错误
        pass
```

### 2. 实现重试机制

```python
import time
from functools import wraps

def retry(max_retries=3, delay=0.1):
    def decorator(func):
        @wraps(func)
        def wrapper(*args, **kwargs):
            for i in range(max_retries):
                try:
                    return func(*args, **kwargs)
                except redis.ConnectionError:
                    if i < max_retries - 1:
                        time.sleep(delay * (2 ** i))
                    else:
                        raise
        return wrapper
    return decorator

@retry(max_retries=3)
def get_value(key):
    return r.get(key)
```

## 安全最佳实践

### 1. 网络隔离

- 不要将 AiKv 直接暴露在公网
- 使用防火墙限制访问 IP
- 使用 VPN 或内网通信

### 2. 输入验证

```python
# 验证键名
import re

def validate_key(key):
    if not re.match(r'^[\w:.-]+$', key):
        raise ValueError("Invalid key format")
    if len(key) > 200:
        raise ValueError("Key too long")
    return key

# 使用
safe_key = validate_key(user_input)
r.get(safe_key)
```

### 3. 避免存储敏感数据

- 不要存储明文密码
- 加密敏感信息
- 设置合理的过期时间

## 监控和告警

### 1. 关键指标

| 指标 | 告警阈值 | 说明 |
|------|----------|------|
| 连接数 | > 80% 最大值 | 可能需要扩容 |
| 内存使用 | > 80% | 清理或扩容 |
| 慢查询数 | > 10/分钟 | 优化查询 |
| 命令错误率 | > 1% | 检查客户端代码 |

### 2. 定期检查

```bash
# 定期执行健康检查
INFO
SLOWLOG GET 10
DBSIZE
```

## 代码示例

### Python 完整示例

```python
import redis
from redis.connection import ConnectionPool
import json

# 创建连接池
pool = ConnectionPool(
    host='localhost',
    port=6379,
    max_connections=100,
    socket_timeout=5,
    socket_connect_timeout=5,
    retry_on_timeout=True
)

r = redis.Redis(connection_pool=pool)

class UserService:
    def __init__(self, redis_client):
        self.r = redis_client
        self.cache_ttl = 3600
    
    def get_user(self, user_id):
        """获取用户信息，带缓存"""
        cache_key = f"user:{user_id}:profile"
        
        # 尝试从缓存获取
        cached = self.r.get(cache_key)
        if cached:
            return json.loads(cached)
        
        # 从数据库获取（模拟）
        user = self._fetch_from_db(user_id)
        
        # 写入缓存
        if user:
            self.r.setex(cache_key, self.cache_ttl, json.dumps(user))
        
        return user
    
    def update_user(self, user_id, data):
        """更新用户信息"""
        cache_key = f"user:{user_id}:profile"
        
        # 更新数据库（模拟）
        self._update_db(user_id, data)
        
        # 删除缓存
        self.r.delete(cache_key)
    
    def increment_login_count(self, user_id):
        """增加登录计数（原子操作）"""
        key = f"user:{user_id}:login_count"
        return self.r.incr(key)
    
    def _fetch_from_db(self, user_id):
        # 模拟数据库查询
        return {"id": user_id, "name": "Alice"}
    
    def _update_db(self, user_id, data):
        # 模拟数据库更新
        pass

# 使用
service = UserService(r)
user = service.get_user(1000)
```

### Go 完整示例

```go
package main

import (
    "context"
    "encoding/json"
    "time"

    "github.com/go-redis/redis/v8"
)

type UserService struct {
    client   *redis.Client
    cacheTTL time.Duration
}

func NewUserService(addr string) *UserService {
    client := redis.NewClient(&redis.Options{
        Addr:         addr,
        PoolSize:     100,
        MinIdleConns: 10,
        DialTimeout:  5 * time.Second,
        ReadTimeout:  3 * time.Second,
        WriteTimeout: 3 * time.Second,
    })

    return &UserService{
        client:   client,
        cacheTTL: time.Hour,
    }
}

func (s *UserService) GetUser(ctx context.Context, userID int64) (*User, error) {
    cacheKey := fmt.Sprintf("user:%d:profile", userID)

    // 尝试从缓存获取
    val, err := s.client.Get(ctx, cacheKey).Result()
    if err == nil {
        var user User
        if err := json.Unmarshal([]byte(val), &user); err == nil {
            return &user, nil
        }
    }

    // 从数据库获取
    user, err := s.fetchFromDB(userID)
    if err != nil {
        return nil, err
    }

    // 写入缓存
    if data, err := json.Marshal(user); err == nil {
        s.client.SetEX(ctx, cacheKey, data, s.cacheTTL)
    }

    return user, nil
}

func (s *UserService) IncrementLoginCount(ctx context.Context, userID int64) (int64, error) {
    key := fmt.Sprintf("user:%d:login_count", userID)
    return s.client.Incr(ctx, key).Result()
}
```

---

**最后更新**: 2026-01-16  
**版本**: v0.1.0  
**维护者**: @Genuineh
