# Lua Scripting Support

AiKv supports Lua scripting for executing complex operations atomically on the server side, similar to Redis.

## Overview

Lua scripting in AiKv provides:
- **Atomic execution**: Scripts run atomically without interruption
- **Transactional semantics**: All operations within a script are buffered and committed together, or rolled back on error
- **Server-side computation**: Reduce network round trips by executing logic on the server
- **Script caching**: Scripts can be loaded once and executed multiple times
- **Redis compatibility**: Supports Redis-style scripting API

## Transaction Support ✅ (已实现)

AiKv implements automatic rollback for Lua scripts using a **write-buffer + AiDb WriteBatch** approach:

### 实现架构

```
┌─────────────────────────────────────────────────────────────┐
│                    Lua Script Execution                      │
├─────────────────────────────────────────────────────────────┤
│                                                               │
│   1. 创建 ScriptTransaction (内存写缓冲区)                    │
│                         ↓                                     │
│   2. 执行脚本中的 Redis 命令                                  │
│      • redis.call('SET', ...) → 写入 write_buffer            │
│      • redis.call('GET', ...) → 先查 write_buffer → 再查存储 │
│      • redis.call('DEL', ...) → 标记 DELETE 到 write_buffer  │
│                         ↓                                     │
│   3a. 脚本成功 → storage.write_batch(operations)             │
│       └→ AiDb WriteBatch: WAL 原子写入 + 单次 fsync          │
│                                                               │
│   3b. 脚本失败 → transaction 被 drop                          │
│       └→ write_buffer 丢弃 → 自动回滚                        │
│                                                               │
└─────────────────────────────────────────────────────────────┘
```

### 核心特性

- **Write buffering**: All write operations (SET, DEL) within a script are first written to an in-memory buffer
- **Read-your-own-writes**: Read operations (GET, EXISTS) can see writes made earlier in the same script
- **Automatic commit**: When a script completes successfully, all buffered writes are committed to storage atomically via `write_batch()`
- **Automatic rollback**: If a script fails (Lua error, unsupported command, etc.), all buffered writes are discarded, ensuring data consistency
- **AiDb WriteBatch**: Uses AiDb's native batch write API for true atomic persistence with WAL durability

### AiDb WriteBatch 保证

| 特性 | 说明 |
|------|------|
| **WAL 原子性** | 所有操作先写 WAL，失败则整个 batch 回滚 |
| **单次 fsync** | 整个 batch 只需一次磁盘同步，性能最优 |
| **崩溃恢复** | 进程崩溃后 WAL replay 保证数据完整性 |
| **持久化保证** | 数据不会因崩溃而丢失 |

### Example: Transaction Commit

```lua
-- Script succeeds - changes are committed
EVAL "
  redis.call('SET', 'key1', 'value1')
  redis.call('SET', 'key2', 'value2')
  return 'OK'
" 0
-- After execution, both key1 and key2 are in storage
```

### Example: Transaction Rollback

```lua
-- Script fails - changes are rolled back
EVAL "
  redis.call('SET', 'key1', 'value1')
  redis.call('SET', 'key2', 'value2')
  error('something went wrong')
" 0
-- After execution, neither key1 nor key2 exist in storage
```

### Example: Read Your Own Writes

```lua
-- Read operations see buffered writes
EVAL "
  redis.call('SET', 'mykey', 'first')
  local v1 = redis.call('GET', 'mykey')  -- Returns 'first'
  redis.call('SET', 'mykey', 'second')
  local v2 = redis.call('GET', 'mykey')  -- Returns 'second'
  return {v1, v2}
" 0
-- Returns: ['first', 'second']
-- Storage contains: mykey = 'second'
```

## Supported Commands

### EVAL

Execute a Lua script.

**Syntax:**
```
EVAL script numkeys [key [key ...]] [arg [arg ...]]
```

**Arguments:**
- `script`: The Lua script to execute
- `numkeys`: Number of keys that follow
- `key`: Key names (accessible as `KEYS[1]`, `KEYS[2]`, etc. in the script)
- `arg`: Additional arguments (accessible as `ARGV[1]`, `ARGV[2]`, etc. in the script)

**Example:**
```lua
EVAL "return KEYS[1]" 1 mykey
EVAL "return ARGV[1] * 2" 0 21
```

### EVALSHA

Execute a previously cached script by its SHA1 digest.

**Syntax:**
```
EVALSHA sha1 numkeys [key [key ...]] [arg [arg ...]]
```

**Example:**
```
SCRIPT LOAD "return 'hello'"
# Returns: "1b936e3fe509bcbc9cd0664897bbe8fd0cac101b"

EVALSHA 1b936e3fe509bcbc9cd0664897bbe8fd0cac101b 0
# Returns: "hello"
```

### SCRIPT LOAD

Load a script into the cache without executing it.

**Syntax:**
```
SCRIPT LOAD script
```

**Returns:** The SHA1 digest of the script

**Example:**
```
SCRIPT LOAD "return 'cached'"
# Returns: SHA1 hash of the script
```

### SCRIPT EXISTS

Check if scripts exist in the cache.

**Syntax:**
```
SCRIPT EXISTS sha1 [sha1 ...]
```

**Returns:** Array of integers (1 if exists, 0 if not)

**Example:**
```
SCRIPT EXISTS 1b936e3fe509bcbc9cd0664897bbe8fd0cac101b
# Returns: [1] or [0]
```

### SCRIPT FLUSH

Clear all cached scripts.

**Syntax:**
```
SCRIPT FLUSH [ASYNC|SYNC]
```

**Returns:** OK

**Example:**
```
SCRIPT FLUSH
# Returns: OK
```

### SCRIPT KILL

Terminate a currently running script (returns NOTBUSY if no script is running).

**Syntax:**
```
SCRIPT KILL
```

**Note:** In the current implementation, this command returns NOTBUSY since scripts execute atomically.

## Lua Environment

### Global Variables

Scripts have access to:
- `KEYS`: Array (1-indexed) of key names passed to the script
- `ARGV`: Array (1-indexed) of additional arguments passed to the script
- `redis`: Table with Redis command functions

### Redis Commands

Scripts can call Redis commands using:

#### redis.call()

Execute a Redis command. Throws an error if the command fails.

**Syntax:**
```lua
redis.call('command', arg1, arg2, ...)
```

**Example:**
```lua
redis.call('SET', KEYS[1], ARGV[1])
local value = redis.call('GET', KEYS[1])
return value
```

#### redis.pcall()

Protected call - executes a Redis command but returns error as a result instead of throwing.

**Syntax:**
```lua
redis.pcall('command', arg1, arg2, ...)
```

### Supported Commands in Scripts

### Supported Commands in Scripts ✅

Scripts can execute the following Redis commands (33 total):

**String Commands (11):**
- `GET`: Get a value by key
- `SET`: Set a key-value pair
- `DEL`: Delete one or more keys
- `EXISTS`: Check if keys exist
- `INCR`: Increment the integer value of a key
- `DECR`: Decrement the integer value of a key
- `INCRBY`: Increment by a specific amount
- `DECRBY`: Decrement by a specific amount
- `INCRBYFLOAT`: Increment by a floating point value
- `APPEND`: Append a value to a key
- `STRLEN`: Get the length of a string value

**Hash Commands (9):**
- `HGET`: Get a field from a hash
- `HSET`: Set field(s) in a hash
- `HDEL`: Delete field(s) from a hash
- `HGETALL`: Get all fields and values
- `HMGET`: Get multiple fields
- `HMSET`: Set multiple fields
- `HINCRBY`: Increment a field by an integer
- `HEXISTS`: Check if a field exists
- `HLEN`: Get the number of fields

**List Commands (7):**
- `LPUSH`: Push to the left/head of a list
- `RPUSH`: Push to the right/tail of a list
- `LPOP`: Pop from the left/head
- `RPOP`: Pop from the right/tail
- `LLEN`: Get the length of a list
- `LRANGE`: Get a range of elements
- `LINDEX`: Get an element by index

**Set Commands (5):**
- `SADD`: Add members to a set
- `SREM`: Remove members from a set
- `SMEMBERS`: Get all members
- `SISMEMBER`: Check if a member exists
- `SCARD`: Get the number of members

**Sorted Set Commands (6):**
- `ZADD`: Add members with scores
- `ZREM`: Remove members
- `ZSCORE`: Get the score of a member
- `ZRANK`: Get the rank of a member
- `ZRANGE`: Get a range of members by rank
- `ZCARD`: Get the number of members

## Type Conversions

### Lua to RESP

| Lua Type | RESP Type |
|----------|-----------|
| `nil` | Null |
| `boolean` | Integer (1 for true, 0 for false) |
| `number` (integer) | Integer |
| `number` (float) | Bulk String |
| `string` | Bulk String |
| `table` (array) | Array |

### RESP to Lua

| RESP Type | Lua Type |
|-----------|----------|
| Null | `false` |
| Simple String | string |
| Error | string |
| Integer | number |
| Bulk String | string or `false` (for null) |
| Array | table (1-indexed) or `false` (for null) |

## Examples

### Simple Script

Return a constant value:
```lua
EVAL "return 42" 0
# Returns: 42
```

### Using KEYS

Access key names passed to the script:
```lua
EVAL "return KEYS[1]" 1 mykey
# Returns: "mykey"
```

### Using ARGV

Access arguments passed to the script:
```lua
EVAL "return ARGV[1] * 2" 0 21
# Returns: 42
```

### Redis Commands

Set and get a value:
```lua
EVAL "redis.call('SET', KEYS[1], ARGV[1]); return redis.call('GET', KEYS[1])" 1 mykey myvalue
# Returns: "myvalue"
```

### Complex Logic

Calculate and store a result:
```lua
EVAL "
local count = redis.call('EXISTS', KEYS[1])
if count == 1 then
    return redis.call('GET', KEYS[1])
else
    redis.call('SET', KEYS[1], 'default')
    return 'default'
end
" 1 mykey
```

### Script Caching

Load a script once and execute it multiple times:
```lua
# Load the script
SCRIPT LOAD "return redis.call('GET', KEYS[1])"
# Returns: "a9b7f1c8e2d3a4b5c6d7e8f9a0b1c2d3e4f5a6b7"

# Execute using the SHA1
EVALSHA a9b7f1c8e2d3a4b5c6d7e8f9a0b1c2d3e4f5a6b7 1 mykey
```

## Best Practices

1. **Use KEYS and ARGV**: Always parameterize your scripts using KEYS and ARGV instead of hardcoding values
2. **Cache scripts**: For frequently used scripts, use SCRIPT LOAD and EVALSHA to reduce network overhead
3. **Keep scripts simple**: Complex logic can make debugging difficult
4. **Error handling**: Use redis.pcall() when you want to handle errors gracefully
5. **Test thoroughly**: Test scripts with various inputs before production use
6. **Use KEYS parameter**: For optimal parallel execution, declare all keys your script accesses via the KEYS parameter

## Key-Level Locking ✅ (已实现)

AiKv implements key-level locking for parallel script execution. This allows scripts operating on different keys to execute in parallel while ensuring data consistency for scripts accessing the same keys.

### 设计架构

```
┌─────────────────────────────────────────────────────────────┐
│                    Key-Level Locking                         │
├─────────────────────────────────────────────────────────────┤
│                                                               │
│   EVAL script 2 key1 key2 arg1 arg2                          │
│                         ↓                                     │
│   KeyLockManager.lock_keys([key1, key2])                     │
│                         ↓                                     │
│   ┌─────────────────────────────────────────────┐            │
│   │ key1: 已被其他脚本锁定 → 等待 (Condvar)      │            │
│   │ key2: 空闲 → 加锁成功                        │            │
│   └─────────────────────────────────────────────┘            │
│                         ↓                                     │
│   执行脚本 (ScriptTransaction)                               │
│                         ↓                                     │
│   KeyLockManager.unlock_keys([key1, key2])                   │
│                                                               │
└─────────────────────────────────────────────────────────────┘
```

### 并行执行收益

| 场景 | 行为 |
|------|------|
| 两个脚本操作不同 keys | **并行执行** |
| 两个脚本操作相同 key | 串行执行 (保证一致性) |
| 无 KEYS 参数的脚本 | 无需等待，立即执行 |

### 锁特性

- **超时机制**: 默认 30 秒锁超时，防止死锁
- **公平调度**: 使用 Condvar 实现公平的等待队列
- **自动释放**: 使用 RAII 模式，脚本执行完毕自动释放锁
- **键排序**: 锁获取前自动排序键，防止交叉死锁

## Transaction Support for All Data Types ✅ (已实现)

Scripts now support transactional operations for all Redis data types:

### 支持的数据类型

| 数据类型 | 读操作 | 写操作 | 事务回滚 |
|----------|--------|--------|----------|
| String | ✅ | ✅ | ✅ |
| Hash | ✅ | ✅ | ✅ |
| List | ✅ | ✅ | ✅ |
| Set | ✅ | ✅ | ✅ |
| Sorted Set | ✅ | ✅ | ✅ |

### 示例：复杂类型事务

```lua
-- 所有操作在一个事务中执行
EVAL "
    redis.call('HSET', KEYS[1], 'field1', ARGV[1])
    redis.call('LPUSH', KEYS[2], ARGV[2])
    redis.call('SADD', KEYS[3], ARGV[3])
    -- 如果这里发生错误，上述所有操作都会回滚
    return 'OK'
" 3 myhash mylist myset value1 value2 value3
```

## Limitations

Current limitations:
- No timeout mechanism for long-running scripts
- SCRIPT KILL is not functional in the current implementation
- No support for script debugging
- Complex type operations (Hash, List, Set, ZSet) are committed individually, not atomically across types. Key-level locking ensures no other script can observe partial state during normal operation, but a crash during commit could result in partial writes.

## Performance Considerations

- Scripts execute atomically with key-level locking
- Scripts operating on different keys can execute in parallel
- Script caching (EVALSHA) is more efficient than EVAL for repeated executions
- The SHA1 calculation overhead is minimal compared to network transfer
- Lua VM initialization is done per script execution
- Lock acquisition uses fair queuing with Condvar

## Security

- Scripts run in a sandboxed Lua environment with limited standard library access
- No access to file system or network operations
- No ability to load external Lua modules

## Technical Details

- **Lua Version**: Lua 5.4
- **Lua Library**: mlua v0.10 (with vendored Lua)
- **Hash Algorithm**: SHA1 for script caching
- **Standard Libraries**: TABLE, STRING, MATH, UTF8 only
- **Lock Timeout**: 30 seconds (configurable via `with_lock_timeout`)
- **Supported Commands**: 33 (String: 11, Hash: 9, List: 7, Set: 5, ZSet: 6)

## Related Documentation

- [API Commands](../api/01-commands.md) - EVAL, EVALSHA, SCRIPT commands
- [Best Practices](../../guide/04-best-practices.md) - Script optimization tips
- [Redis Lua Documentation](https://redis.io/docs/interact/programmability/lua-scripting/) - Redis Lua scripting reference

---

**Last Updated**: 2026-01-16  
**Version**: v0.1.0  
**Lua Version**: 5.4 (via mlua v0.10)  
**Supported Commands**: 38 (String: 11, Hash: 9, List: 7, Set: 5, ZSet: 6)  
**Maintained by**: @Genuineh
