# Lua Scripting Support

AiKv supports Lua scripting for executing complex operations atomically on the server side, similar to Redis.

## Overview

Lua scripting in AiKv provides:
- **Atomic execution**: Scripts run atomically without interruption
- **Transactional semantics**: All operations within a script are buffered and committed together, or rolled back on error
- **Server-side computation**: Reduce network round trips by executing logic on the server
- **Script caching**: Scripts can be loaded once and executed multiple times
- **Redis compatibility**: Supports Redis-style scripting API

## Transaction Support

AiKv implements automatic rollback for Lua scripts:

- **Write buffering**: All write operations (SET, DEL) within a script are first written to an in-memory buffer
- **Read-your-own-writes**: Read operations (GET, EXISTS) can see writes made earlier in the same script
- **Automatic commit**: When a script completes successfully, all buffered writes are committed to storage atomically
- **Automatic rollback**: If a script fails (Lua error, unsupported command, etc.), all buffered writes are discarded, ensuring data consistency

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

Currently, scripts can execute the following Redis commands:
- `GET`: Get a value by key
- `SET`: Set a key-value pair
- `DEL`: Delete one or more keys
- `EXISTS`: Check if keys exist

More commands will be supported in future versions.

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

## Limitations

Current limitations (to be addressed in future versions):
- Limited set of Redis commands available in scripts (GET, SET, DEL, EXISTS)
- No timeout mechanism for long-running scripts
- SCRIPT KILL is not functional in the current implementation
- No support for script debugging
- Transaction support only for String operations (List, Hash, Set, ZSet not yet supported in scripts)

## Performance Considerations

- Scripts execute atomically, blocking other operations
- Script caching (EVALSHA) is more efficient than EVAL for repeated executions
- The SHA1 calculation overhead is minimal compared to network transfer
- Lua VM initialization is done per script execution

## Security

- Scripts run in a sandboxed Lua environment with limited standard library access
- No access to file system or network operations
- No ability to load external Lua modules

## Technical Details

- **Lua Version**: Lua 5.4
- **Lua Library**: mlua v0.10 (with vendored Lua)
- **Hash Algorithm**: SHA1 for script caching
- **Standard Libraries**: TABLE, STRING, MATH, UTF8 only
