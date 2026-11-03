//! @component aikv-cluster
//! 回归测试: ATOM.EXEC/EXEC JSON batch (事务提交协议) 在集群拓扑
//! 下必须把内部快照阶段命中的 MOVED/ASK 原样透传给客户端, 而不能包裹成
//! 一个通用的 "ERR internal error during batch snapshot: ..." 错误.
//!
//! 背景: `cmd_atom_exec_json_batch` 在真正执行每条写命令前, 先用内部
//! `routed_command("DUMP", key)` 给要写的 key 做快照 (用于失败回滚).
//! 8bfac2f (移除进程内透明转发 forward_command, 让客户端自行按 -c 重定向)
//! 之后, 若该 key 的 slot 不在本节点, 这次内部 DUMP 会先拿到一个
//! `RespValue::Error("MOVED ...")`, `batch_load_key_snapshot` 把它转成
//! `Error::Command("MOVED ...")` 向上冒泡, 最终被 `cmd_atom_exec_json_batch`
//! 用 `format!("ERR internal error during batch snapshot: {e}")` 包了一层,
//! 产出类似 `ERR internal error during batch snapshot: 命令错误: MOVED 3821
//! 192.168.1.112:6379` 的顶层响应.这不是标准 `-MOVED slot addr` 格式,
//! 客户端无法识别并自动重定向重试整个
//! batch, 导致客户端事务提交直接抛出
//! 异常.
#![cfg(feature = "cluster")]
#![recursion_limit = "512"]
#[path = "modules/cluster/atom_exec_json_batch_redirect.rs"]
mod tests;
