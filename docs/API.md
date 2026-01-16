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

### INCR

将存储的数字值加一。如果键不存在，会在操作前将其设置为 0。

**语法:**
```
INCR key
```

**返回值:**
- 操作后的值

**示例:**
```bash
redis> SET counter "100"
OK
redis> INCR counter
(integer) 101
redis> INCR counter
(integer) 102
```

**时间复杂度:** O(1)

---

### DECR

将存储的数字值减一。如果键不存在，会在操作前将其设置为 0。

**语法:**
```
DECR key
```

**返回值:**
- 操作后的值

**示例:**
```bash
redis> SET counter "100"
OK
redis> DECR counter
(integer) 99
```

**时间复杂度:** O(1)

---

### INCRBY

将存储的数字值加上指定的整数增量。

**语法:**
```
INCRBY key increment
```

**参数:**
- `key`: 键名
- `increment`: 增量值（整数）

**返回值:**
- 操作后的值

**示例:**
```bash
redis> SET counter "10"
OK
redis> INCRBY counter 5
(integer) 15
```

**时间复杂度:** O(1)

---

### DECRBY

将存储的数字值减去指定的整数减量。

**语法:**
```
DECRBY key decrement
```

**参数:**
- `key`: 键名
- `decrement`: 减量值（整数）

**返回值:**
- 操作后的值

**示例:**
```bash
redis> SET counter "10"
OK
redis> DECRBY counter 3
(integer) 7
```

**时间复杂度:** O(1)

---

### INCRBYFLOAT

将存储的数字值加上指定的浮点增量。

**语法:**
```
INCRBYFLOAT key increment
```

**参数:**
- `key`: 键名
- `increment`: 增量值（浮点数）

**返回值:**
- 操作后的值（字符串形式）

**示例:**
```bash
redis> SET value "10.5"
OK
redis> INCRBYFLOAT value 0.1
"10.6"
redis> INCRBYFLOAT value -5.2
"5.4"
```

**时间复杂度:** O(1)

---

### GETRANGE

获取存储在键中的字符串值的子字符串。

**语法:**
```
GETRANGE key start end
```

**参数:**
- `key`: 键名
- `start`: 起始位置（支持负索引）
- `end`: 结束位置（支持负索引）

**返回值:**
- 子字符串

**示例:**
```bash
redis> SET mykey "Hello World"
OK
redis> GETRANGE mykey 0 4
"Hello"
redis> GETRANGE mykey -5 -1
"World"
```

**时间复杂度:** O(N)，其中 N 是返回字符串的长度

---

### SETRANGE

从指定偏移量开始覆盖存储在键中的字符串值。

**语法:**
```
SETRANGE key offset value
```

**参数:**
- `key`: 键名
- `offset`: 偏移量
- `value`: 要覆盖的值

**返回值:**
- 修改后字符串的长度

**示例:**
```bash
redis> SET mykey "Hello World"
OK
redis> SETRANGE mykey 6 "Redis"
(integer) 11
redis> GET mykey
"Hello Redis"
```

**时间复杂度:** O(1)

---

### GETEX

获取键的值，并可选地设置其过期时间。

**语法:**
```
GETEX key [EX seconds | PX milliseconds | EXAT unix-time | PXAT unix-time-ms | PERSIST]
```

**参数:**
- `key`: 键名
- `EX seconds`: 设置过期时间（秒）
- `PX milliseconds`: 设置过期时间（毫秒）
- `EXAT unix-time`: 设置过期的 Unix 时间戳（秒）
- `PXAT unix-time-ms`: 设置过期的 Unix 时间戳（毫秒）
- `PERSIST`: 移除过期时间

**返回值:**
- 键的值，如果键不存在则返回 nil

**示例:**
```bash
redis> SET mykey "Hello"
OK
redis> GETEX mykey EX 60
"Hello"
```

**时间复杂度:** O(1)

---

### GETDEL

获取键的值并删除该键。

**语法:**
```
GETDEL key
```

**返回值:**
- 键的值，如果键不存在则返回 nil

**示例:**
```bash
redis> SET mykey "Hello"
OK
redis> GETDEL mykey
"Hello"
redis> GET mykey
(nil)
```

**时间复杂度:** O(1)

---

### SETNX

仅当键不存在时设置键的值。

**语法:**
```
SETNX key value
```

**返回值:**
- 1: 键被设置
- 0: 键未被设置（键已存在）

**示例:**
```bash
redis> SETNX mykey "Hello"
(integer) 1
redis> SETNX mykey "World"
(integer) 0
redis> GET mykey
"Hello"
```

**时间复杂度:** O(1)

---

### SETEX

设置键的值并指定过期时间（秒）。

**语法:**
```
SETEX key seconds value
```

**参数:**
- `key`: 键名
- `seconds`: 过期时间（秒）
- `value`: 值

**返回值:**
- OK

**示例:**
```bash
redis> SETEX mykey 10 "Hello"
OK
redis> TTL mykey
(integer) 10
```

**时间复杂度:** O(1)

---

### PSETEX

设置键的值并指定过期时间（毫秒）。

**语法:**
```
PSETEX key milliseconds value
```

**参数:**
- `key`: 键名
- `milliseconds`: 过期时间（毫秒）
- `value`: 值

**返回值:**
- OK

**示例:**
```bash
redis> PSETEX mykey 10000 "Hello"
OK
redis> PTTL mykey
(integer) 10000
```

**时间复杂度:** O(1)

---

## List 命令扩展

### LPOS

返回列表中匹配元素的索引。

**语法:**
```
LPOS key element [RANK rank] [COUNT num-matches] [MAXLEN len]
```

**参数:**
- `key`: 键名
- `element`: 要查找的元素
- `RANK rank`: 指定返回第几个匹配（正数从头开始，负数从尾开始）
- `COUNT num-matches`: 返回的匹配数量（0 表示全部）
- `MAXLEN len`: 扫描的最大元素数量

**返回值:**
- 不带 COUNT: 返回第一个匹配的索引，如果没找到返回 nil
- 带 COUNT: 返回匹配索引的数组

**示例:**
```bash
redis> RPUSH mylist "a" "b" "c" "b" "d" "b"
(integer) 6
redis> LPOS mylist "b"
(integer) 1
redis> LPOS mylist "b" RANK 2
(integer) 3
redis> LPOS mylist "b" COUNT 0
1) (integer) 1
2) (integer) 3
3) (integer) 5
```

**时间复杂度:** O(N)

---

## Set 命令扩展

### SSCAN

增量迭代集合中的成员。

**语法:**
```
SSCAN key cursor [MATCH pattern] [COUNT count]
```

**参数:**
- `key`: 键名
- `cursor`: 游标（从 0 开始）
- `MATCH pattern`: 匹配模式
- `COUNT count`: 每次迭代返回的元素数量提示

**返回值:**
- 包含两个元素的数组：下一个游标和成员数组

**示例:**
```bash
redis> SADD myset "member1" "member2" "member3"
(integer) 3
redis> SSCAN myset 0
1) "0"
2) 1) "member1"
   2) "member2"
   3) "member3"
redis> SSCAN myset 0 MATCH "member*"
1) "0"
2) 1) "member1"
   2) "member2"
   3) "member3"
```

**时间复杂度:** O(1) 每次调用，O(N) 完整迭代

---

### SMOVE

将成员从一个集合移动到另一个集合。

**语法:**
```
SMOVE source destination member
```

**参数:**
- `source`: 源集合
- `destination`: 目标集合
- `member`: 要移动的成员

**返回值:**
- 1: 成员被成功移动
- 0: 成员不在源集合中

**示例:**
```bash
redis> SADD src "a" "b" "c"
(integer) 3
redis> SADD dst "x" "y"
(integer) 2
redis> SMOVE src dst "b"
(integer) 1
redis> SMEMBERS src
1) "a"
2) "c"
redis> SMEMBERS dst
1) "b"
2) "x"
3) "y"
```

**时间复杂度:** O(1)

---

## Sorted Set 命令扩展

### ZSCAN

增量迭代有序集合中的成员和分数。

**语法:**
```
ZSCAN key cursor [MATCH pattern] [COUNT count]
```

**参数:**
- `key`: 键名
- `cursor`: 游标（从 0 开始）
- `MATCH pattern`: 匹配模式
- `COUNT count`: 每次迭代返回的元素数量提示

**返回值:**
- 包含两个元素的数组：下一个游标和成员-分数数组

**示例:**
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three"
(integer) 3
redis> ZSCAN myzset 0
1) "0"
2) 1) "one"
   2) "1"
   3) "two"
   4) "2"
   5) "three"
   6) "3"
```

**时间复杂度:** O(1) 每次调用，O(N) 完整迭代

---

### ZPOPMIN

移除并返回有序集合中分数最低的成员。

**语法:**
```
ZPOPMIN key [count]
```

**参数:**
- `key`: 键名
- `count`: 返回的成员数量（可选，默认 1）

**返回值:**
- 成员和分数的数组

**示例:**
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three"
(integer) 3
redis> ZPOPMIN myzset
1) "one"
2) "1"
redis> ZPOPMIN myzset 2
1) "two"
2) "2"
3) "three"
4) "3"
```

**时间复杂度:** O(log(N)*M)，其中 N 是有序集合的大小，M 是弹出的成员数量

---

### ZPOPMAX

移除并返回有序集合中分数最高的成员。

**语法:**
```
ZPOPMAX key [count]
```

**参数:**
- `key`: 键名
- `count`: 返回的成员数量（可选，默认 1）

**返回值:**
- 成员和分数的数组

**示例:**
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three"
(integer) 3
redis> ZPOPMAX myzset
1) "three"
2) "3"
```

**时间复杂度:** O(log(N)*M)，其中 N 是有序集合的大小，M 是弹出的成员数量

---

### ZRANGEBYLEX

按字典序范围返回有序集合中的成员（需要所有成员具有相同分数）。

**语法:**
```
ZRANGEBYLEX key min max [LIMIT offset count]
```

**参数:**
- `key`: 键名
- `min`: 最小值（`-` 表示负无穷，`[value` 包含，`(value` 不包含）
- `max`: 最大值（`+` 表示正无穷，`[value` 包含，`(value` 不包含）
- `LIMIT offset count`: 分页选项

**返回值:**
- 成员数组

**示例:**
```bash
redis> ZADD myzset 0 "a" 0 "b" 0 "c" 0 "d" 0 "e"
(integer) 5
redis> ZRANGEBYLEX myzset [b [d
1) "b"
2) "c"
3) "d"
redis> ZRANGEBYLEX myzset - +
1) "a"
2) "b"
3) "c"
4) "d"
5) "e"
```

**时间复杂度:** O(log(N)+M)，其中 N 是有序集合的大小，M 是返回的成员数量

---

### ZREVRANGEBYLEX

按字典序逆序范围返回有序集合中的成员。

**语法:**
```
ZREVRANGEBYLEX key max min [LIMIT offset count]
```

**参数:**
- `key`: 键名
- `max`: 最大值
- `min`: 最小值
- `LIMIT offset count`: 分页选项

**返回值:**
- 成员数组（逆序）

**示例:**
```bash
redis> ZADD myzset 0 "a" 0 "b" 0 "c" 0 "d" 0 "e"
(integer) 5
redis> ZREVRANGEBYLEX myzset [d [b
1) "d"
2) "c"
3) "b"
```

**时间复杂度:** O(log(N)+M)

---

### ZLEXCOUNT

返回有序集合中指定字典序范围内的成员数量。

**语法:**
```
ZLEXCOUNT key min max
```

**参数:**
- `key`: 键名
- `min`: 最小值
- `max`: 最大值

**返回值:**
- 范围内的成员数量

**示例:**
```bash
redis> ZADD myzset 0 "a" 0 "b" 0 "c" 0 "d" 0 "e"
(integer) 5
redis> ZLEXCOUNT myzset [b [d
(integer) 3
redis> ZLEXCOUNT myzset - +
(integer) 5
```

**时间复杂度:** O(log(N))

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
