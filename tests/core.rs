//! 核心协议、命令、存储、服务与配置集成测试统一入口
//! @component aikv-core
#![recursion_limit = "512"]

#[path = "modules/resp/mod.rs"]
mod resp;

#[path = "modules/command/mod.rs"]
mod command;

#[path = "modules/server/mod.rs"]
mod server;

#[path = "modules/storage/mod.rs"]
mod storage;

#[path = "modules/config/mod.rs"]
mod config;

#[path = "modules/storage/lazy_expire_mock.rs"]
mod lazy_expire_mock;
