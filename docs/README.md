# AiKv 文档目录

欢迎查阅 AiKv 项目文档。本目录包含了项目的所有技术文档、架构设计和开发指南。

## 📚 文档导航

### 🏗️ 架构和设计

#### [ARCHITECTURE_REFACTORING.md](ARCHITECTURE_REFACTORING.md) ⭐ 新增
**存储层架构重构计划**
- 详细的重构方案和实施计划
- 问题分析和架构目标
- 新的 StorageBackend trait 设计
- 7 阶段迁移策略
- 风险管理和验收标准

推荐先阅读此文档了解即将进行的重大架构改进。

#### [ARCHITECTURE_COMPARISON.md](ARCHITECTURE_COMPARISON.md) ⭐ 新增
**架构对比和可视化**
- 当前架构 vs 目标架构的可视化对比
- 详细的迁移示例（MSET, LPUSH）
- 收益对比表格
- 图表化展示架构变化

配合 ARCHITECTURE_REFACTORING.md 阅读，理解重构的必要性和收益。

#### [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md)
**项目开发计划**
- 项目概述和技术栈
- 架构设计图
- 8 个开发阶段的详细任务分解
- 性能目标和兼容性说明
- 风险评估和时间规划

### 📖 使用指南

#### [API.md](API.md)
**API 参考文档**
- 所有支持命令的详细说明
- 命令语法、参数和返回值
- 完整的使用示例
- 多语言客户端示例（Rust, Python, Node.js, Go）
- 错误处理说明
- 性能建议和限制

#### [DEPLOYMENT.md](DEPLOYMENT.md)
**部署指南**
- 系统要求和安装步骤
- 详细的配置选项说明
- 多种部署方式：
  - 直接运行
  - Systemd 服务
  - Docker 容器
- 监控和维护建议
- 故障排查指南
- 安全建议和性能调优

### 🔧 技术实现

#### [AIDB_INTEGRATION.md](AIDB_INTEGRATION.md)
**AiDb 集成文档**
- AiDb 存储引擎集成说明
- 从内存适配器迁移到 AiDb
- 配置和使用指南
- 性能对比和优化建议

#### [PERSISTENCE.md](PERSISTENCE.md)
**数据持久化**
- RDB 快照持久化
- AOF 日志持久化
- 持久化策略和配置
- 数据恢复流程

#### [LUA_SCRIPTING.md](LUA_SCRIPTING.md)
**Lua 脚本支持**
- Lua 脚本执行引擎
- EVAL, EVALSHA 命令使用
- 脚本管理和缓存
- 安全性和限制

### 📊 性能和测试

#### [PERFORMANCE.md](PERFORMANCE.md)
**性能测试和优化**
- 性能基准测试结果
- 与 Redis 的对比
- 性能优化建议
- 瓶颈分析

#### [PERFORMANCE_TUNING.md](PERFORMANCE_TUNING.md) ⭐ 新增
**性能调优指南**
- 系统层面优化（操作系统、硬件）
- 应用层面优化（连接池、命令优化）
- 集群优化建议
- 常见性能问题诊断

#### [PRIORITY_7_COMPLETION.md](PRIORITY_7_COMPLETION.md)
**优先级 7 完成报告**
- 性能优化任务完成情况
- 测试结果和数据
- 下一步计划

### 🔧 运维指南 ⭐ 新增

#### [ARCHITECTURE.md](ARCHITECTURE.md) ⭐ 新增
**架构设计文档**
- 系统整体架构
- 核心模块设计
- 数据流和设计模式
- 扩展性说明

#### [TROUBLESHOOTING.md](TROUBLESHOOTING.md) ⭐ 新增
**故障排查指南**
- 快速诊断流程
- 启动、连接、命令问题排查
- 性能和数据问题诊断
- 集群问题处理

#### [BEST_PRACTICES.md](BEST_PRACTICES.md) ⭐ 新增
**最佳实践**
- 键命名规范
- 数据类型选择
- 批量操作和管道
- 安全最佳实践
- 代码示例（Python、Go）

### 📝 项目总结

#### [SUMMARY.md](SUMMARY.md)
**项目完成情况总结**
- 已完成功能清单
- 代码统计和测试覆盖率
- 技术亮点
- 后续优化建议

## 🎯 快速导航

### 我是新手，想了解项目
1. 先阅读 [../README.md](../README.md) - 项目主页
2. 再看 [SUMMARY.md](SUMMARY.md) - 了解项目完成情况
3. 然后看 [API.md](API.md) - 学习如何使用

### 我想部署 AiKv
1. [DEPLOYMENT.md](DEPLOYMENT.md) - 完整部署指南
2. [PERFORMANCE.md](PERFORMANCE.md) - 了解性能特征
3. [../README.md](../README.md) - 快速开始

### 我想参与开发
1. [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md) - 了解项目规划
2. [ARCHITECTURE_REFACTORING.md](ARCHITECTURE_REFACTORING.md) ⭐ - 当前重点任务
3. [ARCHITECTURE_COMPARISON.md](ARCHITECTURE_COMPARISON.md) ⭐ - 理解架构演进
4. [../TODO.md](../TODO.md) - 查看待办事项
5. [../CONTRIBUTING.md](../CONTRIBUTING.md) - 贡献指南

### 我想了解架构重构计划 ⭐
**推荐阅读顺序：**
1. [ARCHITECTURE_COMPARISON.md](ARCHITECTURE_COMPARISON.md) - 先看可视化对比，快速理解问题
2. [ARCHITECTURE_REFACTORING.md](ARCHITECTURE_REFACTORING.md) - 深入了解详细计划
3. [../TODO.md](../TODO.md) - 查看"优先级 0"部分的任务清单

### 我想了解特定技术细节
- **存储引擎**: [AIDB_INTEGRATION.md](AIDB_INTEGRATION.md)
- **持久化**: [PERSISTENCE.md](PERSISTENCE.md)
- **脚本支持**: [LUA_SCRIPTING.md](LUA_SCRIPTING.md)
- **性能优化**: [PERFORMANCE.md](PERFORMANCE.md)
- **API 使用**: [API.md](API.md)

## 📌 重要提示

### ⭐ 最新架构重构计划（2025-11-13）

项目即将进行重大架构重构，目标是将命令逻辑从存储层分离出来。这将：
- 减少存储层方法数量 70%（从 52+ 到 ~15）
- 提高代码可维护性和可测试性
- 使切换存储引擎更容易
- 使架构更符合设计原则

详见：
- [ARCHITECTURE_REFACTORING.md](ARCHITECTURE_REFACTORING.md)
- [ARCHITECTURE_COMPARISON.md](ARCHITECTURE_COMPARISON.md)
- [../TODO.md](../TODO.md) - 优先级 0 部分

## 🔗 相关链接

- **主仓库**: [GitHub - Genuineh/AiKv](https://github.com/Genuineh/AiKv)
- **AiDb 仓库**: [GitHub - Genuineh/AiDb](https://github.com/Genuineh/AiDb)
- **问题追踪**: [GitHub Issues](https://github.com/Genuineh/AiKv/issues)
- **待办事项**: [../TODO.md](../TODO.md)

## 📅 文档更新

- **最后更新**: 2025-12-02
- **维护者**: @Genuineh, @copilot

## 🤝 贡献文档

如果您发现文档有误或需要改进，欢迎：
1. 提交 Issue 报告问题
2. 提交 Pull Request 改进文档
3. 在讨论区分享建议

详见 [../CONTRIBUTING.md](../CONTRIBUTING.md)

---

**提示**: 本文档目录会随着项目发展持续更新，建议定期查看最新版本。
