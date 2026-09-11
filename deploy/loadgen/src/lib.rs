//! AiKv 交互式加压工具 (loadgen).
//!
//! 对已部署的 AiKv 单机/集群持续加压; 不记录任何压测结果统计.

pub mod config;
pub mod ratelimit;
pub mod workload;
