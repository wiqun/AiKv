//! @component aikv-cluster
//! 集群多 Group 全局扫描 (SCAN / KEYS) 与流式分页集成测试.
#![cfg(feature = "cluster")]
#[path = "modules/cluster/scan.rs"]
mod tests;
