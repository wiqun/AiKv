//! 负载计划: 命令抽样 / key 生成 / pipeline 组装.
//!
//! 设计: `plan_batch` 是纯函数 (只依赖配置与 RNG), 便于单测;
//! `build_pipeline` 是 `PlannedOp -> redis::Pipeline` 的无随机映射.

use rand::Rng;

use crate::config::{Mix, WorkloadConfig};

/// 单轮 SET 的 TTL 秒数.
pub const TTL_SECONDS: u64 = 60;

/// 支持的命令集合.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    Set,
    Get,
    Del,
    Mget,
    Incr,
    Expire,
}

impl Op {
    pub const ALL: [Op; 6] = [Op::Set, Op::Get, Op::Del, Op::Mget, Op::Incr, Op::Expire];

    pub fn weight(self, mix: &Mix) -> u32 {
        match self {
            Op::Set => mix.set,
            Op::Get => mix.get,
            Op::Del => mix.del,
            Op::Mget => mix.mget,
            Op::Incr => mix.incr,
            Op::Expire => mix.expire,
        }
    }

    /// 只读模式: 写命令降级为读, 保证压力连续 (而不是拒绝执行).
    pub fn read_only_fallback(self) -> Op {
        match self {
            Op::Set | Op::Del | Op::Incr | Op::Expire => Op::Get,
            Op::Get | Op::Mget => self,
        }
    }
}

/// 一条已计划的命令 (纯数据, 可测).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedOp {
    pub op: Op,
    pub key: String,
    /// MGET 的第二个 key
    pub second_key: Option<String>,
    /// SET 的 value 长度
    pub value_size: usize,
    /// SET 的 TTL 秒数; EXPIRE 固定 60
    pub ttl_seconds: Option<u64>,
}

/// 按累计权重抽样; roll 需落在 `0..mix.total()`.
pub fn pick_op(mix: &Mix, roll: u32) -> Op {
    let mut acc = 0;
    for op in Op::ALL {
        acc += op.weight(mix);
        if roll < acc {
            return op;
        }
    }
    Op::Get
}

/// 生成 key; `use_hashtag` 时包 `{}` 钉在同一 slot (定向单节点).
pub fn make_key(cfg: &WorkloadConfig, index: u64, miss: bool) -> String {
    let segment = if miss { "miss" } else { "key" };
    if cfg.use_hashtag {
        format!("{{{}}}:{segment}:{index}", cfg.key_prefix)
    } else {
        format!("{}:{segment}:{index}", cfg.key_prefix)
    }
}

/// 在 [min, max] 内取 value 尺寸.
pub fn pick_value_size(min: u32, max: u32, rng: &mut impl Rng) -> usize {
    if min == max {
        min as usize
    } else {
        rng.gen_range(min..=max) as usize
    }
}

/// 计划一轮 pipeline (长度 = cfg.pipeline).
pub fn plan_batch(cfg: &WorkloadConfig, rng: &mut impl Rng) -> Vec<PlannedOp> {
    let total = cfg.mix.total();
    let mut plan = Vec::with_capacity(cfg.pipeline as usize);
    for _ in 0..cfg.pipeline {
        let mut op = pick_op(&cfg.mix, rng.gen_range(0..total));
        if cfg.readonly {
            op = op.read_only_fallback();
        }
        let key = make_key(
            cfg,
            rng.gen_range(0..cfg.keyspace),
            rng.gen_bool(cfg.miss_ratio),
        );
        let (second_key, value_size, ttl_seconds) = match op {
            Op::Set => (
                None,
                pick_value_size(cfg.value_size_min, cfg.value_size_max, rng),
                if rng.gen_bool(cfg.ttl_ratio) {
                    Some(TTL_SECONDS)
                } else {
                    None
                },
            ),
            Op::Mget => (
                Some(make_key(
                    cfg,
                    rng.gen_range(0..cfg.keyspace),
                    rng.gen_bool(cfg.miss_ratio),
                )),
                0,
                None,
            ),
            Op::Expire => (None, 0, Some(TTL_SECONDS)),
            Op::Get | Op::Del | Op::Incr => (None, 0, None),
        };
        plan.push(PlannedOp {
            op,
            key,
            second_key,
            value_size,
            ttl_seconds,
        });
    }
    plan
}

/// 把计划映射为 redis pipeline; 所有命令 ignore, 无结果解析开销.
pub fn build_pipeline(plan: &[PlannedOp], value: &[u8]) -> redis::Pipeline {
    let mut pipe = redis::pipe();
    for planned in plan {
        let cmd = match planned.op {
            Op::Set => {
                let size = planned.value_size.min(value.len());
                let mut cmd = redis::cmd("SET");
                cmd.arg(&planned.key).arg(&value[..size]);
                if let Some(ttl) = planned.ttl_seconds {
                    cmd.arg("EX").arg(ttl);
                }
                cmd
            }
            Op::Get => {
                let mut cmd = redis::cmd("GET");
                cmd.arg(&planned.key);
                cmd
            }
            Op::Del => {
                let mut cmd = redis::cmd("DEL");
                cmd.arg(&planned.key);
                cmd
            }
            Op::Incr => {
                let mut cmd = redis::cmd("INCR");
                cmd.arg(&planned.key);
                cmd
            }
            Op::Expire => {
                let mut cmd = redis::cmd("EXPIRE");
                cmd.arg(&planned.key)
                    .arg(planned.ttl_seconds.unwrap_or(TTL_SECONDS));
                cmd
            }
            Op::Mget => {
                let mut cmd = redis::cmd("MGET");
                cmd.arg(&planned.key);
                if let Some(second) = &planned.second_key {
                    cmd.arg(second);
                }
                cmd
            }
        };
        pipe.add_command(cmd);
        pipe.ignore();
    }
    pipe
}

#[cfg(test)]
mod tests;
