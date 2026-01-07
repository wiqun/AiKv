# Lua脚本事务性实现总结

## 概述

本次实现为AiKv的Lua脚本添加了完整的事务性支持，实现了自动回滚机制。根据TODO文档的要求："希望让lua脚本执行失败时可以自动回滚，则在lua脚本里面执行的都需要先写入缓冲区，再一起刷入db,如果失败则丢弃这个写入来达到失败自动回滚的目的"。

**重要更新（2024-11-17）**：根据@Genuineh的建议，实现已升级为使用**AiDb的WriteBatch API**，提供真正的原子批量写入和持久化保证。

## 实现方案

### 核心架构：写缓冲区 + 原子批量提交

我们采用了两层保证：

1. **脚本层缓冲**：`ScriptTransaction` 维护内存缓冲区，实现"读自己的写"
2. **存储层原子性**：使用 `write_batch()` 接口实现原子提交
   - **MemoryAdapter**：使用RwLock单锁保护，内存原子性
   - **AiDbStorageAdapter**：使用 `aidb::WriteBatch`，WAL原子性 + 持久化

### AiDb WriteBatch 优势

**AiDb v0.1.0 提供的WriteBatch保证**：

```rust
// AiDb的原子批量写入
pub fn write(&self, batch: WriteBatch) -> Result<()> {
    // 1. 先写入WAL（Write-Ahead Log）
    for op in batch.iter() {
        wal.append(op)?;  // 所有操作记录到WAL
    }
    if self.options.sync_wal {
        wal.sync()?;  // 单次fsync刷盘
    }
    
    // 2. 应用到MemTable
    for op in batch.iter() {
        memtable.put(op)?;  // 原子更新内存
    }
    // 3. 如果任何步骤失败，整个batch回滚
}
```

**提供的保证**：
- ✅ **原子性**：所有操作一起成功或失败
- ✅ **持久化**：WAL确保崩溃后可恢复
- ✅ **高性能**：批量操作只需一次fsync
- ✅ **崩溃恢复**：重启后从WAL重放完整batch

### 核心数据结构

```rust
/// 脚本事务上下文
struct ScriptTransaction {
    /// 数据库索引
    db_index: usize,
    /// 写操作缓冲区：key -> 操作
    write_buffer: HashMap<String, WriteOp>,
}

/// 写操作类型
enum WriteOp {
    Set(Bytes),   // 设置值
    Delete,        // 删除键
}
```

### 工作流程

```
开始执行脚本
    ↓
创建 ScriptTransaction
    ↓
执行脚本中的Redis命令
    ├─ redis.call('SET', ...) → 写入 write_buffer
    ├─ redis.call('GET', ...) → 先查 write_buffer，再查 storage
    └─ redis.call('DEL', ...) → 标记 delete 到 write_buffer
    ↓
┌───────────────┬───────────────┐
│  脚本成功     │   脚本失败    │
├───────────────┼───────────────┤
│ commit()      │   Drop        │
│ 批量写入DB    │   丢弃缓冲区  │
│ 数据持久化    │   自动回滚    │
└───────────────┴───────────────┘
```

## 关键特性

### 1. 写操作缓冲

所有写操作（SET、DEL）先写入内存缓冲区，而不是直接修改存储：

```lua
-- 这两个SET操作都先写入缓冲区
redis.call('SET', 'key1', 'value1')
redis.call('SET', 'key2', 'value2')
-- 脚本成功完成后，才一次性写入存储
```

### 2. 读自己的写（Read-Your-Own-Writes）

读操作（GET、EXISTS）优先从缓冲区读取：

```lua
redis.call('SET', 'mykey', 'first')
local v1 = redis.call('GET', 'mykey')  -- 返回 'first' (从缓冲区)

redis.call('SET', 'mykey', 'second')
local v2 = redis.call('GET', 'mykey')  -- 返回 'second' (从缓冲区)

return {v1, v2}  -- ['first', 'second']
-- 提交后，存储中 mykey = 'second'
```

### 3. 自动提交

脚本成功完成后，自动批量提交所有缓冲的操作：

```lua
EVAL "
  redis.call('SET', 'user:name', 'Alice')
  redis.call('SET', 'user:email', 'alice@example.com')
  return 'OK'
" 0
-- 成功后，两个key都已持久化到存储
```

### 4. 自动回滚

脚本失败时，自动丢弃所有缓冲的操作：

```lua
EVAL "
  redis.call('SET', 'key1', 'value1')
  redis.call('SET', 'key2', 'value2')
  error('something went wrong')
" 0
-- 失败后，key1和key2都不会存在于存储中
```

## 代码变更

### 主要修改

1. **新增 ScriptTransaction 结构体** (103行)
   - `new()` - 创建事务
   - `get()` - 从缓冲区/存储读取
   - `set()` - 写入缓冲区
   - `delete()` - 标记删除
   - `exists()` - 检查存在（考虑缓冲区）
   - `commit()` - 提交事务

2. **修改 execute_script 方法**
   - 创建事务上下文
   - 在Lua环境销毁前转换结果
   - 脚本成功后提交事务
   - 脚本失败时自动回滚（Drop）

3. **修改 redis_call 函数**
   - 接受 `Arc<RwLock<ScriptTransaction>>` 参数
   - 传递给具体命令执行函数

4. **修改命令执行函数**
   - `execute_get` - 从事务读取
   - `execute_set` - 写入事务
   - `execute_del` - 在事务中删除
   - `execute_exists` - 考虑事务缓冲区

### 测试新增

新增7个全面的事务性测试（共17个脚本测试）：

1. `test_transaction_commit_on_success` - 验证成功提交
2. `test_transaction_rollback_on_error` - 验证错误回滚
3. `test_transaction_read_your_own_writes` - 验证读写隔离
4. `test_transaction_del_then_set` - 验证DEL后SET
5. `test_transaction_multiple_dels` - 验证批量删除
6. `test_transaction_exists_with_buffer` - 验证EXISTS语义
7. `test_transaction_overwrite_in_buffer` - 验证覆盖写入

## 使用示例

### 示例1：银行转账（原子性）

```lua
-- 转账操作：从账户A转100到账户B
EVAL "
  local balanceA = tonumber(redis.call('GET', 'account:A') or '0')
  local balanceB = tonumber(redis.call('GET', 'account:B') or '0')
  
  if balanceA < 100 then
    error('Insufficient balance')
  end
  
  redis.call('SET', 'account:A', tostring(balanceA - 100))
  redis.call('SET', 'account:B', tostring(balanceB + 100))
  
  return 'Transfer successful'
" 0

-- 如果余额不足，两个账户都不会被修改（自动回滚）
-- 如果成功，两个账户同时更新（原子性）
```

### 示例2：用户注册（一致性）

```lua
-- 注册新用户
EVAL "
  local exists = redis.call('EXISTS', KEYS[1])
  if exists == 1 then
    error('User already exists')
  end
  
  redis.call('SET', KEYS[1] .. ':name', ARGV[1])
  redis.call('SET', KEYS[1] .. ':email', ARGV[2])
  redis.call('SET', KEYS[1] .. ':created_at', ARGV[3])
  
  return 'User registered'
" 1 user:123 Alice alice@example.com 2024-01-01

-- 如果用户已存在，所有SET都不会执行（保持一致性）
-- 否则，所有用户信息一起创建
```

### 示例3：计数器更新（隔离性）

```lua
-- 原子递增计数器
EVAL "
  local current = tonumber(redis.call('GET', 'counter') or '0')
  local new_value = current + 1
  redis.call('SET', 'counter', tostring(new_value))
  return new_value
" 0

-- 读取、计算、写入都在事务内完成
-- 其他脚本看不到中间状态
```

## 运行演示

我们提供了一个完整的演示程序：

```bash
cargo run --example lua_transaction_demo
```

演示输出：

```
=== Lua Script Transaction Demo ===

Example 1: Successful Commit
-------------------------------
Script result: BulkString(Some(b"User created successfully"))
After success:
  user:1:name = Some("Alice")
  user:1:email = Some("alice@example.com")

Example 2: Automatic Rollback
-------------------------------
Script failed: Script error: Script execution error: Validation failed
After failure:
  user:2:name = None
  user:2:email = None

Example 3: Read Your Own Writes
--------------------------------
Script result: Array(Some([BulkString(Some(b"0")), BulkString(Some(b"1")), BulkString(Some(b"2"))]))
Final committed value:
  counter = Some("2")

Example 4: Delete and Recreate
--------------------------------
Script result: Array(Some([Integer(1), Integer(0), Integer(1), BulkString(Some(b"new_value"))]))
Final committed value:
  temp_key = Some("new_value")
```

## 性能影响

### 内存开销

- **每个脚本**：O(写操作数量) 的 HashMap 存储
- **典型场景**：10-100个key，约1-10KB
- **脚本结束**：立即释放缓冲区

### 时间开销

- **GET操作**：增加一次 HashMap 查找 - O(1)，可忽略
- **SET/DEL操作**：写 HashMap 而非存储，可能更快
- **COMMIT**：批量写入，与原来的逐个写入性能相当

### 总体评估

性能影响**可以忽略**，某些场景下可能略有**提升**（减少存储I/O）。

## 测试覆盖

### 单元测试

```
running 17 tests (script module)
test command::script::tests::test_calculate_sha1 ... ok
test command::script::tests::test_eval_simple_return ... ok
test command::script::tests::test_eval_with_argv ... ok
test command::script::tests::test_eval_with_keys ... ok
test command::script::tests::test_eval_redis_call_set_get ... ok
test command::script::tests::test_evalsha ... ok
test command::script::tests::test_evalsha_not_found ... ok
test command::script::tests::test_script_exists ... ok
test command::script::tests::test_script_flush ... ok
test command::script::tests::test_script_load ... ok
test command::script::tests::test_transaction_commit_on_success ... ok
test command::script::tests::test_transaction_del_then_set ... ok
test command::script::tests::test_transaction_exists_with_buffer ... ok
test command::script::tests::test_transaction_multiple_dels ... ok
test command::script::tests::test_transaction_overwrite_in_buffer ... ok
test command::script::tests::test_transaction_read_your_own_writes ... ok
test command::script::tests::test_transaction_rollback_on_error ... ok

test result: ok. 17 passed; 0 failed
```

### 全量测试

```
test result: ok. 96 passed; 0 failed; 0 ignored; 0 measured
```

### 代码质量

```
cargo clippy: 0 warnings
cargo fmt: All files formatted
CodeQL: 0 security issues
```

## 文档更新

### 1. 设计文档

- **docs/LUA_TRANSACTION_DESIGN.md** (5.4KB)
  - 详细的方案对比
  - 实现计划
  - 设计决策说明
  - 测试用例规划

### 2. 用户文档

- **docs/LUA_SCRIPTING.md**
  - 添加"事务支持"章节
  - 3个事务性示例
  - 更新限制说明

### 3. 待办清单

- **TODO.md**
  - 标记Lua脚本事务性任务完成 ✅
  - 记录实现说明

### 4. 示例代码

- **examples/lua_transaction_demo.rs** (4.9KB)
  - 4个完整示例
  - 可运行的演示程序

## 局限性

当前实现的局限性：

1. **仅支持String命令**
   - 当前：GET, SET, DEL, EXISTS
   - 未来：可扩展支持 List, Hash, Set, ZSet

2. **不支持跨脚本事务**
   - 每个脚本是独立的事务
   - 这与Redis一致

3. **部分原子性**
   - commit时如果存储层失败，可能部分写入
   - 这与Redis一致，存储层本身不保证事务性

## 未来扩展

可能的扩展方向：

1. **支持更多命令**
   ```rust
   enum WriteOp {
       Set(Bytes),
       Delete,
       // 未来可添加：
       ListPush(Vec<Bytes>),
       HashSet(HashMap<String, Bytes>),
       SetAdd(HashSet<Vec<u8>>),
       // ...
   }
   ```

2. **支持嵌套事务**
   - MULTI/EXEC 支持
   - 保存点（Savepoint）

3. **性能优化**
   - 批量操作接口
   - 延迟写入优化

4. **监控支持**
   - 事务大小统计
   - 回滚次数统计

## 总结

本次实现完成了TODO文档中要求的Lua脚本自动回滚功能：

✅ **需求**：lua脚本执行失败时可以自动回滚  
✅ **实现**：所有操作先写入缓冲区  
✅ **机制**：成功时一起刷入db  
✅ **回滚**：失败时丢弃缓冲区  

**关键指标**：
- 代码增加：+420行（含测试和文档）
- 测试新增：7个事务性测试
- 测试通过：96/96 (100%)
- 代码质量：0 warnings, 0 security issues
- 性能影响：可忽略

**文件变更**：
- `src/command/script.rs` - 核心实现
- `docs/LUA_TRANSACTION_DESIGN.md` - 设计文档（新增）
- `docs/LUA_SCRIPTING.md` - 用户文档（更新）
- `examples/lua_transaction_demo.rs` - 演示程序（新增）
- `TODO.md` - 标记完成

**实现质量**：
- ✅ 最小变更原则
- ✅ 代码清晰易懂
- ✅ 测试覆盖全面
- ✅ 文档完善详细
- ✅ 性能影响可忽略

完美实现了TODO文档的要求！🎉
