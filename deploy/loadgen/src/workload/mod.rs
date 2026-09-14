//! 负载计划: 命令抽样 / key 生成 / pipeline 组装.
//!
//! 设计: `plan_batch` 是纯函数 (只依赖配置与 RNG), 便于单测;
//! `build_pipeline` 是 `PlannedOp -> redis::Pipeline` 的无随机映射.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use rand::Rng;

use crate::config::{Mix, TargetMode, WorkloadConfig};

/// 单轮 SET 的默认 TTL 秒数.
pub const TTL_SECONDS: u64 = 60;

// CRC16-CCITT 查找表 (多项式 0x1021)
const CRC16_TABLE: [u16; 256] = [
    0x0000, 0x1021, 0x2042, 0x3063, 0x4084, 0x50a5, 0x60c6, 0x70e7, 0x8108, 0x9129, 0xa14a, 0xb16b,
    0xc18c, 0xd1ad, 0xe1ce, 0xf1ef, 0x1231, 0x0210, 0x3273, 0x2252, 0x52b5, 0x4294, 0x72f7, 0x62d6,
    0x9339, 0x8318, 0xb37b, 0xa35a, 0xd3bd, 0xc39c, 0xf3ff, 0xe3de, 0x2462, 0x3443, 0x0420, 0x1401,
    0x64e6, 0x74c7, 0x44a4, 0x5485, 0xa56a, 0xb54b, 0x8528, 0x9509, 0xe5ee, 0xf5cf, 0xc5ac, 0xd58d,
    0x3653, 0x2672, 0x1611, 0x0630, 0x76d7, 0x66f6, 0x5695, 0x46b4, 0xb75b, 0xa77a, 0x9719, 0x8738,
    0xf7df, 0xe7fe, 0xd79d, 0xc7bc, 0x48c4, 0x58e5, 0x6886, 0x78a7, 0x0840, 0x1861, 0x2802, 0x3823,
    0xc9cc, 0xd9ed, 0xe98e, 0xf9af, 0x8948, 0x9969, 0xa90a, 0xb92b, 0x5af5, 0x4ad4, 0x7ab7, 0x6a96,
    0x1a71, 0x0a50, 0x3a33, 0x2a12, 0xdbfd, 0xcbdc, 0xfbbf, 0xeb9e, 0x9b79, 0x8b58, 0xbb3b, 0xab1a,
    0x6ca6, 0x7c87, 0x4ce4, 0x5cc5, 0x2c22, 0x3c03, 0x0c60, 0x1c41, 0xedae, 0xfd8f, 0xcdec, 0xddcd,
    0xad2a, 0xbd0b, 0x8d68, 0x9d49, 0x7e97, 0x6eb6, 0x5ed5, 0x4ef4, 0x3e13, 0x2e32, 0x1e51, 0x0e70,
    0xff9f, 0xefbe, 0xdfdd, 0xcffc, 0xbf1b, 0xaf3a, 0x9f59, 0x8f78, 0x9188, 0x81a9, 0xb1ca, 0xa1eb,
    0xd10c, 0xc12d, 0xf14e, 0xe16f, 0x1080, 0x00a1, 0x30c2, 0x20e3, 0x5004, 0x4025, 0x7046, 0x6067,
    0x83b9, 0x9398, 0xa3fb, 0xb3da, 0xc33d, 0xd31c, 0xe37f, 0xf35e, 0x02b1, 0x1290, 0x22f3, 0x32d2,
    0x4235, 0x5214, 0x6277, 0x7256, 0xb5ea, 0xa5cb, 0x95a8, 0x8589, 0xf56e, 0xe54f, 0xd52c, 0xc50d,
    0x34e2, 0x24c3, 0x14a0, 0x0481, 0x7466, 0x6447, 0x5424, 0x4405, 0xa7db, 0xb7fa, 0x8799, 0x97b8,
    0xe75f, 0xf77e, 0xc71d, 0xd73c, 0x26d3, 0x36f2, 0x0691, 0x16b0, 0x6657, 0x7676, 0x4615, 0x5634,
    0xd94c, 0xc96d, 0xf90e, 0xe92f, 0x99c8, 0x89e9, 0xb98a, 0xa9ab, 0x5844, 0x4865, 0x7806, 0x6827,
    0x18c0, 0x08e1, 0x3882, 0x28a3, 0xcb7d, 0xdb5c, 0xeb3f, 0xfb1e, 0x8bf9, 0x9bd8, 0xabbb, 0xbb9a,
    0x4a75, 0x5a54, 0x6a37, 0x7a16, 0x0af1, 0x1ad0, 0x2ab3, 0x3a92, 0xfd2e, 0xed0f, 0xdd6c, 0xcd4d,
    0xbdaa, 0xad8b, 0x9de8, 0x8dc9, 0x7c26, 0x6c07, 0x5c64, 0x4c45, 0x3ca2, 0x2c83, 0x1ce0, 0x0cc1,
    0xef1f, 0xff3e, 0xcf5d, 0xdf7c, 0xaf9b, 0xbfba, 0x8fd9, 0x9ff8, 0x6e17, 0x7e36, 0x4e55, 0x5e74,
    0x2e93, 0x3eb2, 0x0ed1, 0x1ef0,
];

/// 计算数据切片的 CRC16-CCITT 校验和.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        let idx = ((crc >> 8) ^ u16::from(byte)) as usize;
        crc = (crc << 8) ^ CRC16_TABLE[idx & 0xff];
    }
    crc
}

/// 计算 key 对应的 slot (0..16384). 若存在 hash tag 则以 tag 计算.
pub fn key_slot(key: &str) -> u16 {
    let tag = hash_tag(key).unwrap_or(key);
    crc16(tag.as_bytes()) % 16_384
}

static SLOT_TAGS: OnceLock<Box<[String; 16_384]>> = OnceLock::new();

/// 获取可命中指定 slot (0..16384) 的 hashtag 字符串.
pub fn tag_for_slot(slot: u16) -> &'static str {
    let tags = SLOT_TAGS.get_or_init(|| {
        let mut map = vec![String::new(); 16_384];
        let mut filled = 0;
        let mut i = 0u32;
        while filled < 16_384 && i < 200_000 {
            let s = i.to_string();
            let s_slot = (crc16(s.as_bytes()) % 16_384) as usize;
            if map[s_slot].is_empty() {
                map[s_slot] = s;
                filled += 1;
            }
            i += 1;
        }
        map.into_boxed_slice().try_into().expect("16384 slots")
    });
    &tags[(slot % 16_384) as usize]
}

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
    /// SET 的全局写入序列号 (用于生成轮次与版本标识)
    pub set_seq: Option<u64>,
    /// SET 的 TTL 秒数; EXPIRE 固定 60
    pub ttl_seconds: Option<u64>,
}

/// 语义空间段: 意图分层隔离, 杜绝命中率腐蚀与 WRONGTYPE 报错.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySegment {
    /// 主数据段: 长期有效, 纯写入与命中读, 不设 TTL, 绝不 DEL
    Main,
    /// 穿透段: 永远不写, 专供未命中读 (100% 穿透为 nil)
    Miss,
    /// 挥发段: 带 TTL 的 SET 与 EXPIRE 专属空间
    Ttl,
    /// 扰动段: DEL 命令专属空间, 压测删除吞吐与内存回收
    Churn,
    /// 整型段: INCR/DECR 专属空间, 避免类型冲突
    Int,
}

impl KeySegment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "key",
            Self::Miss => "miss",
            Self::Ttl => "ttl",
            Self::Churn => "churn",
            Self::Int => "int",
        }
    }
}

/// 多原子水位线: 分别追踪各意图段的写入水位.
#[derive(Debug, Default)]
pub struct Watermarks {
    /// 主数据段已写入水位
    pub main: AtomicU64,
    /// 挥发段 (带 TTL) 已写入水位
    pub ttl: AtomicU64,
    /// 扰动段 (DEL) 活跃水位
    pub churn: AtomicU64,
}

impl Watermarks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&self) {
        self.main.store(0, Ordering::Relaxed);
        self.ttl.store(0, Ordering::Relaxed);
        self.churn.store(0, Ordering::Relaxed);
    }
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

/// 提取 Redis hash tag (`{tag}`); 无 tag 时返回 None.
pub fn hash_tag(key: &str) -> Option<&str> {
    let start = key.find('{')?;
    let inner = &key[start + 1..];
    let end = inner.find('}')?;
    if end == 0 {
        None
    } else {
        Some(&inner[..end])
    }
}

fn slot_tag_for_batch(cfg: &WorkloadConfig, rng: &mut impl Rng) -> Option<String> {
    if cfg.mode == TargetMode::Cluster && !cfg.use_hashtag {
        Some(rng.gen_range(0u16..16_384).to_string())
    } else {
        None
    }
}

/// 生成对应语义段的 key.
pub fn make_key(
    cfg: &WorkloadConfig,
    index: u64,
    segment: KeySegment,
    slot_tag: Option<&str>,
) -> String {
    let seg = segment.as_str();
    if cfg.use_hashtag {
        if let Some(target) = cfg.target_slot {
            let tag = tag_for_slot(target);
            format!("{{{tag}}}:{}:{seg}:{index}", cfg.key_prefix)
        } else {
            format!("{{{}}}:{seg}:{index}", cfg.key_prefix)
        }
    } else if let Some(tag) = slot_tag {
        format!("{{{tag}}}:{}:{seg}:{index}", cfg.key_prefix)
    } else {
        format!("{}:{seg}:{index}", cfg.key_prefix)
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

fn pick_existing_key(
    cfg: &WorkloadConfig,
    watermark: &AtomicU64,
    segment: KeySegment,
    tag: Option<&str>,
    rng: &mut impl Rng,
) -> String {
    let w = watermark.load(Ordering::Relaxed);
    let pool = if cfg.keyspace == 0 {
        0
    } else {
        w.min(cfg.keyspace)
    };
    let index = if pool == 0 { 0 } else { rng.gen_range(0..pool) };
    make_key(cfg, index, segment, tag)
}

fn pick_read_key(
    cfg: &WorkloadConfig,
    watermarks: &Watermarks,
    tag: Option<&str>,
    rng: &mut impl Rng,
) -> String {
    let hit_ratio = (1.0 - cfg.miss_ratio).clamp(0.0, 1.0);
    let is_hit = if hit_ratio <= 0.0 {
        false
    } else if hit_ratio >= 1.0 {
        true
    } else {
        rng.gen_bool(hit_ratio)
    };

    if is_hit {
        pick_existing_key(cfg, &watermarks.main, KeySegment::Main, tag, rng)
    } else {
        let index = if cfg.keyspace == 0 {
            0
        } else {
            rng.gen_range(0..cfg.keyspace)
        };
        make_key(cfg, index, KeySegment::Miss, tag)
    }
}

fn plan_single_op(
    cfg: &WorkloadConfig,
    watermarks: &Watermarks,
    op: Op,
    tag: Option<&str>,
    rng: &mut impl Rng,
) -> PlannedOp {
    let (key, second_key, value_size, set_seq, ttl_seconds) = match op {
        Op::Set => {
            let is_ttl = rng.gen_bool(cfg.ttl_ratio);
            if is_ttl {
                let seq = watermarks.ttl.fetch_add(1, Ordering::Relaxed);
                let index = seq.checked_rem(cfg.keyspace).unwrap_or(0);
                let key = make_key(cfg, index, KeySegment::Ttl, tag);
                let value_size = pick_value_size(cfg.value_size_min, cfg.value_size_max, rng);
                (key, None, value_size, Some(seq), Some(cfg.ttl_seconds))
            } else {
                let seq = watermarks.main.fetch_add(1, Ordering::Relaxed);
                let index = seq.checked_rem(cfg.keyspace).unwrap_or(0);
                let key = make_key(cfg, index, KeySegment::Main, tag);
                let value_size = pick_value_size(cfg.value_size_min, cfg.value_size_max, rng);
                (key, None, value_size, Some(seq), None)
            }
        }
        Op::Get => (
            pick_read_key(cfg, watermarks, tag, rng),
            None,
            0,
            None,
            None,
        ),
        Op::Mget => {
            let k1 = pick_read_key(cfg, watermarks, tag, rng);
            let k2 = pick_read_key(cfg, watermarks, tag, rng);
            (k1, Some(k2), 0, None, None)
        }
        Op::Del => {
            let seq = watermarks.churn.fetch_add(1, Ordering::Relaxed);
            let index = seq.checked_rem(cfg.keyspace).unwrap_or(0);
            let key = make_key(cfg, index, KeySegment::Churn, tag);
            (key, None, 0, None, None)
        }
        Op::Expire => {
            let key = pick_existing_key(cfg, &watermarks.ttl, KeySegment::Ttl, tag, rng);
            (key, None, 0, None, Some(cfg.ttl_seconds))
        }
        Op::Incr => {
            let index = if cfg.keyspace == 0 {
                0
            } else {
                rng.gen_range(0..cfg.keyspace)
            };
            (
                make_key(cfg, index, KeySegment::Int, tag),
                None,
                0,
                None,
                None,
            )
        }
    };
    PlannedOp {
        op,
        key,
        second_key,
        value_size,
        set_seq,
        ttl_seconds,
    }
}

/// 计划一轮 pipeline (长度 = cfg.pipeline).
pub fn plan_batch(
    cfg: &WorkloadConfig,
    watermarks: &Watermarks,
    rng: &mut impl Rng,
) -> Vec<PlannedOp> {
    let total = cfg.mix.total();
    let slot_tag = slot_tag_for_batch(cfg, rng);
    let tag = slot_tag.as_deref();
    let mut plan = Vec::with_capacity(cfg.pipeline as usize);
    for _ in 0..cfg.pipeline {
        let mut op = pick_op(&cfg.mix, rng.gen_range(0..total));
        if cfg.readonly {
            op = op.read_only_fallback();
        }
        plan.push(plan_single_op(cfg, watermarks, op, tag, rng));
    }
    plan
}

/// 生成带有轮次与序列号标识的 value 载荷: `v{round}:{seq}:xxxxx`
pub fn build_value_payload(seq: u64, keyspace: u64, target_size: usize, pad: &[u8]) -> Vec<u8> {
    let round = seq.checked_div(keyspace).unwrap_or(0);
    let prefix = format!("v{round}:{seq}:");
    let prefix_bytes = prefix.as_bytes();
    let mut buf = Vec::with_capacity(target_size);
    if target_size <= prefix_bytes.len() {
        buf.extend_from_slice(&prefix_bytes[..target_size]);
    } else {
        buf.extend_from_slice(prefix_bytes);
        let remain = target_size - prefix_bytes.len();
        if pad.len() >= remain {
            buf.extend_from_slice(&pad[..remain]);
        } else {
            buf.extend_from_slice(pad);
            buf.resize(target_size, b'x');
        }
    }
    buf
}

fn build_op_cmd(planned: &PlannedOp, keyspace: u64, value: &[u8]) -> redis::Cmd {
    match planned.op {
        Op::Set => {
            let mut cmd = redis::cmd("SET");
            cmd.arg(&planned.key);
            if let Some(seq) = planned.set_seq {
                let payload = build_value_payload(seq, keyspace, planned.value_size, value);
                cmd.arg(payload);
            } else {
                let size = planned.value_size.min(value.len());
                cmd.arg(&value[..size]);
            }
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
    }
}

/// 把计划映射为 redis pipeline; 所有命令 ignore, 无结果解析开销.
pub fn build_pipeline(plan: &[PlannedOp], keyspace: u64, value: &[u8]) -> redis::Pipeline {
    let mut pipe = redis::pipe();
    for planned in plan {
        pipe.add_command(build_op_cmd(planned, keyspace, value));
        pipe.ignore();
    }
    pipe
}

#[cfg(test)]
mod tests;
