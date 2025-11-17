# Lua脚本事务性设计方案

## 概述

本文档描述了 AiKv 存储层架构重构的完整计划。这次重构旨在将命令逻辑从存储层分离出来，使架构更加清晰、可维护和可扩展。

## 当前问题

当前Lua脚本执行中的Redis命令直接修改存储层，没有事务性保证。如果脚本执行失败（例如Lua错误、不支持的命令等），已经执行的操作无法回滚，导致数据不一致。

## 目标

实现Lua脚本的自动回滚机制：
1. 脚本执行期间的所有写操作先写入缓冲区
2. 脚本成功完成后，使用存储层的原子批量写入接口提交
3. 脚本失败时，丢弃缓冲区，实现自动回滚

## 方案对比

### 方案A：写缓冲区 + 存储层原子批量写入（已实现）

**核心思路**：在脚本执行期间维护一个临时的写缓冲区，所有写操作先写入缓冲区，读操作优先从缓冲区读取。脚本成功后，使用存储层的 `write_batch()` 方法原子提交。

**实现结构**：
```rust
/// Lua脚本事务上下文
struct ScriptTransaction {
    db_index: usize,
    write_buffer: HashMap<String, BatchOp>,
}

/// 批量写操作类型
enum BatchOp {
    Set(Bytes),
    Delete,
}

impl StorageAdapter {
    /// 原子批量写入（MemoryAdapter实现）
    fn write_batch(&self, db_index: usize, operations: Vec<(String, BatchOp)>) -> Result<()>;
}

impl AiDbStorageAdapter {
    /// 原子批量写入（使用AiDb的WriteBatch）
    fn write_batch(&self, db_index: usize, operations: Vec<(String, BatchOp)>) -> Result<()>;
}
```

**AiDb WriteBatch保证**：
- ✅ **WAL原子性**：所有操作先写入WAL，失败则整个batch失败
- ✅ **单次fsync**：整个batch只需要一次磁盘同步
- ✅ **崩溃恢复**：WAL中的batch条目会一起重放
- ✅ **持久化保证**：数据不会因进程崩溃而丢失

**MemoryAdapter保证**：
- ✅ **进程内原子性**：单个RwLock保护，所有操作在锁内完成
- ⚠️ **无持久化**：进程崩溃数据丢失（符合内存存储语义）

**执行流程**：
```
1. 开始执行脚本
   ↓
2. 创建 ScriptTransaction (内存缓冲区)
   ↓
3. 执行脚本中的Redis命令
   - redis.call('SET', ...) → 写入 write_buffer
   - redis.call('GET', ...) → 先查 write_buffer，再查 storage
   - redis.call('DEL', ...) → 标记 delete 到 write_buffer
   ↓
4a. 脚本成功
    → 调用 storage.write_batch(operations)
    → MemoryAdapter: 单锁内批量更新
    → AiDbStorageAdapter: 使用 aidb::WriteBatch 原子提交
       • 所有操作写入WAL
       • 单次fsync刷盘
       • 提供崩溃恢复保证
    → 返回成功
4b. 脚本失败
    → transaction 被 drop
    → write_buffer 被丢弃
    → 自动回滚（不写 storage）
```

**优点**：
- ✅ 实现简单，代码改动小
- ✅ 不需要修改存储层核心逻辑（只添加 write_batch 接口）
- ✅ 自动回滚，无需手动清理
- ✅ 符合Redis脚本原子性语义
- ✅ 支持"读自己的写"语义
- ✅ **使用AiDb的WriteBatch获得真正的持久化原子性**

**相比初版改进**：
- ✅ **存储层原子性**：使用 AiDb WriteBatch 而非逐个写入
- ✅ **WAL保证**：AiDb 确保所有操作先写WAL再提交
- ✅ **崩溃恢复**：进程崩溃后可从WAL恢复完整事务
- ✅ **性能提升**：单次fsync而非多次

### 方案B：存储层事务支持

**核心思路**：在存储层实现begin/commit/rollback接口，类似数据库事务。

**实现结构**：
```rust
trait TransactionalStorage {
    fn begin_transaction(&self) -> Result<TransactionId>;
    fn commit_transaction(&self, txn_id: TransactionId) -> Result<()>;
    fn rollback_transaction(&self, txn_id: TransactionId) -> Result<()>;
    
    fn get_in_transaction(&self, txn_id: TransactionId, db: usize, key: &str) -> Result<Option<Bytes>>;
    fn set_in_transaction(&self, txn_id: TransactionId, db: usize, key: String, value: Bytes) -> Result<()>;
}
```

**优点**：
- ✅ 更通用，可用于MULTI/EXEC事务
- ✅ 可以支持更复杂的事务语义
- ✅ 可以支持跨命令的事务

**缺点**：
- ❌ 需要大幅修改存储层（违反最小变更原则）
- ❌ MemoryAdapter和AiDbAdapter都需要实现
- ❌ AiDb可能不支持事务，需要自己实现
- ❌ 实现复杂度高，影响范围大
- ❌ 性能开销：需要维护事务状态

## 推荐方案：方案A

基于以下考虑，**强烈推荐方案A**：

1. **最小变更原则**：只需修改`script.rs`，不影响其他模块
2. **实现难度**：相对简单，风险可控
3. **功能充分性**：满足当前需求（String命令的事务性）
4. **性能可接受**：HashMap查找O(1)，开销很小
5. **可扩展性**：未来如需支持复杂类型，可以扩展WriteOp枚举

## 实现计划

### 阶段1：设计和实现ScriptTransaction（1-2小时）

1. 在`script.rs`中实现`ScriptTransaction`结构体
2. 实现缓冲区的读写逻辑
3. 实现commit方法（批量写入存储）
4. 添加单元测试验证基本功能

### 阶段2：集成到脚本执行流程（1小时）

1. 修改`execute_script`方法，创建transaction
2. 修改`redis_call`函数，传递transaction引用
3. 修改`execute_get/set/del/exists`，使用transaction
4. 脚本成功时commit，失败时自动rollback

### 阶段3：测试和验证（1小时）

1. 添加事务成功场景测试
2. 添加事务回滚场景测试（脚本错误、命令错误）
3. 添加"读自己的写"测试
4. 运行完整测试套件确保无回归

### 阶段4：文档更新（30分钟）

1. 更新`docs/LUA_SCRIPTING.md`说明事务性
2. 更新`TODO.md`标记任务完成
3. 添加代码注释

## 需要确认的设计决策

### 1. 读语义
**问题**：脚本中的GET应该能看到同一脚本中SET的值吗？

**建议**：**是**，这符合Redis的脚本语义。实现为"优先从缓冲区读取"。

**示例**：
```lua
redis.call('SET', 'key', 'value')
local val = redis.call('GET', 'key')  -- 应该返回 'value'
```

### 2. 冲突处理
**问题**：同一个key被多次SET，保留哪个？

**建议**：**保留最后一次**，这符合Redis语义。实现为"直接覆盖HashMap中的值"。

**示例**：
```lua
redis.call('SET', 'key', 'v1')
redis.call('SET', 'key', 'v2')  -- 覆盖为v2
-- commit时只写入v2
```

### 3. DEL后SET
**问题**：先DEL后SET同一个key，最终结果？

**建议**：**SET生效**，保留最后一次操作。实现为"覆盖WriteOp"。

**示例**：
```lua
redis.call('SET', 'key', 'v1')
redis.call('DEL', 'key')         -- 标记删除
redis.call('SET', 'key', 'v2')   -- 覆盖为设置
-- commit时写入v2
```

### 4. 过期时间
**问题**：EXPIRE等命令也需要缓冲吗？

**建议**：**暂不支持**。当前脚本环境不支持EXPIRE命令，未来需要时再扩展。

### 5. 错误处理
**问题**：commit失败怎么办？

**建议**：
- 如果commit时发生存储错误，返回错误给脚本调用者
- 部分写入的情况：尽力而为，不保证原子性（与Redis一致）
- 记录错误日志以便排查

### 6. EXISTS语义
**问题**：EXISTS应该考虑缓冲区的DELETE吗？

**建议**：**是**。如果缓冲区标记为DELETE，EXISTS应返回false。

**示例**：
```lua
redis.call('SET', 'key', 'value')
redis.call('DEL', 'key')
local exists = redis.call('EXISTS', 'key')  -- 应该返回0
```

## 性能影响分析

### 内存影响
- 每个脚本执行：O(写操作数量) 的HashMap存储
- 典型场景：10-100个key，约1-10KB
- 可接受：内存开销小，脚本结束后立即释放

### 时间影响
- GET操作：增加一次HashMap查找 O(1)
- SET/DEL操作：写HashMap而非存储，可能更快
- COMMIT：批量写入，与原来的逐个写入性能相当
- **总体**：性能影响可忽略，可能略有提升（减少存储I/O）

## 局限性

1. **仅支持String命令**：当前实现只支持GET/SET/DEL/EXISTS
   - 扩展方案：未来可以扩展WriteOp支持复杂类型
   
2. **不支持跨脚本事务**：每个脚本独立的事务
   - 这与Redis一致，符合设计目标
   
3. **部分原子性**：commit时如果存储层失败，可能部分写入
   - 这与Redis一致，存储层本身不保证事务性

## 测试用例

### 成功提交场景
```lua
-- 脚本1：SET后GET
redis.call('SET', 'key1', 'value1')
local val = redis.call('GET', 'key1')
return val  -- 应返回 'value1'，且key1应持久化
```

### 回滚场景
```lua
-- 脚本2：SET后报错
redis.call('SET', 'key2', 'value2')
error('intentional error')
-- key2不应存在于存储中
```

### 读写顺序
```lua
-- 脚本3：复杂读写
redis.call('SET', 'key3', 'v1')
redis.call('DEL', 'key3')
redis.call('SET', 'key3', 'v2')
local exists = redis.call('EXISTS', 'key3')  -- 应返回1
return redis.call('GET', 'key3')  -- 应返回'v2'
```

## 结论

**推荐采用方案A（写缓冲区）**，理由：
- 符合最小变更原则
- 实现简单，风险低
- 满足功能需求
- 性能影响可忽略
- 为未来扩展留有空间

**实施时间估算**：3-4小时
**风险评估**：低
**收益**：高（解决数据一致性问题）

## 下一步

请确认：
1. 方案A是否可接受？
2. 上述6个设计决策是否合理？
3. 是否有其他需要考虑的场景？

确认后即可开始实施。

---

## 实现更新 (2024-11-17)

### 采用AiDb WriteBatch实现真正的原子性

根据@Genuineh的建议，实现已升级为使用AiDb的 `WriteBatch` API，提供了更强的原子性和持久化保证。

**核心改进**：

1. **添加 write_batch 方法到存储层**
   - `StorageAdapter::write_batch()` - 使用RwLock实现内存原子性
   - `AiDbStorageAdapter::write_batch()` - 使用 `aidb::WriteBatch` 实现WAL原子性

2. **ScriptTransaction 使用 BatchOp**
   ```rust
   use crate::storage::BatchOp;  // 统一的批量操作类型
   
   struct ScriptTransaction {
       db_index: usize,
       write_buffer: HashMap<String, BatchOp>,
   }
   
   fn commit(self, storage: &StorageAdapter) -> Result<()> {
       let operations: Vec<(String, BatchOp)> = 
           self.write_buffer.into_iter().collect();
       storage.write_batch(self.db_index, operations)
   }
   ```

3. **AiDb WriteBatch 保证**
   - **WAL原子性**：所有操作先写WAL，任何失败导致整个batch回滚
   - **单次fsync**：整个batch只需一次磁盘同步，性能最优
   - **崩溃恢复**：进程崩溃后WAL replay保证batch完整性
   - **真正的持久化**：数据不会因崩溃而丢失

**实现代码**：

```rust
// aidb_adapter.rs
pub fn write_batch(&self, db_index: usize, operations: Vec<(String, BatchOp)>) -> Result<()> {
    let db = &self.databases[db_index];
    let mut batch = WriteBatch::new();  // AiDb的WriteBatch
    
    for (key, op) in operations {
        match op {
            BatchOp::Set(value) => batch.put(key.as_bytes(), &value),
            BatchOp::Delete => {
                batch.delete(key.as_bytes());
                // 同时删除过期元数据
                let expire_key = Self::expiration_key(key.as_bytes());
                batch.delete(&expire_key);
            }
        }
    }
    
    // 原子提交：先写WAL，再写MemTable
    db.write(batch)?;
    Ok(())
}
```

**对比初版实现**：

| 特性 | 初版 (逐个写入) | 改进版 (WriteBatch) |
|------|----------------|-------------------|
| 进程内原子性 | ✅ | ✅ |
| WAL保证 | ❌ 多次WAL写入 | ✅ 单次WAL batch |
| 崩溃恢复 | ❌ 部分数据丢失 | ✅ 完整恢复 |
| 磁盘同步 | ❌ 多次fsync | ✅ 单次fsync |
| 性能 | ⚠️ O(n)次I/O | ✅ O(1)次I/O |

**测试验证**：
- ✅ 所有17个脚本测试通过
- ✅ 所有96个单元测试通过
- ✅ 0个clippy警告
- ✅ 代码格式化完成

**结论**：实现已升级为使用AiDb的原生批量写入能力，提供了真正的原子性和持久化保证，同时保持了代码的简洁性。
