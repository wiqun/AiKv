# AiKv API 文档

## 概述

本文档详细描述了 AiKv Redis 协议兼容层支持的所有命令及其使用方法。AiKv 支持 RESP2 和 RESP3 协议。

## 协议支持

AiKv 支持两种 Redis 序列化协议版本：

- **RESP2** (默认): 传统的 Redis 协议，兼容所有 Redis 客户端
- **RESP3**: 新版协议，支持更多数据类型 (Null, Boolean, Double, Map, Set, Push, Attributes, Streaming 等)

### RESP3 高级特性

- **Attributes**: 允许服务器在响应中附加元数据，如 TTL、流行度统计等
- **Streaming**: 支持大型字符串的分块传输，减少内存使用

### 协议切换

使用 `HELLO` 命令在 RESP2 和 RESP3 之间切换。

## 连接到 AiKv

可以使用任何 Redis 客户端连接到 AiKv：

```bash
# 使用 redis-cli
redis-cli -h 127.0.0.1 -p 6379

# 使用 telnet
telnet 127.0.0.1 6379
```

## 协议命令

### HELLO

协议版本协商命令，用于切换 RESP2 和 RESP3 协议。

**语法:**
```
HELLO protover
```

**参数:**
- `protover`: 协议版本 (2 或 3)

**返回值:**
- RESP2 模式: 返回数组包含服务器信息
- RESP3 模式: 返回 Map 类型包含服务器信息

**示例:**
```bash
# 切换到 RESP3
redis> HELLO 3
1) "server"
2) "aikv"
3) "version"
4) "0.1.0"
5) "proto"
6) (integer) 3

# 切换回 RESP2
redis> HELLO 2
1) "server"
2) "aikv"
3) "version"
4) "0.1.0"
5) "proto"
6) (integer) 2
```

**时间复杂度:** O(1)

---

### PING

测试服务器连接是否正常。

**语法:**
```
PING
```

**返回值:**
- 返回 "PONG"

**示例:**
```bash
redis> PING
PONG
```

**时间复杂度:** O(1)

---

### ECHO

回显给定的字符串。

**语法:**
```
ECHO message
```

**参数:**
- `message`: 要回显的消息

**返回值:**
- 返回给定的消息

**示例:**
```bash
redis> ECHO "Hello World"
"Hello World"
```

**时间复杂度:** O(1)

---

## String 命令

String 是 Redis 最基本的数据类型，可以存储字符串、整数或浮点数。

### GET

获取指定键的值。

**语法:**
```
GET key
```

**返回值:**
- 如果键存在，返回键对应的值
- 如果键不存在，返回 nil

**示例:**
```bash
redis> SET mykey "Hello"
OK
redis> GET mykey
"Hello"
redis> GET nonexisting
(nil)
```

**时间复杂度:** O(1)

---

### SET

设置指定键的值。如果键已经存在，SET 会覆盖旧值，无视其类型。

**语法:**
```
SET key value [EX seconds] [PX milliseconds] [NX|XX]
```

**参数:**
- `key`: 键名
- `value`: 要设置的值
- `EX seconds`: 设置过期时间（秒）
- `PX milliseconds`: 设置过期时间（毫秒）
- `NX`: 只在键不存在时设置
- `XX`: 只在键存在时设置

**返回值:**
- `OK`: 设置成功
- `nil`: 使用 NX 或 XX 选项时，条件不满足

**示例:**
```bash
redis> SET mykey "Hello"
OK
redis> GET mykey
"Hello"

# 设置带过期时间的键
redis> SET mykey "Hello" EX 10
OK

# 只在键不存在时设置
redis> SET mykey "Hello" NX
(nil)

# 只在键存在时设置
redis> SET mykey "World" XX
OK
```

**时间复杂度:** O(1)

---

### DEL

删除一个或多个键。

**语法:**
```
DEL key [key ...]
```

**返回值:**
- 返回被删除的键的数量

**示例:**
```bash
redis> SET key1 "Hello"
OK
redis> SET key2 "World"
OK
redis> DEL key1 key2 key3
(integer) 2
```

**时间复杂度:** O(N)，其中 N 是要删除的键的数量

---

### EXISTS

检查给定键是否存在。

**语法:**
```
EXISTS key [key ...]
```

**返回值:**
- 返回存在的键的数量

**示例:**
```bash
redis> SET key1 "Hello"
OK
redis> EXISTS key1
(integer) 1
redis> EXISTS nosuchkey
(integer) 0
redis> SET key2 "World"
OK
redis> EXISTS key1 key2 nosuchkey
(integer) 2
```

**时间复杂度:** O(N)，其中 N 是要检查的键的数量

---

### MGET

获取所有（一个或多个）给定键的值。

**语法:**
```
MGET key [key ...]
```

**返回值:**
- 返回一个数组，包含所有给定键的值
- 如果某个键不存在，对应位置返回 nil

**示例:**
```bash
redis> SET key1 "Hello"
OK
redis> SET key2 "World"
OK
redis> MGET key1 key2 nonexisting
1) "Hello"
2) "World"
3) (nil)
```

**时间复杂度:** O(N)，其中 N 是键的数量

---

### MSET

同时设置一个或多个键值对。

**语法:**
```
MSET key value [key value ...]
```

**返回值:**
- 总是返回 OK

**示例:**
```bash
redis> MSET key1 "Hello" key2 "World"
OK
redis> GET key1
"Hello"
redis> GET key2
"World"
```

**时间复杂度:** O(N)，其中 N 是要设置的键的数量

---

### STRLEN

返回键存储的字符串值的长度。

**语法:**
```
STRLEN key
```

**返回值:**
- 字符串值的长度
- 如果键不存在，返回 0

**示例:**
```bash
redis> SET mykey "Hello World"
OK
redis> STRLEN mykey
(integer) 11
redis> STRLEN nonexisting
(integer) 0
```

**时间复杂度:** O(1)

---

### APPEND

如果键已经存在并且是一个字符串，该命令会将 value 追加到原来的值的末尾。如果键不存在，该命令会创建一个新键，效果等同于 SET。

**语法:**
```
APPEND key value
```

**返回值:**
- 追加后字符串值的长度

**示例:**
```bash
redis> EXISTS mykey
(integer) 0
redis> APPEND mykey "Hello"
(integer) 5
redis> APPEND mykey " World"
(integer) 11
redis> GET mykey
"Hello World"
```

**时间复杂度:** O(1)

---

## JSON 命令

JSON 命令允许在 Redis 中存储、更新和检索 JSON 值。

### JSON.GET

获取 JSON 值。

**语法:**
```
JSON.GET key [path]
```

**参数:**
- `key`: 键名
- `path`: JSONPath 表达式（可选，默认为 `$` 即根路径）

**返回值:**
- JSON 字符串表示

**示例:**
```bash
redis> JSON.SET user $ '{"name":"John","age":30,"city":"New York"}'
OK
redis> JSON.GET user
"{\"name\":\"John\",\"age\":30,\"city\":\"New York\"}"
redis> JSON.GET user $.name
"\"John\""
redis> JSON.GET user $.age
"30"
```

**时间复杂度:** O(N)，其中 N 是 JSON 值的大小

---

### JSON.SET

设置 JSON 值。

**语法:**
```
JSON.SET key path value [NX|XX]
```

**参数:**
- `key`: 键名
- `path`: JSONPath 表达式
- `value`: JSON 值
- `NX`: 只在路径不存在时设置
- `XX`: 只在路径存在时设置

**返回值:**
- `OK`: 设置成功
- `nil`: 使用 NX 或 XX 选项时，条件不满足

**示例:**
```bash
redis> JSON.SET user $ '{"name":"John"}'
OK
redis> JSON.SET user $.age 30
OK
redis> JSON.GET user
"{\"name\":\"John\",\"age\":30}"
```

**时间复杂度:** O(M+N)，其中 M 是原 JSON 值的大小，N 是新值的大小

---

### JSON.DEL

删除 JSON 路径。

**语法:**
```
JSON.DEL key [path]
```

**参数:**
- `key`: 键名
- `path`: JSONPath 表达式（可选，默认为 `$` 即删除整个键）

**返回值:**
- 返回删除的路径数量

**示例:**
```bash
redis> JSON.SET user $ '{"name":"John","age":30,"city":"New York"}'
OK
redis> JSON.DEL user $.age
(integer) 1
redis> JSON.GET user
"{\"name\":\"John\",\"city\":\"New York\"}"
redis> JSON.DEL user
(integer) 1
```

**时间复杂度:** O(N)，其中 N 是 JSON 值的大小

---

### JSON.TYPE

获取 JSON 值的类型。

**语法:**
```
JSON.TYPE key [path]
```

**参数:**
- `key`: 键名
- `path`: JSONPath 表达式（可选，默认为 `$`）

**返回值:**
- JSON 类型：`string`, `number`, `boolean`, `null`, `object`, `array`

**示例:**
```bash
redis> JSON.SET user $ '{"name":"John","age":30,"active":true}'
OK
redis> JSON.TYPE user $.name
"string"
redis> JSON.TYPE user $.age
"number"
redis> JSON.TYPE user $.active
"boolean"
```

**时间复杂度:** O(1)

---

### JSON.STRLEN

获取 JSON 字符串的长度。

**语法:**
```
JSON.STRLEN key [path]
```

**参数:**
- `key`: 键名
- `path`: JSONPath 表达式（可选，默认为 `$`）

**返回值:**
- 字符串长度，如果不是字符串类型返回 nil

**示例:**
```bash
redis> JSON.SET user $ '{"name":"John"}'
OK
redis> JSON.STRLEN user $.name
(integer) 4
```

**时间复杂度:** O(1)

---

### JSON.ARRLEN

获取 JSON 数组的长度。

**语法:**
```
JSON.ARRLEN key [path]
```

**参数:**
- `key`: 键名
- `path`: JSONPath 表达式（可选，默认为 `$`）

**返回值:**
- 数组长度，如果不是数组类型返回 nil

**示例:**
```bash
redis> JSON.SET arr $ '[1,2,3,4,5]'
OK
redis> JSON.ARRLEN arr
(integer) 5
```

**时间复杂度:** O(1)

---

### JSON.OBJLEN

获取 JSON 对象的键数量。

**语法:**
```
JSON.OBJLEN key [path]
```

**参数:**
- `key`: 键名
- `path`: JSONPath 表达式（可选，默认为 `$`）

**返回值:**
- 对象键的数量，如果不是对象类型返回 nil

**示例:**
```bash
redis> JSON.SET user $ '{"name":"John","age":30}'
OK
redis> JSON.OBJLEN user
(integer) 2
```

**时间复杂度:** O(N)，其中 N 是对象中的键数量

---

## 错误处理

AiKv 返回的错误格式遵循 Redis RESP 协议：

```
-ERR error message
```

常见错误类型：

- **ERR wrong number of arguments**: 命令参数数量不正确
- **WRONGTYPE Operation against a key holding the wrong kind of value**: 对不匹配类型的键执行操作
- **ERR syntax error**: 命令语法错误
- **ERR invalid expire time**: 无效的过期时间

**示例:**
```bash
redis> GET
-ERR wrong number of arguments for 'get' command
```

## 客户端示例

### Rust 客户端

```rust
use redis::Commands;

fn main() -> redis::RedisResult<()> {
    let client = redis::Client::open("redis://127.0.0.1:6379")?;
    let mut con = client.get_connection()?;

    // String 操作
    con.set("mykey", "Hello")?;
    let value: String = con.get("mykey")?;
    println!("Value: {}", value);

    // JSON 操作
    con.set("user", r#"{"name":"John","age":30}"#)?;
    let json: String = con.get("user")?;
    println!("JSON: {}", json);

    Ok(())
}
```

### Python 客户端

```python
import redis
import json

# 连接到 AiKv
r = redis.Redis(host='127.0.0.1', port=6379, decode_responses=True)

# String 操作
r.set('mykey', 'Hello')
print(r.get('mykey'))

# JSON 操作
user = {'name': 'John', 'age': 30}
r.execute_command('JSON.SET', 'user', '$', json.dumps(user))
result = r.execute_command('JSON.GET', 'user')
print(json.loads(result))
```

### Node.js 客户端

```javascript
const redis = require('redis');

const client = redis.createClient({
    host: '127.0.0.1',
    port: 6379
});

client.on('connect', async () => {
    // String 操作
    await client.set('mykey', 'Hello');
    const value = await client.get('mykey');
    console.log('Value:', value);

    // JSON 操作
    const user = { name: 'John', age: 30 };
    await client.sendCommand(['JSON.SET', 'user', '$', JSON.stringify(user)]);
    const json = await client.sendCommand(['JSON.GET', 'user']);
    console.log('JSON:', JSON.parse(json));
});

client.connect();
```

### Go 客户端

```go
package main

import (
    "context"
    "fmt"
    "github.com/go-redis/redis/v8"
)

func main() {
    ctx := context.Background()
    
    rdb := redis.NewClient(&redis.Options{
        Addr: "127.0.0.1:6379",
    })

    // String 操作
    err := rdb.Set(ctx, "mykey", "Hello", 0).Err()
    if err != nil {
        panic(err)
    }

    val, err := rdb.Get(ctx, "mykey").Result()
    if err != nil {
        panic(err)
    }
    fmt.Println("Value:", val)

    // JSON 操作
    user := `{"name":"John","age":30}`
    err = rdb.Do(ctx, "JSON.SET", "user", "$", user).Err()
    if err != nil {
        panic(err)
    }

    json, err := rdb.Do(ctx, "JSON.GET", "user").Result()
    if err != nil {
        panic(err)
    }
    fmt.Println("JSON:", json)
}
```

## 性能建议

1. **批量操作**: 使用 MGET 和 MSET 代替多次 GET 和 SET
2. **连接池**: 使用连接池避免频繁创建连接
3. **Pipeline**: 使用 pipeline 减少网络往返
4. **合适的数据结构**: 根据使用场景选择合适的命令
5. **避免大值**: 尽量避免存储过大的值（建议 < 10MB）

## 限制

1. **键名长度**: 建议不超过 1KB
2. **值大小**: 建议不超过 512MB
3. **并发连接**: 默认最大 1000 个并发连接
4. **命令超时**: 默认命令超时时间为 30 秒

## 版本兼容性

AiKv 当前版本: v0.1.0

- Redis 协议版本: RESP2
- 兼容 Redis 客户端: 6.0+

## 支持与反馈

如有问题或建议，请访问项目 GitHub 仓库提交 Issue。
