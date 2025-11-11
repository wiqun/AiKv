# 贡献指南 (Contributing Guide)

感谢您考虑为 AiKv 项目做贡献！

## 行为准则

参与本项目即表示您同意遵守我们的行为准则。请对所有社区成员保持尊重和专业。

## 如何贡献

### 报告问题 (Issues)

如果您发现 bug 或有功能请求：

1. 先搜索现有的 issues，避免重复
2. 创建新 issue 时请提供：
   - 清晰的标题和描述
   - 复现步骤（如果是 bug）
   - 预期行为和实际行为
   - 环境信息（OS、Rust 版本等）
   - 相关日志或错误信息

### 提交代码

1. **Fork 仓库**
   ```bash
   git clone https://github.com/YOUR_USERNAME/AiKv.git
   cd AiKv
   ```

2. **创建分支**
   ```bash
   git checkout -b feature/your-feature-name
   # 或
   git checkout -b fix/your-bug-fix
   ```

3. **进行修改**
   - 遵循代码规范（见下文）
   - 编写或更新测试
   - 更新相关文档

4. **提交更改**
   ```bash
   git add .
   git commit -m "feat: add new feature"
   ```
   
   提交信息格式请遵循 [Conventional Commits](https://www.conventionalcommits.org/)：
   - `feat:` 新功能
   - `fix:` Bug 修复
   - `docs:` 文档更新
   - `style:` 代码格式（不影响功能）
   - `refactor:` 重构
   - `perf:` 性能优化
   - `test:` 测试相关
   - `chore:` 构建/工具相关

5. **推送到 GitHub**
   ```bash
   git push origin feature/your-feature-name
   ```

6. **创建 Pull Request**
   - 提供清晰的 PR 描述
   - 关联相关的 issue（使用 `Fixes #123`）
   - 等待 code review

## 代码规范

### Rust 代码风格

我们使用标准的 Rust 代码风格，通过以下工具强制执行：

#### 1. Rustfmt (代码格式化)

```bash
# 检查格式
cargo fmt --all -- --check

# 自动格式化
cargo fmt --all
```

配置文件：`rustfmt.toml`

#### 2. Clippy (代码检查)

```bash
# 运行 clippy
cargo clippy --all-targets --all-features -- -D warnings

# 自动修复
cargo clippy --fix --all-targets --all-features
```

配置文件：`clippy.toml`

### 代码规范要点

1. **命名规范**
   - 类型和 trait：`PascalCase`
   - 函数和变量：`snake_case`
   - 常量：`SCREAMING_SNAKE_CASE`
   - 模块：`snake_case`

2. **注释规范**
   - 公共 API 必须有文档注释（`///`）
   - 复杂逻辑添加行内注释（`//`）
   - 使用中文或英文均可，但同一文件保持一致

3. **函数规范**
   - 函数长度不超过 50 行（复杂函数除外）
   - 参数数量不超过 5 个
   - 返回 `Result<T, Error>` 而不是 panic

4. **错误处理**
   - 使用自定义错误类型
   - 避免 `unwrap()` 和 `expect()`，除非在测试或示例中
   - 提供有意义的错误信息

5. **测试规范**
   - 每个公共函数都应有测试
   - 测试函数命名：`test_function_name_scenario`
   - 使用 `#[test]` 标记单元测试
   - 使用 `tests/` 目录存放集成测试

### 代码示例

```rust
/// 获取键的值
///
/// # Arguments
///
/// * `key` - 要查询的键名
///
/// # Returns
///
/// 返回键对应的值，如果键不存在则返回 None
///
/// # Examples
///
/// ```
/// use aikv::StorageAdapter;
/// 
/// let storage = StorageAdapter::new();
/// let value = storage.get("mykey")?;
/// ```
pub fn get(&self, key: &str) -> Result<Option<Bytes>> {
    let data = self.data.read()
        .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;
    Ok(data.get(key).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_existing_key() {
        let storage = StorageAdapter::new();
        storage.set("key1".to_string(), Bytes::from("value1")).unwrap();
        
        let result = storage.get("key1").unwrap();
        assert_eq!(result, Some(Bytes::from("value1")));
    }

    #[test]
    fn test_get_nonexistent_key() {
        let storage = StorageAdapter::new();
        let result = storage.get("nonexistent").unwrap();
        assert_eq!(result, None);
    }
}
```

## 测试

### 运行测试

```bash
# 运行所有测试
cargo test

# 运行特定测试
cargo test test_name

# 运行并显示输出
cargo test -- --nocapture

# 运行集成测试
cargo test --test '*'
```

### 测试覆盖率

```bash
# 安装 tarpaulin
cargo install cargo-tarpaulin

# 生成覆盖率报告
cargo tarpaulin --out Html
```

### 性能测试

```bash
# 运行 benchmark
cargo bench
```

## 构建和运行

### 开发构建

```bash
# 调试构建
cargo build

# 运行
cargo run
```

### 发布构建

```bash
# 优化构建
cargo build --release

# 运行
./target/release/aikv
```

## 文档

### 生成文档

```bash
# 生成并打开文档
cargo doc --open

# 生成所有依赖的文档
cargo doc --no-deps
```

### 文档规范

- 所有公共 API 必须有文档
- 包含使用示例
- 说明参数和返回值
- 注明 panic 情况和错误情况

## Pull Request 检查清单

在提交 PR 之前，请确认：

- [ ] 代码通过 `cargo fmt` 格式化
- [ ] 代码通过 `cargo clippy` 检查
- [ ] 所有测试通过 (`cargo test`)
- [ ] 添加了新功能的测试
- [ ] 更新了相关文档
- [ ] 提交信息符合规范
- [ ] PR 描述清晰，关联了相关 issue
- [ ] 没有包含不相关的更改

## Code Review 流程

1. 至少一位维护者审查代码
2. 通过所有 CI 检查
3. 解决所有审查意见
4. 获得批准后合并

## 开发环境设置

### 必需工具

```bash
# 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装开发工具
rustup component add rustfmt clippy

# 安装其他工具
cargo install cargo-watch cargo-edit cargo-audit
```

### 推荐工具

- IDE: VSCode + rust-analyzer
- 调试: rust-gdb 或 rust-lldb
- 性能分析: flamegraph, valgrind

### 开发工作流

```bash
# 监视文件变化并自动测试
cargo watch -x test

# 监视并运行
cargo watch -x run
```

## 发布流程

（仅限维护者）

1. 更新版本号在 `Cargo.toml`
2. 更新 `CHANGELOG.md`
3. 创建 git tag: `git tag -a v0.x.0 -m "Release v0.x.0"`
4. 推送 tag: `git push origin v0.x.0`
5. GitHub Actions 自动构建和发布

## 获取帮助

如有疑问，可以通过以下方式获取帮助：

- 创建 issue 提问
- 查看现有文档：`docs/` 目录
- 参考 API 文档：`cargo doc --open`

## 许可证

提交代码即表示您同意您的贡献使用 MIT 许可证。

---

再次感谢您的贡献！🎉
