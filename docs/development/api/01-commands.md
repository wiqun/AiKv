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

### SETBIT

设置或清除键对应值的指定位（bit）。

**语法:**
```
SETBIT key offset value
```

**参数:**
- `key`: 键名
- `offset`: 位偏移量（从 0 开始）
- `value`: 0 或 1

**返回值:**
- 该位原来的值

**示例:**
```bash
redis> SETBIT mykey 10 1
(integer) 0
redis> GETBIT mykey 10
(integer) 1
redis> GETBIT mykey 0
(integer) 0
```

**时间复杂度:** O(1)

---

### GETBIT

获取键对应值的指定位的值。

**语法:**
```
GETBIT key offset
```

**参数:**
- `key`: 键名
- `offset`: 位偏移量

**返回值:**
- 该位的值（0 或 1）

**示例:**
```bash
redis> SETBIT mykey 7 1
(integer) 0
redis> GETBIT mykey 7
(integer) 1
```

**时间复杂度:** O(1)

---

## List 命令

List（列表）是简单的字符串列表，按照插入顺序排序。你可以在列表的头部或尾部添加元素。

### LPUSH

将一个或多个值插入到列表的头部。

|**语法:**|
|----------|
```
LPUSH key element [element ...]
```

|**参数:**|
|- `key`: 键名|
|- `element`: 要插入的元素（至少一个）|

|**返回值:**|
|- 插入后列表的长度|

|**示例:**|
```bash
redis> LPUSH mylist "a"
(integer) 1
redis> LPUSH mylist "b" "c"
(integer) 3
redis> LRANGE mylist 0 -1
1) "c"
2) "b"
3) "a"
```

|**时间复杂度:** O(N)，其中 N 是插入元素的数量

---

### RPUSH

将一个或多个值插入到列表的尾部。

|**语法:**|
|----------|
```
RPUSH key element [element ...]
```

|**参数:**|
|- `key`: 键名|
|- `element`: 要插入的元素（至少一个）|

|**返回值:**|
|- 插入后列表的长度|

|**示例:**|
```bash
redis> RPUSH mylist "a"
(integer) 1
redis> RPUSH mylist "b" "c"
(integer) 3
redis> LRANGE mylist 0 -1
1) "a"
2) "b"
3) "c"
```

|**时间复杂度:** O(N)，其中 N 是插入元素的数量

---

### LPOP

移除并返回列表的第一个元素。

|**语法:**|
|----------|
```
LPOP key [count]
```

|**参数:**|
|- `key`: 键名|
|- `count`: 返回的元素数量（可选，默认 1）|

|**返回值:**|
|- 第一个元素的值（如果没有 count 参数）|
|- 包含多个元素的数组（如果指定了 count）|
|- nil（如果列表为空）|

|**示例:**|
```bash
redis> RPUSH mylist "a" "b" "c" "d"
(integer) 4
redis> LPOP mylist
"a"
redis> LPOP mylist 2
1) "b"
2) "c"
```

|**时间复杂度:** O(N)，其中 N 是返回元素的数量

---

### RPOP

移除并返回列表的最后一个元素。

|**语法:**|
|----------|
```
RPOP key [count]
```

|**参数:**|
|- `key`: 键名|
|- `count`: 返回的元素数量（可选，默认 1）|

|**返回值:**|
|- 最后一个元素的值（如果没有 count 参数）|
|- 包含多个元素的数组（如果指定了 count）|
|- nil（如果列表为空）|

|**示例:**|
```bash
redis> RPUSH mylist "a" "b" "c" "d"
(integer) 4
redis> RPOP mylist
"d"
redis> RPOP mylist 2
1) "c"
2) "b"
```

|**时间复杂度:** O(N)，其中 N 是返回元素的数量

---

### LLEN

返回列表的长度。

|**语法:**|
|----------|
```
LLEN key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 列表的长度（如果键不存在，返回 0）|

|**示例:**|
```bash
redis> RPUSH mylist "a" "b" "c"
(integer) 3
redis> LLEN mylist
(integer) 3
redis> DEL mylist
(integer) 1
redis> LLEN mylist
(integer) 0
```

|**时间复杂度:** O(1)

---

### LRANGE

返回列表中指定范围内的元素。

|**语法:**|
|----------|
```
LRANGE key start stop
```

|**参数:**|
|- `key`: 键名|
|- `start`: 起始索引（支持负索引）|
|- `stop`: 结束索引（支持负索引）|

|**返回值:**|
|- 指定范围内的元素数组|

|**示例:**|
```bash
redis> RPUSH mylist "a" "b" "c" "d" "e"
(integer) 5
redis> LRANGE mylist 0 2
1) "a"
2) "b"
3) "c"
redis> LRANGE mylist -3 -1
1) "c"
2) "d"
3) "e"
redis> LRANGE mylist 0 -1
1) "a"
2) "b"
3) "c"
4) "d"
5) "e"
```

|**时间复杂度:** O(N)，其中 N 是返回元素的数量

---

### LINDEX

通过索引返回列表中的元素。

|**语法:**|
|----------|
```
LINDEX key index
```

|**参数:**|
|- `key`: 键名|
|- `index`: 索引（支持负索引）|

|**返回值:**|
|- 指定索引处的元素|
|- nil（如果索引超出范围或键不存在）|

|**示例:**|
```bash
redis> RPUSH mylist "a" "b" "c"
(integer) 3
redis> LINDEX mylist 0
"a"
redis> LINDEX mylist 1
"b"
redis> LINDEX mylist -1
"c"
redis> LINDEX mylist 5
(nil)
```

|**时间复杂度:** O(N)，其中 N 是索引位置

---

### LSET

通过索引设置列表元素的值。

|**语法:**|
|----------|
```
LSET key index element
```

|**参数:**|
|- `key`: 键名|
|- `index`: 索引（支持负索引）|
|- `element`: 新值|

|**返回值:**|
|- OK（设置成功）|
|- ERR（如果索引超出范围）|

|**示例:**|
```bash
redis> RPUSH mylist "a" "b" "c"
(integer) 3
redis> LSET mylist 0 "x"
OK
redis> LRANGE mylist 0 -1
1) "x"
2) "b"
3) "c"
```

|**时间复杂度:** O(N)，其中 N 是列表长度

---

### LREM

从列表中移除元素。

|**语法:**|
|----------|
```
LREM key count element
```

|**参数:**|
|- `key`: 键名|
|- `count`: 要移除的元素数量|
|  - count > 0: 从头部开始移除|
|  - count < 0: 从尾部开始移除|
|  - count = 0: 移除所有匹配的元素|
|- `element`: 要移除的元素|

|**返回值:**|
|- 被移除元素的数量|

|**示例:**|
```bash
redis> RPUSH mylist "a" "b" "c" "a" "b" "c" "a"
(integer) 7
redis> LREM mylist 2 "a"
(integer) 2
redis> LRANGE mylist 0 -1
1) "b"
2) "c"
3) "b"
4) "c"
5) "a"
redis> LREM mylist -1 "a"
(integer) 1
redis> LREM mylist 0 "b"
(integer) 2
```

|**时间复杂度:** O(N)，其中 N 是列表长度

---

### LTRIM

对列表进行修剪，只保留指定范围内的元素。

|**语法:**|
|----------|
```
LTRIM key start stop
```

|**参数:**|
|- `key`: 键名|
|- `start`: 起始索引（支持负索引）|
|- `stop`: 结束索引（支持负索引）|

|**返回值:**|
|- OK（成功）|

|**示例:**|
```bash
redis> RPUSH mylist "a" "b" "c" "d" "e" "f"
(integer) 6
redis> LTRIM mylist 1 3
OK
redis> LRANGE mylist 0 -1
1) "b"
2) "c"
3) "d"
```

|**时间复杂度:** O(N)，其中 N 是被移除的元素数量

---

### LINSERT

在列表的元素前或后插入元素。

|**语法:**|
|----------|
```
LINSERT key BEFORE|AFTER pivot element
```

|**参数:**|
|- `key`: 键名|
|- `BEFORE|AFTER`: 在元素前或后插入|
|- `pivot`: 参考元素|
|- `element`: 要插入的元素|

|**返回值:**|
|- 插入后列表的长度|
|- -1（如果 pivot 不存在）|
|- 0（如果键不存在）|

|**示例:**|
```bash
redis> RPUSH mylist "a" "b" "c"
(integer) 3
redis> LINSERT mylist BEFORE "b" "x"
(integer) 4
redis> LRANGE mylist 0 -1
1) "a"
2) "x"
3) "b"
4) "c"
redis> LINSERT mylist AFTER "c" "y"
(integer) 5
redis> LRANGE mylist 0 -1
1) "a"
2) "x"
3) "b"
4) "c"
5) "y"
redis> LINSERT mylist BEFORE "notexist" "x"
(integer) -1
```

|**时间复杂度:** O(N)，其中 N 是到 pivot 元素的距离

---

### LMOVE

原子性地将元素从一个列表移动到另一个列表。

|**语法:**|
|----------|
```
LMOVE source destination LEFT|RIGHT LEFT|RIGHT
```

|**参数:**|
|- `source`: 源列表键名|
|- `destination`: 目标列表键名|
|- `LEFT|RIGHT`: 从源列表的哪一端弹出|
|- `LEFT|RIGHT`: 推入到目标列表的哪一端|

|**返回值:**|
|- 被移动的元素|
|- nil（如果源列表为空）|

|**示例:**|
```bash
redis> RPUSH list1 "a" "b" "c"
(integer) 3
redis> RPUSH list2 "x"
(integer) 1
redis> LMOVE list1 list2 RIGHT LEFT
"a"
redis> LRANGE list1 0 -1
1) "b"
2) "c"
redis> LRANGE list2 0 -1
1) "x"
2) "a"
```

|**时间复杂度:** O(N)，其中 N 是源列表的长度

---

## List 命令扩展

### LPOS

返回列表中匹配元素的索引。

|**语法:**|
|----------|
```
LPOS key element [RANK rank] [COUNT num-matches] [MAXLEN len]
```

|**参数:**|
|- `key`: 键名|
|- `element`: 要查找的元素|
|- `RANK rank`: 指定返回第几个匹配（正数从头开始，负数从尾开始）|
|- `COUNT num-matches`: 返回的匹配数量（0 表示全部）|
|- `MAXLEN len`: 扫描的最大元素数量|

|**返回值:**|
|- 不带 COUNT: 返回第一个匹配的索引，如果没找到返回 nil|
|- 带 COUNT: 返回匹配索引的数组|

|**示例:**|
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

|**时间复杂度:** O(N)

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

|**时间复杂度:** O(N)

---

## Hash 命令

Hash（哈希）是一个 string 类型的 field（字段）和 value（值）的映射表，非常适合用于存储对象。

### HSET

设置 hash 中指定字段的值。

|**语法:**|
|----------|
```
HSET key field value [field value ...]
```

|**参数:**|
|- `key`: 键名|
|- `field`: 字段名|
|- `value`: 值|

|**返回值:**|
|- 设置的字段数量（新增字段的数量）|

|**示例:**|
```bash
redis> HSET user name "John"
(integer) 1
redis> HSET user age 30 name "Jane"
(integer) 2
redis> HGET user name
"Jane"
redis> HGETALL user
1) "name"
2) "Jane"
3) "age"
4) "30"
```

|**时间复杂度:** O(N)，其中 N 是设置的字段数量

---

### HSETNX

仅当字段不存在时设置 hash 中字段的值。

|**语法:**|
|----------|
```
HSETNX key field value
```

|**参数:**|
|- `key`: 键名|
|- `field`: 字段名|
|- `value`: 值|

|**返回值:**|
|- 1: 字段被设置|
|- 0: 字段已存在，未设置|

|**示例:**|
```bash
redis> HSETNX user name "John"
(integer) 1
redis> HSETNX user name "Jane"
(integer) 0
redis> HGET user name
"John"
```

|**时间复杂度:** O(1)

---

### HGET

获取 hash 中指定字段的值。

|**语法:**|
|----------|
```
HGET key field
```

|**参数:**|
|- `key`: 键名|
|- `field`: 字段名|

|**返回值:**|
|- 字段的值（如果字段存在）|
|- nil（如果字段或键不存在）|

|**示例:**|
```bash
redis> HSET user name "John"
OK
redis> HGET user name
"John"
redis> HGET user age
(nil)
```

|**时间复杂度:** O(1)

---

### HMGET

获取 hash 中一个或多个字段的值。

|**语法:**|
|----------|
```
HMGET key field [field ...]
```

|**参数:**|
|- `key`: 键名|
|- `field`: 字段名（至少一个）|

|**返回值:**|
|- 包含字段值的数组|
|- nil（对应不存在的字段）|

|**示例:**|
```bash
redis> HSET user name "John" age 30 city "New York"
OK
redis> HMGET user name age
1) "John"
2) "30"
redis> HMGET user name age country
1) "John"
2) "30"
3) (nil)
```

|**时间复杂度:** O(N)，其中 N 是请求的字段数量

---

### HMSET

同时设置 hash 中多个字段的值。

|**语法:**|
|----------|
```
HMSET key field value [field value ...]
```

|**参数:**|
|- `key`: 键名|
|- `field value`: 字段-值对（至少一对）|

|**返回值:**|
|- OK|

|**示例:**|
```bash
redis> HMSET user name "John" age 30 city "New York"
OK
redis> HGETALL user
1) "name"
2) "John"
3) "age"
4) "30"
5) "city"
6) "New York"
```

|**时间复杂度:** O(N)，其中 N 是设置的字段数量

---

### HDEL

删除 hash 中一个或多个字段。

|**语法:**|
|----------|
```
HDEL key field [field ...]
```

|**参数:**|
|- `key`: 键名|
|- `field`: 字段名（至少一个）|

|**返回值:**|
|- 被删除字段的数量|

|**示例:**|
```bash
redis> HSET user name "John" age 30 city "New York"
(integer) 3
redis> HDEL user age city
(integer) 2
redis> HGETALL user
1) "name"
2) "John"
```

|**时间复杂度:** O(N)，其中 N 是删除的字段数量

---

### HEXISTS

检查 hash 中字段是否存在。

|**语法:**|
|----------|
```
HEXISTS key field
```

|**参数:**|
|- `key`: 键名|
|- `field`: 字段名|

|**返回值:**|
|- 1: 字段存在|
|- 0: 字段不存在|

|**示例:**|
```bash
redis> HSET user name "John"
(integer) 1
redis> HEXISTS user name
(integer) 1
redis> HEXISTS user age
(integer) 0
```

|**时间复杂度:** O(1)

---

### HLEN

返回 hash 中字段的数量。

|**语法:**|
|----------|
```
HLEN key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- hash 中的字段数量（如果键不存在，返回 0）|

|**示例:**|
```bash
redis> HSET user name "John" age 30 city "New York"
(integer) 3
redis> HLEN user
(integer) 3
```

|**时间复杂度:** O(1)

---

### HKEYS

返回 hash 中所有的字段名。

|**语法:**|
|----------|
```
HKEYS key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 包含所有字段名的数组|

|**示例:**|
```bash
redis> HSET user name "John" age 30 city "New York"
OK
redis> HKEYS user
1) "name"
2) "age"
3) "city"
```

|**时间复杂度:** O(N)，其中 N 是 hash 的大小

---

### HVALS

返回 hash 中所有的值。

|**语法:**|
|----------|
```
HVALS key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 包含所有值的数组|

|**示例:**|
```bash
redis> HSET user name "John" age 30 city "New York"
OK
redis> HVALS user
1) "John"
2) "30"
3) "New York"
```

|**时间复杂度:** O(N)，其中 N 是 hash 的大小

---

### HGETALL

返回 hash 中所有的字段和值。

|**语法:**|
|----------|
```
HGETALL key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 包含字段-值对的数组 [field1, value1, field2, value2, ...]|

|**示例:**|
```bash
redis> HSET user name "John" age 30
OK
redis> HGETALL user
1) "name"
2) "John"
3) "age"
4) "30"
```

|**时间复杂度:** O(N)，其中 N 是 hash 的大小

---

### HINCRBY

将 hash 中字段的整数值增加指定的数量。

|**语法:**|
|----------|
```
HINCRBY key field increment
```

|**参数:**|
|- `key`: 键名|
|- `field`: 字段名|
|- `increment`: 增量值（整数）|

|**返回值:**|
|- 增加后的值|

|**示例:**|
```bash
redis> HSET user age 20
OK
redis> HINCRBY user age 5
(integer) 25
redis> HINCRBY user age -3
(integer) 22
```

|**时间复杂度:** O(1)

---

### HINCRBYFLOAT

将 hash 中字段的浮点数值增加指定的数量。

|**语法:**|
|----------|
```
HINCRBYFLOAT key field increment
```

|**参数:**|
|- `key`: 键名|
|- `field`: 字段名|
|- `increment`: 增量值（浮点数）|

|**返回值:**|
|- 增加后的值（字符串形式）|

|**示例:**|
```bash
redis> HSET user score 10.5
OK
redis> HINCRBYFLOAT user score 0.5
"11"
redis> HINCRBYFLOAT user score -2.5
"8.5"
```

|**时间复杂度:** O(1)

---

### HSCAN

增量迭代 hash 中的字段和值。

|**语法:**|
|----------|
```
HSCAN key cursor [MATCH pattern] [COUNT count]
```

|**参数:**|
|- `key`: 键名|
|- `cursor`: 游标（从 0 开始）|
|- `MATCH pattern`: 匹配模式|
|- `COUNT count`: 每次迭代返回的元素数量提示|

|**返回值:**|
|- 包含两个元素的数组：下一个游标和字段-值数组|

|**示例:**|
```bash
redis> HMSET user name "John" age 30 city "New York" country "USA"
OK
redis> HSCAN user 0
1) "0"
2) 1) "name"
   2) "John"
   3) "age"
   4) "30"
   5) "city"
   6) "New York"
   7) "country"
   8) "USA"
redis> HSCAN user 0 MATCH "a*"
1) "0"
2) 1) "age"
   2) "30"
```

|**时间复杂度:** O(1) 每次调用，O(N) 完整迭代

---

## Set 命令

Set（集合）是 string 类型的无序集合，集合成员是唯一的。

### SADD

向集合添加一个或多个成员。

|**语法:**|
|----------|
```
SADD key member [member ...]
```

|**参数:**|
|- `key`: 键名|
|- `member`: 成员（至少一个）|

|**返回值:**|
|- 添加到集合中的新成员数量（不包括已存在的成员）|

|**示例:**|
```bash
redis> SADD myset "a" "b" "c"
(integer) 3
redis> SADD myset "a" "d"
(integer) 1
redis> SMEMBERS myset
1) "a"
2) "b"
3) "c"
4) "d"
```

|**时间复杂度:** O(N)，其中 N 是添加的成员数量

---

### SREM

从集合中移除一个或多个成员。

|**语法:**|
|----------|
```
SREM key member [member ...]
```

|**参数:**|
|- `key`: 键名|
|- `member`: 成员（至少一个）|

|**返回值:**|
|- 从集合中移除的成员数量（不存在的成员不计数）|

|**示例:**|
```bash
redis> SADD myset "a" "b" "c"
(integer) 3
redis> SREM myset "a" "b"
(integer) 2
redis> SMEMBERS myset
1) "c"
```

|**时间复杂度:** O(N)，其中 N 是移除的成员数量

---

### SISMEMBER

判断给定值是否是集合的成员。

|**语法:**|
|----------|
```
SISMEMBER key member
```

|**参数:**|
|- `key`: 键名|
|- `member`: 成员|

|**返回值:**|
|- 1: 成员在集合中|
|- 0: 成员不在集合中或键不存在|

|**示例:**|
```bash
redis> SADD myset "a"
(integer) 1
redis> SISMEMBER myset "a"
(integer) 1
redis> SISMEMBER myset "b"
(integer) 0
```

|**时间复杂度:** O(1)

---

### SMEMBERS

返回集合中的所有成员。

|**语法:**|
|----------|
```
SMEMBERS key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 集合中的所有成员（无序）|

|**示例:**|
```bash
redis> SADD myset "a" "b" "c"
(integer) 3
redis> SMEMBERS myset
1) "a"
2) "b"
3) "c"
```

|**时间复杂度:** O(N)，其中 N 是集合的大小

---

### SCARD

返回集合的成员数量。

|**语法:**|
|----------|
```
SCARD key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 集合的成员数量（如果键不存在，返回 0）|

|**示例:**|
```bash
redis> SADD myset "a" "b" "c"
(integer) 3
redis> SCARD myset
(integer) 3
```

|**时间复杂度:** O(1)

---

### SPOP

移除并返回集合中的一个或多个随机成员。

|**语法:**|
|----------|
```
SPOP key [count]
```

|**参数:**|
|- `key`: 键名|
|- `count`: 返回的成员数量（可选，默认 1）|

|**返回值:**|
|- 被移除的成员（如果没有 count 参数）|
|- 包含多个成员的数组（如果指定了 count）|
|- nil（如果集合为空）|

|**示例:**|
```bash
redis> SADD myset "a" "b" "c" "d"
(integer) 4
redis> SPOP myset
"a"
redis> SPOP myset 2
1) "b"
2) "c"
redis> SMEMBERS myset
1) "d"
```

|**时间复杂度:** O(N)，其中 N 是返回成员的数量

---

### SRANDMEMBER

返回集合中的一个或多个随机成员（不移除）。

|**语法:**|
|----------|
```
SRANDMEMBER key [count]
```

|**参数:**|
|- `key`: 键名|
|- `count`: 返回的成员数量（可选，默认 1，可为负数）|

|**返回值:**|
|- 随机成员（如果没有 count 参数）|
|- 包含多个成员的数组（如果指定了 count）|
|- nil（如果集合为空且 count 未指定）|

|**注意:** 如果 count 为正数，返回不重复的随机成员。如果 count 为负数，可能返回重复的成员。|

|**示例:**|
```bash
redis> SADD myset "a" "b" "c" "d"
(integer) 4
redis> SRANDMEMBER myset
"b"
redis> SRANDMEMBER myset 2
1) "a"
2) "c"
```

|**时间复杂度:** O(N)，其中 N 是返回成员的数量

---

### SUNION

返回给定集合的并集。

|**语法:**|
|----------|
```
SUNION key [key ...]
```

|**参数:**|
|- `key`: 键名（至少一个）|

|**返回值:**|
|- 并集中的所有成员（无序）|

|**示例:**|
```bash
redis> SADD set1 "a" "b" "c"
(integer) 3
redis> SADD set2 "c" "d" "e"
(integer) 3
redis> SUNION set1 set2
1) "a"
2) "b"
3) "c"
4) "d"
5) "e"
```

|**时间复杂度:** O(N)，其中 N 是所有集合的总大小

---

### SINTER

返回给定集合的交集。

|**语法:**|
|----------|
```
SINTER key [key ...]
```

|**参数:**|
|- `key`: 键名（至少一个）|

|**返回值:**|
|- 交集中的所有成员（无序）|
|- 空数组（如果没有任何交集）|

|**示例:**|
```bash
redis> SADD set1 "a" "b" "c"
(integer) 3
redis> SADD set2 "b" "c" "d"
(integer) 3
redis> SINTER set1 set2
1) "b"
2) "c"
```

|**时间复杂度:** O(N*M)，其中 N 是第一个集合的大小，M 是其他集合的平均大小

---

### SDIFF

返回给定集合的差集。

|**语法:**|
|----------|
```
SDIFF key [key ...]
```

|**参数:**|
|- `key`: 键名（至少一个，第一个集合为基准）|

|**返回值:**|
|- 差集中的所有成员（属于第一个集合但不属于其他集合）|

|**示例:**|
```bash
redis> SADD set1 "a" "b" "c"
(integer) 3
redis> SADD set2 "b" "c" "d"
(integer) 3
redis> SDIFF set1 set2
1) "a"
```

|**时间复杂度:** O(N)，其中 N 是第一个集合的大小

---

### SUNIONSTORE

将给定集合的并集存储到目标集合。

|**语法:**|
|----------|
```
SUNIONSTORE destination key [key ...]
```

|**参数:**|
|- `destination`: 目标集合键名|
|- `key`: 源集合键名（至少一个）|

|**返回值:**|
|- 存储到目标集合的成员数量|

|**示例:**|
```bash
redis> SADD set1 "a" "b" "c"
(integer) 3
redis> SADD set2 "c" "d" "e"
(integer) 3
redis> SUNIONSTORE result set1 set2
(integer) 5
redis> SMEMBERS result
1) "a"
2) "b"
3) "c"
4) "d"
5) "e"
```

|**时间复杂度:** O(N)，其中 N 是所有集合的总大小

---

### SINTERSTORE

将给定集合的交集存储到目标集合。

|**语法:**|
|----------|
```
SINTERSTORE destination key [key ...]
```

|**参数:**|
|- `destination`: 目标集合键名|
|- `key`: 源集合键名（至少一个）|

|**返回值:**|
|- 存储到目标集合的成员数量|

|**示例:**|
```bash
redis> SADD set1 "a" "b" "c"
(integer) 3
redis> SADD set2 "b" "c" "d"
(integer) 3
redis> SINTERSTORE result set1 set2
(integer) 2
redis> SMEMBERS result
1) "b"
2) "c"
```

|**时间复杂度:** O(N*M)，其中 N 是第一个集合的大小，M 是其他集合的平均大小

---

### SDIFFSTORE

将给定集合的差集存储到目标集合。

|**语法:**|
|----------|
```
SDIFFSTORE destination key [key ...]
```

|**参数:**|
|- `destination`: 目标集合键名|
|- `key`: 源集合键名（至少一个）|

|**返回值:**|
|- 存储到目标集合的成员数量|

|**示例:**|
```bash
redis> SADD set1 "a" "b" "c"
(integer) 3
redis> SADD set2 "b" "c" "d"
(integer) 3
redis> SDIFFSTORE result set1 set2
(integer) 1
redis> SMEMBERS result
1) "a"
```

|**时间复杂度:** O(N)，其中 N 是第一个集合的大小

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

## Sorted Set 命令

Sorted Set（有序集合）是 string 类型元素的集合，且不允许重复的成员。每个元素都会关联一个 double 类型的分数，通过分数来为集合中的成员进行从小到大的排序。

### ZADD

向有序集合添加一个或多个成员，或者更新已存在成员的分数。

|**语法:**|
|----------|
```
ZADD key [NX|XX] [CH] [INCR] score member [score member ...]
```

|**参数:**|
|- `key`: 键名|
|- `score`: 成员的分数（double 类型）|
|- `member`: 成员|
|- `NX`: 只添加新成员，不更新已存在成员|
|- `XX`: 只更新已存在成员，不添加新成员|
|- `CH`: 返回修改的元素数量（新增 + 更新）|
|- `INCR`: 增加成员的分数，返回增加后的分数|

|**返回值:**|
|- 添加或更新的成员数量（不使用 CH 时）|
|- 修改的元素数量（使用 CH 时）|
|- 增加后的分数（使用 INCR 时）|

|**示例:**|
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three"
(integer) 3
redis> ZADD myzset 4 "four"
(integer) 1
redis> ZRANGE myzset 0 -1 WITHSCORES
1) "one"
2) "1"
3) "two"
4) "2"
5) "three"
6) "3"
7) "four"
8) "4"
```

|**时间复杂度:** O(N)，其中 N 是添加的成员数量

---

### ZREM

从有序集合中移除一个或多个成员。

|**语法:**|
|----------|
```
ZREM key member [member ...]
```

|**参数:**|
|- `key`: 键名|
|- `member`: 成员（至少一个）|

|**返回值:**|
|- 从有序集合中移除的成员数量（不存在的成员不计数）|

|**示例:**|
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three"
(integer) 3
redis> ZREM myzset "two"
(integer) 1
redis> ZRANGE myzset 0 -1
1) "one"
2) "three"
```

|**时间复杂度:** O(N*M)，其中 N 是有序集合的大小，M 是移除的成员数量

---

### ZSCORE

返回有序集合中成员的分数。

|**语法:**|
|----------|
```
ZSCORE key member
```

|**参数:**|
|- `key`: 键名|
|- `member`: 成员|

|**返回值:**|
|- 成员的分数（字符串形式）|
|- nil（成员不存在）|

|**示例:**|
```bash
redis> ZADD myzset 1 "one"
(integer) 1
redis> ZSCORE myzset "one"
"1"
```

|**时间复杂度:** O(1)

---

### ZRANK

返回有序集合中成员的索引（按分数从小到大排序）。

|**语法:**|
|----------|
```
ZRANK key member
```

|**参数:**|
|- `key`: 键名|
|- `member`: 成员|

|**返回值:**|
|- 成员的索引（从 0 开始）|
|- nil（成员不存在）|

|**示例:**|
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three"
(integer) 3
redis> ZRANK myzset "one"
(integer) 0
redis> ZRANK myzset "three"
(integer) 2
```

|**时间复杂度:** O(log(N))，其中 N 是有序集合的大小

---

### ZREVRANK

返回有序集合中成员的逆序索引（按分数从大到小排序）。

|**语法:**|
|----------|
```
ZREVRANK key member
```

|**参数:**|
|- `key`: 键名|
|- `member`: 成员|

|**返回值:**|
|- 成员的逆序索引（从 0 开始）|
|- nil（成员不存在）|

|**示例:**|
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three"
(integer) 3
redis> ZREVRANK myzset "one"
(integer) 2
redis> ZREVRANK myzset "three"
(integer) 0
```

|**时间复杂度:** O(log(N))，其中 N 是有序集合的大小

---

### ZRANGE

通过索引区间返回有序集合的成员（按分数从小到大排序）。

|**语法:**|
|----------|
```
ZRANGE key start stop [WITHSCORES]
```

|**参数:**|
|- `key`: 键名|
|- `start`: 起始索引（支持负索引）|
|- `stop`: 结束索引（支持负索引）|
|- `WITHSCORES`: 同时返回成员的分数|

|**返回值:**|
|- 指定索引范围内的成员（不带 WITHSCORES）|
|- 包含成员和分数的数组（带 WITHSCORES）|

|**示例:**|
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three"
(integer) 3
redis> ZRANGE myzset 0 -1
1) "one"
2) "two"
3) "three"
redis> ZRANGE myzset 0 1 WITHSCORES
1) "one"
2) "1"
3) "two"
4) "2"
```

|**时间复杂度:** O(log(N) + M)，其中 N 是有序集合的大小，M 是返回的成员数量

---

### ZREVRANGE

通过索引区间返回有序集合的成员（按分数从大到小排序）。

|**语法:**|
|----------|
```
ZREVRANGE key start stop [WITHSCORES]
```

|**参数:**|
|- `key`: 键名|
|- `start`: 起始索引（支持负索引，从大到小）|
|- `stop`: 结束索引（支持负索引）|
|- `WITHSCORES`: 同时返回成员的分数|

|**返回值:**|
|- 指定索引范围内的成员（不带 WITHSCORES）|
|- 包含成员和分数的数组（带 WITHSCORES）|

|**示例:**|
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three"
(integer) 3
redis> ZREVRANGE myzset 0 -1
1) "three"
2) "two"
3) "one"
```

|**时间复杂度:** O(log(N) + M)，其中 N 是有序集合的大小，M 是返回的成员数量

---

### ZRANGEBYSCORE

通过分数区间返回有序集合的成员（按分数从小到大排序）。

|**语法:**|
|----------|
```
ZRANGEBYSCORE key min max [WITHSCORES] [LIMIT offset count]
```

|**参数:**|
|- `key`: 键名|
|- `min`: 最小分数（使用 -inf 表示负无穷）|
|- `max`: 最大分数（使用 +inf 表示正无穷）|
|- `WITHSCORES`: 同时返回成员的分数|
|- `LIMIT offset count`: 限制返回的成员数量|

|**返回值:**|
|- 指定分数范围内的成员（不带 WITHSCORES）|
|- 包含成员和分数的数组（带 WITHSCORES）|

|**示例:**|
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three" 4 "four"
(integer) 4
redis> ZRANGEBYSCORE myzset 2 3
1) "two"
2) "three"
redis> ZRANGEBYSCORE myzset -inf +inf WITHSCORES
1) "one"
2) "1"
3) "two"
4) "2"
5) "three"
6) "3"
7) "four"
8) "4"
```

|**时间复杂度:** O(log(N) + M)，其中 N 是有序集合的大小，M 是返回的成员数量

---

### ZREVRANGEBYSCORE

通过分数区间返回有序集合的成员（按分数从大到小排序）。

|**语法:**|
|----------|
```
ZREVRANGEBYSCORE key max min [WITHSCORES] [LIMIT offset count]
```

|**参数:**|
|- `key`: 键名|
|- `max`: 最大分数（使用 +inf 表示正无穷）|
|- `min`: 最小分数（使用 -inf 表示负无穷）|
|- `WITHSCORES`: 同时返回成员的分数|
|- `LIMIT offset count`: 限制返回的成员数量|

|**返回值:**|
|- 指定分数范围内的成员（不带 WITHSCORES）|
|- 包含成员和分数的数组（带 WITHSCORES）|

|**示例:**|
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three" 4 "four"
(integer) 4
redis> ZREVRANGEBYSCORE myzset 3 1
1) "three"
2) "two"
3) "one"
```

|**时间复杂度:** O(log(N) + M)，其中 N 是有序集合的大小，M 是返回的成员数量

---

### ZCARD

返回有序集合的成员数量。

|**语法:**|
|----------|
```
ZCARD key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 有序集合的成员数量（如果键不存在，返回 0）|

|**示例:**|
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three"
(integer) 3
redis> ZCARD myzset
(integer) 3
```

|**时间复杂度:** O(1)

---

### ZCOUNT

返回有序集合中指定分数区间内的成员数量。

|**语法:**|
|----------|
```
ZCOUNT key min max
```

|**参数:**|
|- `key`: 键名|
|- `min`: 最小分数|
|- `max`: 最大分数|

|**返回值:**|
|- 指定分数区间内的成员数量|

|**示例:**|
```bash
redis> ZADD myzset 1 "one" 2 "two" 3 "three"
(integer) 3
redis> ZCOUNT myzset 1 2
(integer) 2
```

|**时间复杂度:** O(log(N) + M)，其中 N 是有序集合的大小，M 是区间内的成员数量

---

### ZINCRBY

增加有序集合中成员的分数。

|**语法:**|
|----------|
```
ZINCRBY key increment member
```

|**参数:**|
|- `key`: 键名|
|- `increment`: 增加的分数（可为负数）|
|- `member`: 成员|

|**返回值:**|
|- 成员的新分数（字符串形式）|

|**示例:**|
```bash
redis> ZADD myzset 1 "one"
(integer) 1
redis> ZINCRBY myzset 2 "one"
"3"
redis> ZSCORE myzset "one"
"3"
```

|**时间复杂度:** O(log(N))，其中 N 是有序集合的大小

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
## Key 命令

Key（键）命令用于管理数据库中的键，包括键的查询、过期时间设置、类型检查等操作。

### KEYS

查找所有符合给定模式的键。

|**语法:**|
|----------|
```
KEYS pattern
```

|**参数:**|
|- `pattern`: 匹配模式，支持 `*` 匹配任意字符，`?` 匹配单个字符|

|**返回值:**|
|- 符合模式的键名数组|

|**示例:**|
```bash
redis> SET mykey "Hello"
OK
redis> SET myotherkey "World"
OK
redis> KEYS my*
1) "mykey"
2) "myotherkey"
redis> KEYS *
1) "mykey"
2) "myotherkey"
```

|**时间复杂度:** O(N)，其中 N 是数据库中的键数量

**注意:** 在生产环境中应谨慎使用 KEYS 命令，它会遍历所有键。对于大规模数据集，建议使用 SCAN 命令。

---

### SCAN

增量迭代数据库中的键。

|**语法:**|
|----------```
SCAN cursor [MATCH pattern] [COUNT count]
```

|**参数:**|
|- `cursor`: 游标（从 0 开始，0 表示开始新迭代）|
|- `MATCH pattern`: 匹配模式|
|- `COUNT count`: 每次迭代返回的键数量提示（默认 10）|

|**返回值:**|
|- 包含两个元素的数组：[下一个游标, 键名数组]|
|- 当游标返回 0 时表示迭代完成|

|**示例:**|
```bash
redis> SCAN 0
1) "0"
2) 1) "key1"
   2) "key2"
   3) "key3"
redis> SCAN 0 MATCH my*
1) "5"
2) 1) "mykey1"
   2) "mykey2"
```

|**时间复杂度:** O(1) 每次调用，O(N) 完整迭代

---

### RANDOMKEY

从当前数据库中随机返回一个键。

|**语法:**|
|----------|
```
RANDOMKEY
```

|**返回值:**|
|- 随机键名|
|- nil（如果数据库为空）|

|**示例:**|
```bash
redis> SET key1 "Hello"
OK
redis> SET key2 "World"
OK
redis> RANDOMKEY
"key2"
```

|**时间复杂度:** O(1)

---

### TYPE

返回键对应的值的数据类型。

|**语法:**|
|----------|
```
TYPE key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 值的类型：`string`, `list`, `set`, `zset`, `hash`, `none`（键不存在）|

|**示例:**|
```bash
redis> SET mykey "value"
OK
redis> LPUSH mylist "a"
(integer) 1
redis> TYPE mykey
"string"
redis> TYPE mylist
"list"
redis> TYPE nonexistent
"none"
```

|**时间复杂度:** O(1)

---

### RENAME

将键重命名为新键名。

|**语法:**|
|----------|
```
RENAME key newkey
```

|**参数:**|
|- `key`: 原键名|
|- `newkey`: 新键名|

|**返回值:**|
|- OK|

|**注意:** 如果 newkey 已存在，其值会被覆盖。|

|**示例:**|
```bash
redis> SET mykey "Hello"
OK
redis> RENAME mykey newkey
OK
redis> GET newkey
"Hello"
```

|**时间复杂度:** O(1)

---

### RENAMENX

仅当新键不存在时，将键重命名为新键名。

|**语法:**|
|----------|
```
RENAMENX key newkey
```

|**参数:**|
|- `key`: 原键名|
|- `newkey`: 新键名|

|**返回值:**|
|- 1: 重命名成功|
|- 0: newkey 已存在，未重命名|

|**示例:**|
```bash
redis> SET key1 "Hello"
OK
redis> SET key2 "World"
OK
redis> RENAMENX key1 key2
(integer) 0
redis> RENAMENX key1 key3
(integer) 1
```

|**时间复杂度:** O(1)

---

### COPY

将键复制到另一个数据库。

|**语法:**|
|----------|
```
COPY source destination [DB destination-db] [REPLACE]
```

|**参数:**|
|- `source`: 源键名|
|- `destination`: 目标键名|
|- `DB destination-db`: 目标数据库编号（可选，默认当前数据库）|
|- `REPLACE`: 如果目标键已存在则替换（可选）|

|**返回值:**|
|- 1: 复制成功|
|- 0: source 不存在|

|**示例:**|
```bash
redis> SET mykey "Hello"
OK
redis> COPY mykey mycopy
(integer) 1
redis> GET mycopy
"Hello"
redis> SELECT 1
OK
redis> COPY mykey mycopy DB 0
(integer) 1
```

|**时间复杂度:** O(N)，其中 N 是值的大小

---

### DUMP

序列化键对应的值。

|**语法:**|
|----------|
```
DUMP key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 序列化的值（可用于 RESTORE 命令）|
|- nil（如果键不存在）|

|**示例:**|
```bash
redis> SET mykey "Hello"
OK
redis> DUMP mykey
"\x00\x05Hello\t\x00\x00\x00\x00\x00\x00\x00\x00"
```

|**时间复杂度:** O(N)，其中 N 是值的大小

---

### RESTORE

使用序列化值创建键。

|**语法:**|
|----------|
```
RESTORE key ttl serialized-value [REPLACE] [ABSTTL]
```

|**参数:**|
|- `key`: 键名|
|- `ttl`: 过期时间（毫秒，0 表示不过期）|
|- `serialized-value`: DUMP 命令生成的序列化值|
|- `REPLACE`: 如果键已存在则替换|
|- `ABSTTL`: ttl 是绝对时间戳（毫秒）|

|**返回值:**|
|- OK|

|**示例:**|
```bash
redis> DUMP mykey
"\x00\x05Hello\t\x00\x00\x00\x00\x00\x00\x00\x00"
redis> RESTORE newkey 0 "\x00\x05Hello\t\x00\x00\x00\x00\x00\x00\x00\x00"
OK
redis> GET newkey
"Hello"
```

|**时间复杂度:** O(N)，其中 N 是值的大小

---

### MIGRATE

原子性地将键从当前实例传输到目标实例。

|**语法:**|
|----------|
```
MIGRATE host port key|"" destination-db timeout [COPY] [REPLACE] [AUTH password] [KEYS key [key ...]]
```

|**参数:**|
|- `host`: 目标主机|
|- `port`: 目标端口|
|- `key`: 要迁移的键名（为空时使用 KEYS 参数）|
|- `destination-db`: 目标数据库编号|
|- `timeout`: 超时时间（毫秒）|
|- `COPY`: 不删除源键（复制模式）|
|- `REPLACE`: 替换目标实例中已存在的键|
|- `AUTH password`: 认证密码|
|- `KEYS key [...]`: 要迁移的多个键|

|**返回值:**|
|- OK（迁移成功）|
|- NOKEY（没有键需要迁移）|

|**示例:**|
```bash
redis> MIGRATE 192.168.1.100 6379 mykey 0 1000
OK
redis> MIGRATE 192.168.1.100 6379 "" 0 1000 KEYS key1 key2
OK
```

|**时间复杂度:** O(N)，其中 N 是要迁移键的数量和大小

---

### EXPIRE

设置键的过期时间（秒）。

|**语法:**|
|----------|
```
EXPIRE key seconds
```

|**参数:**|
|- `key`: 键名|
|- `seconds`: 过期时间（秒）|

|**返回值:**|
|- 1: 过期时间设置成功|
|- 0: 键不存在或无法设置过期时间|

|**示例:**|
```bash
redis> SET mykey "Hello"
OK
redis> EXPIRE mykey 10
(integer) 1
redis> TTL mykey
(integer) 10
redis> GET mykey
"Hello"
# 10秒后
redis> GET mykey
(nil)
```

|**时间复杂度:** O(1)

---

### EXPIREAT

设置键的过期时间点（UNIX 时间戳，秒）。

|**语法:**|
|----------|
```
EXPIREAT key timestamp
```

|**参数:**|
|- `key`: 键名|
|- `timestamp`: UNIX 时间戳（秒）|

|**返回值:**|
|- 1: 过期时间设置成功|
|- 0: 键不存在或时间已过期|

|**示例:**|
```bash
redis> SET mykey "Hello"
OK
redis> EXPIREAT mykey 1704067200  # 2024-01-01 00:00:00 UTC
(integer) 1
redis> TTL mykey
(integer) 86400
```

|**时间复杂度:** O(1)

---

### PEXPIRE

设置键的过期时间（毫秒）。

|**语法:**|
|----------|
```
PEXPIRE key milliseconds
```

|**参数:**|
|- `key`: 键名|
|- `milliseconds`: 过期时间（毫秒）|

|**返回值:**|
|- 1: 过期时间设置成功|
|- 0: 键不存在|

|**示例:**|
```bash
redis> SET mykey "Hello"
OK
redis> PEXPIRE mykey 5000
(integer) 1
redis> PTTL mykey
(integer) 5000
```

|**时间复杂度:** O(1)

---

### PEXPIREAT

设置键的过期时间点（UNIX 时间戳，毫秒）。

|**语法:**|
|----------|
```
PEXPIREAT key milliseconds-timestamp
```

|**参数:**|
|- `key`: 键名|
|- `milliseconds-timestamp`: UNIX 时间戳（毫秒）|

|**返回值:**|
|- 1: 过期时间设置成功|
|- 0: 键不存在|

|**示例:**|
```bash
redis> SET mykey "Hello"
OK
redis> PEXPIREAT mykey 1704067200000  # 2024-01-01 00:00:00.000 UTC
(integer) 1
```

|**时间复杂度:** O(1)

---

### TTL

返回键的剩余生存时间（秒）。

|**语法:**|
|----------|
```
TTL key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 剩余时间（秒）|
|- -1: 键存在但没有设置过期时间|
|- -2: 键不存在|

|**示例:**|
```bash
redis> SET mykey "Hello"
OK
redis> TTL mykey
(integer) -1
redis> EXPIRE mykey 100
(integer) 1
redis> TTL mykey
(integer) 98
redis> DEL mykey
(integer) 1
redis> TTL mykey
(integer) -2
```

|**时间复杂度:** O(1)

---

### PTTL

返回键的剩余生存时间（毫秒）。

|**语法:**|
|----------|
```
PTTL key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 剩余时间（毫秒）|
|- -1: 键存在但没有设置过期时间|
|- -2: 键不存在|

|**示例:**|
```bash
redis> SET mykey "Hello"
OK
redis> PTTL mykey
(integer) -1
redis> EXPIRE mykey 100000
(integer) 1
redis> PTTL mykey
(integer) 99800
```

|**时间复杂度:** O(1)

---

### PERSIST

移除键的过期时间，使键永久存在。

|**语法:**|
|----------|
```
PERSIST key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 1: 成功移除过期时间|
|- 0: 键不存在或没有设置过期时间|

|**示例:**|
```bash
redis> SET mykey "Hello" EX 100
OK
redis> TTL mykey
(integer) 98
redis> PERSIST mykey
(integer) 1
redis> TTL mykey
(integer) -1
```

|**时间复杂度:** O(1)

---

### EXPIRETIME

返回键的过期时间点（UNIX 时间戳，秒）。

|**语法:**|
|----------|
```
EXPIRETIME key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 过期时间戳（秒）|
|- -1: 键存在但没有设置过期时间|
|- -2: 键不存在|

|**示例:**|
```bash
redis> SET mykey "Hello" EX 3600
OK
redis> EXPIRETIME mykey
(integer) 1704067200
```

|**时间复杂度:** O(1)

---

### PEXPIRETIME

返回键的过期时间点（UNIX 时间戳，毫秒）。

|**语法:**|
|----------|
```
PEXPIRETIME key
```

|**参数:**|
|- `key`: 键名|

|**返回值:**|
|- 过期时间戳（毫秒）|
|- -1: 键存在但没有设置过期时间|
|- -2: 键不存在|

|**示例:**|
```bash
redis> SET mykey "Hello" PX 3600000
OK
redis> PEXPIRETIME mykey
(integer) 1704067200000
```

|**时间复杂度:** O(1)

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

## Server 命令

Server（服务器）命令用于获取服务器信息、管理配置、持久化数据等操作。

### INFO

获取服务器信息和统计信息。

|**语法:**|
|----------|
```
INFO [section]
```

|**参数:**|
|- `section`: 信息 section（可选，如 server, clients, memory, stats 等）|

|**返回值:**|
|- 格式化的服务器信息字符串|

|**示例:**|
```bash
redis> INFO server
# Server
redis_version:7.2.4
redis_mode:standalone
os:Linux 5.4.0 x86_64
tcp_port:6379

redis> INFO memory
# Memory
used_memory:1024000
used_memory_human:1000.00K
```

|**时间复杂度:** O(1)

---

### CONFIG GET

获取配置参数的值。

|**语法:**|
|----------|
```
CONFIG GET parameter
```

|**参数:**|
|- `parameter`: 配置参数名（支持 * 匹配所有参数）|

|**返回值:**|
|- 包含参数名和值对的数组|

|**示例:**|
```bash
redis> CONFIG GET port
1) "port"
2) "6379"
redis> CONFIG GET *
1) "server"
2) "aikv"
3) "version"
4) "0.1.0"
5) "port"
6) "6379"
```

|**时间复杂度:** O(N)，其中 N 是返回的配置参数数量

---

### CONFIG SET

设置配置参数的值。

|**语法:**|
|----------|
```
CONFIG SET parameter value
```

|**参数:**|
|- `parameter`: 配置参数名|
|- `value`: 参数值|

|**返回值:**|
|- OK|

|**示例:**|
```bash
redis> CONFIG SET slowlog-log-slower-than 1000
OK
redis> CONFIG GET slowlog-log-slower-than
1) "slowlog-log-slower-than"
2) "1000"
```

|**时间复杂度:** O(1)

---

### SLOWLOG

管理慢查询日志。

|**语法:**|
|----------|
```
SLOWLOG subcommand [argument]
```

|**子命令:**|
|- `GET [count]`: 获取最近的慢查询日志|
|- `LEN`: 获取慢查询日志的长度|
|- `RESET`: 清空慢查询日志|
|- `HELP`: 显示帮助信息|

|**返回值:**|
|- 根据子命令返回相应的结果|

|**示例:**|
```bash
redis> SLOWLOG GET 10
1) 1) (integer) 1
   2) (integer) 1704067200
   3) (integer) 10000
   4) 1) "GET"
   5) "mykey"
   6) ""
   7) ""
redis> SLOWLOG LEN
(integer) 5
redis> SLOWLOG RESET
OK
```

|**时间复杂度:** O(1) 除 GET 外

---

### TIME

返回当前服务器时间。

|**语法:**|
|----------|
```
TIME
```

|**返回值:**|
|- 包含两个元素的数组：[Unix 时间戳, 微秒数]|

|**示例:**|
```bash
redis> TIME
1) "1704067200"
2) "123456"
```

|**时间复杂度:** O(1)

---

### CLIENT LIST

返回所有客户端连接的列表。

|**语法:**|
|----------|
```
CLIENT LIST
```

|**返回值:**|
|- 包含所有客户端连接信息的字符串|

|**示例:**|
```bash
redis> CLIENT LIST
id=1 addr=127.0.0.1:52345 name= age=10 idle=0
id=2 addr=127.0.0.1:52346 name= age=5 idle=2
```

|**时间复杂度:** O(N)，其中 N 是客户端数量

---

### CLIENT SETNAME

为当前连接设置名称。

|**语法:**|
|----------|
```
CLIENT SETNAME name
```

|**参数:**|
|- `name`: 连接名称|

|**返回值:**|
|- OK|

|**示例:**|
```bash
redis> CLIENT SETNAME my-connection
OK
redis> CLIENT LIST
id=1 addr=127.0.0.1:52345 name=my-connection age=10 idle=0
```

|**时间复杂度:** O(1)

---

### SAVE

同步保存数据集到磁盘。

|**语法:**|
|----------|
```
SAVE
```

|**返回值:**|
|- OK|

|**注意:** SAVE 命令会阻塞服务器直到 RDB 文件创建完成。在生产环境中建议使用 BGSAVE。|

|**示例:**|
```bash
redis> SAVE
OK
```

|**时间复杂度:** O(N)，其中 N 是数据集大小

---

### BGSAVE

异步保存数据集到磁盘。

|**语法:**|
|----------|
```
BGSAVE
```

|**返回值:**|
|- OK（或者 "Background saving started"）|

|**示例:**|
```bash
redis> BGSAVE
"Background saving started"
```

|**时间复杂度:** O(N)，在后台异步执行

---

### LASTSAVE

返回最后一次成功保存的时间戳。

|**语法:**|
|----------|
```
LASTSAVE
```

|**返回值:**|
|- Unix 时间戳（秒）|

|**示例:**|
```bash
redis> LASTSAVE
(integer) 1704067200
```

|**时间复杂度:** O(1)

---

### SHUTDOWN

关闭服务器。

|**语法:**|
|----------|
```
SHUTDOWN [NOSAVE|SAVE] [NOW] [FORCE] [ABORT]
```

|**参数:**|
|- `NOSAVE`: 不保存数据|
|- `SAVE`: 保存数据后再关闭|
|- `NOW`: 立即关闭|
|- `FORCE`: 即使有错误也强制关闭|
|- `ABORT`: 取消待执行的关闭操作|

|**返回值:**|
|- 无返回值（服务器会关闭连接）|

|**注意:** 如果未指定 NOSAVE 或 SAVE，取决于是否有未保存的修改。|

|**示例:**|
```bash
redis> SHUTDOWN SAVE
```

|**时间复杂度:** O(N)，取决于持久化操作

---

### COMMAND

返回所有 AiKv 命令的详细信息。

|**语法:**|
|----------|
```
COMMAND
```

|**返回值:**|
|- 包含所有命令信息的数组|

|**示例:**|
```bash
redis> COMMAND
1) 1) "get"
   2) (integer) 2
   3) 1) "readonly"
   4) (integer) 1
   5) (integer) 1
   6) (integer) 1
2) 1) "set"
   3) (integer) -3
...
```

|**时间复杂度:** O(N)，其中 N 是命令数量

---

### DBSIZE

返回当前数据库中的键数量。

|**语法:**|
|----------|
```
DBSIZE
```

|**返回值:**|
|- 键数量|

|**示例:**|
```bash
redis> SET key1 "Hello"
OK
redis> DBSIZE
(integer) 1
```

|**时间复杂度:** O(1)

---

### FLUSHDB

删除当前数据库中的所有键。

|**语法:**|
|----------|
```
FLUSHDB
```

|**返回值:**|
|- OK|

|**注意:** 此操作不可撤销，所有数据将被永久删除。|

|**示例:**|
```bash
redis> DBSIZE
(integer) 10
redis> FLUSHDB
OK
redis> DBSIZE
(integer) 0
```

|**时间复杂度:** O(N)，其中 N 是数据库中的键数量

---

### FLUSHALL

删除所有数据库中的所有键。

|**语法:**|
|----------|
```
FLUSHALL
```

|**返回值:**|
|- OK|

|**注意:** 此操作不可撤销，所有数据库的数据将被永久删除。|

|**示例:**|
```bash
redis> FLUSHALL
OK
```

|**时间复杂度:** O(N)，其中 N 是所有数据库中的键数量

---

### SWAPDB

交换两个数据库的内容。

|**语法:**|
|----------|
```
SWAPDB index1 index2
```

|**参数:**|
|- `index1`: 第一个数据库索引|
|- `index2`: 第二个数据库索引|

|**返回值:**|
|- OK|

|**示例:**|
```bash
redis> SELECT 0
OK
redis> SET key1 "Hello"
OK
redis> SELECT 1
OK
redis> SET key2 "World"
OK
redis> SWAPDB 0 1
OK
redis> GET key1
(nil)
redis> GET key2
"World"
redis> SELECT 0
OK
redis> GET key2
"World"
```

|**时间复杂度:** O(N)，其中 N 是两个数据库中较大的那个的键数量

---

### MOVE

将键移动到另一个数据库。

|**语法:**|
|----------|
```
MOVE key db
```

|**参数:**|
|- `key`: 键名|
|- `db`: 目标数据库编号|

|**返回值:**|
|- 1: 移动成功|
|- 0: 键不存在或目标数据库中已存在同名键|

|**示例:**|
```bash
redis> SET mykey "Hello"
OK
redis> MOVE mykey 1
(integer) 1
redis> GET mykey
(nil)
redis> SELECT 1
OK
redis> GET mykey
"Hello"
```

|**时间复杂度:** O(1)

---

### SELECT

切换到指定的数据库。

|**语法:**|
|----------|
```
SELECT index
```

|**参数:**|
|- `index`: 数据库索引（0-15）|

|**返回值:**|
|- OK|

|**示例:**|
```bash
redis> SELECT 0
OK
redis> SELECT 1
OK
```

|**时间复杂度:** O(1)

---

## Script 命令

Script（脚本）命令用于执行 Lua 脚本，利用 Redis 的 Lua 解释器执行原子操作。

### EVAL

执行 Lua 脚本。

|**语法:**|
|----------|
```
EVAL script numkeys [key [key ...]] [arg [arg ...]]
```

|**参数:**|
|- `script`: Lua 脚本字符串|
|- `numkeys`: 脚本中使用的键的数量|
|- `key`: 脚本中使用的键（可选）|
|- `arg`: 脚本参数（可选）|

|**返回值:**|
|- 脚本的返回值（根据脚本内容）|

|**示例:**|
```bash
redis> EVAL "return {KEYS[1],KEYS[2],ARGV[1],ARGV[2]}" 2 key1 key2 arg1 arg2
1) "key1"
2) "key2"
3) "arg1"
4) "arg2"

redis> EVAL "return redis.call('set',KEYS[1],ARGV[1])" 1 mykey "Hello"
OK
```

|**时间复杂度:** 取决于脚本内容

---

### EVALSHA

通过脚本的 SHA1 摘要执行已缓存的 Lua 脚本。

|**语法:**|
|----------|
```
EVALSHA sha1 numkeys [key [key ...]] [arg [arg ...]]
```

|**参数:**|
|- `sha1`: 脚本的 SHA1 摘要|
|- `numkeys`: 脚本中使用的键的数量|
|- `key`: 脚本中使用的键（可选）|
|- `arg`: 脚本参数（可选）|

|**返回值:**|
|- 脚本的返回值（根据脚本内容）|

|**注意:** 只有当脚本已经通过 SCRIPT LOAD 加载到缓存中时，才能使用 EVALSHA。|

|**示例:**|
```bash
redis> SCRIPT LOAD "return redis.call('get','mykey')"
"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
redis> EVALSHA xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx 0
"myvalue"
```

|**时间复杂度:** 取决于脚本内容

---

### SCRIPT

管理 Lua 脚本。

|**语法:**|
|----------|
```
SCRIPT subcommand [argument [argument ...]]
```

|**子命令:**|
|- `LOAD script`: 加载脚本到缓存，返回 SHA1 摘要|
|- `EXISTS sha1 [sha1 ...]`: 检查脚本是否在缓存中|
|- `FLUSH`: 清空所有已缓存的脚本|
|- `KILL`: 终止当前正在运行的脚本|

|**返回值:**|
|- 根据子命令返回相应的结果|

|**示例:**|
```bash
redis> SCRIPT LOAD "return 'hello'"
"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
redis> SCRIPT EXISTS xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
1) (integer) 1
redis> SCRIPT FLUSH
OK
redis> SCRIPT EXISTS xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
1) (integer) 0
```

|**时间复杂度:** O(N)，其中 N 是检查的 SHA1 数量（EXISTS 子命令）

---

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
