//! 按类型命名空间 + 意图段生成 PlannedOp, 并映射为 redis::Cmd.

use std::sync::atomic::{AtomicU64, Ordering};

use rand::Rng;

use crate::catalog::{
    lookup, ArgTmpl, CommandSpec, OpKind, ValueType, CATALOG, EVAL_SCRIPT, FIELD, JSON_DOC, MEMBER,
    NUM_FIELD,
};
use crate::config::{Mix, TargetMode, WorkloadConfig};

use super::{tag_for_slot, TTL_SECONDS};

/// 一条已调度命令.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Op(pub &'static CommandSpec);

impl Op {
    pub fn all_core() -> [Op; 6] {
        [
            Op::set(),
            Op::get(),
            Op::del(),
            Op::mget(),
            Op::incr(),
            Op::expire(),
        ]
    }

    pub fn named(mix_key: &str) -> Self {
        Self(lookup(mix_key).unwrap_or_else(|| panic!("unknown mix key {mix_key}")))
    }

    pub fn set() -> Self {
        Self::named("set")
    }
    pub fn get() -> Self {
        Self::named("get")
    }
    pub fn del() -> Self {
        Self::named("del")
    }
    pub fn mget() -> Self {
        Self::named("mget")
    }
    pub fn incr() -> Self {
        Self::named("incr")
    }
    pub fn expire() -> Self {
        Self::named("expire")
    }

    pub fn mix_key(self) -> &'static str {
        self.0.mix_key
    }

    pub fn spec(self) -> &'static CommandSpec {
        self.0
    }

    pub fn weight(self, mix: &Mix) -> u32 {
        mix.weight(self.0.mix_key)
    }
}

/// 一条已计划的命令 (纯数据, 可测).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedOp {
    pub op: Op,
    pub key: String,
    pub second_key: Option<String>,
    pub third_key: Option<String>,
    pub value_size: usize,
    pub set_seq: Option<u64>,
    pub ttl_seconds: Option<u64>,
}

/// 语义空间段: 意图分层隔离.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySegment {
    Main,
    Miss,
    Ttl,
    Churn,
}

impl KeySegment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "key",
            Self::Miss => "miss",
            Self::Ttl => "ttl",
            Self::Churn => "churn",
        }
    }
}

#[derive(Debug, Default)]
pub struct TypeWm {
    pub main: AtomicU64,
    pub ttl: AtomicU64,
    pub churn: AtomicU64,
}

impl TypeWm {
    fn reset(&self) {
        self.main.store(0, Ordering::Relaxed);
        self.ttl.store(0, Ordering::Relaxed);
        self.churn.store(0, Ordering::Relaxed);
    }
}

/// 各数据类型独立水位线.
#[derive(Debug, Default)]
pub struct Watermarks {
    pub string: TypeWm,
    pub hash: TypeWm,
    pub list: TypeWm,
    pub set: TypeWm,
    pub zset: TypeWm,
    pub json: TypeWm,
    pub int: TypeWm,
}

impl Watermarks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn of(&self, ty: ValueType) -> &TypeWm {
        match ty {
            ValueType::String | ValueType::None => &self.string,
            ValueType::Hash => &self.hash,
            ValueType::List => &self.list,
            ValueType::Set => &self.set,
            ValueType::ZSet => &self.zset,
            ValueType::Json => &self.json,
            ValueType::Int => &self.int,
        }
    }

    pub fn reset(&self) {
        self.string.reset();
        self.hash.reset();
        self.list.reset();
        self.set.reset();
        self.zset.reset();
        self.json.reset();
        self.int.reset();
    }
}

pub fn pick_op(mix: &Mix, roll: u32) -> Op {
    let mut acc = 0u32;
    for spec in CATALOG {
        let w = mix.weight(spec.mix_key);
        if w == 0 {
            continue;
        }
        acc += w;
        if roll < acc {
            return Op(spec);
        }
    }
    Op::get()
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

pub fn make_key(
    cfg: &WorkloadConfig,
    index: u64,
    ty: ValueType,
    segment: KeySegment,
    slot_tag: Option<&str>,
) -> String {
    let seg = segment.as_str();
    let type_name = ty.as_str();
    if cfg.use_hashtag {
        if let Some(target) = cfg.target_slot {
            let tag = tag_for_slot(target);
            format!("{{{tag}}}:{}:{type_name}:{seg}:{index}", cfg.key_prefix)
        } else {
            format!("{{{}}}:{type_name}:{seg}:{index}", cfg.key_prefix)
        }
    } else if let Some(tag) = slot_tag {
        format!("{{{tag}}}:{}:{type_name}:{seg}:{index}", cfg.key_prefix)
    } else {
        format!("{}:{type_name}:{seg}:{index}", cfg.key_prefix)
    }
}

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
    ty: ValueType,
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
    make_key(cfg, index, ty, segment, tag)
}

fn pick_read_key(
    cfg: &WorkloadConfig,
    watermarks: &Watermarks,
    ty: ValueType,
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
    let marks = watermarks.of(ty);
    if is_hit {
        pick_existing_key(cfg, &marks.main, ty, KeySegment::Main, tag, rng)
    } else {
        let index = if cfg.keyspace == 0 {
            0
        } else {
            rng.gen_range(0..cfg.keyspace)
        };
        make_key(cfg, index, ty, KeySegment::Miss, tag)
    }
}

fn next_index(cfg: &WorkloadConfig, seq: u64) -> u64 {
    seq.checked_rem(cfg.keyspace).unwrap_or(0)
}

fn plan_single_op(
    cfg: &WorkloadConfig,
    watermarks: &Watermarks,
    op: Op,
    tag: Option<&str>,
    rng: &mut impl Rng,
) -> PlannedOp {
    let spec = op.spec();
    let ty = spec.value_type;
    let marks = watermarks.of(ty);
    let mut ttl_seconds = None;
    let mut value_size = 0usize;
    let mut set_seq = None;
    let mut second_key = None;
    let mut third_key = None;

    let key = match spec.kind {
        OpKind::NoKey => {
            if let Some(tag) = tag {
                format!("{{{tag}}}:{}", cfg.key_prefix)
            } else if cfg.use_hashtag {
                if let Some(target) = cfg.target_slot {
                    format!("{{{}}}:{}", tag_for_slot(target), cfg.key_prefix)
                } else {
                    format!("{{{}}}", cfg.key_prefix)
                }
            } else {
                cfg.key_prefix.clone()
            }
        }
        OpKind::Int => {
            let index = if cfg.keyspace == 0 {
                0
            } else {
                rng.gen_range(0..cfg.keyspace)
            };
            make_key(cfg, index, ValueType::Int, KeySegment::Main, tag)
        }
        OpKind::Churn => {
            let seq = marks.churn.fetch_add(1, Ordering::Relaxed);
            let index = next_index(cfg, seq);
            let key = make_key(cfg, index, ty, KeySegment::Churn, tag);
            if matches!(
                spec.tmpl,
                ArgTmpl::TwoKey
                    | ArgTmpl::Lmove
                    | ArgTmpl::Union
                    | ArgTmpl::UnionStore
                    | ArgTmpl::Zinter
                    | ArgTmpl::JsonMset
                    | ArgTmpl::JsonMget
                    | ArgTmpl::Mset
                    | ArgTmpl::Mget
            ) {
                second_key = Some(make_key(
                    cfg,
                    next_index(cfg, seq + 1),
                    ty,
                    KeySegment::Churn,
                    tag,
                ));
            }
            if matches!(spec.tmpl, ArgTmpl::UnionStore) {
                third_key = Some(make_key(
                    cfg,
                    next_index(cfg, seq + 2),
                    ty,
                    KeySegment::Churn,
                    tag,
                ));
            }
            key
        }
        OpKind::Ttl => pick_existing_key(cfg, &marks.ttl, ty, KeySegment::Ttl, tag, rng),
        OpKind::PopulateTtl => {
            let seq = marks.ttl.fetch_add(1, Ordering::Relaxed);
            set_seq = Some(seq);
            value_size = pick_value_size(cfg.value_size_min, cfg.value_size_max, rng);
            ttl_seconds = Some(cfg.ttl_seconds);
            make_key(cfg, next_index(cfg, seq), ty, KeySegment::Ttl, tag)
        }
        OpKind::HitMiss => {
            let k1 = pick_read_key(cfg, watermarks, ty, tag, rng);
            if matches!(
                spec.tmpl,
                ArgTmpl::Mget | ArgTmpl::JsonMget | ArgTmpl::Union | ArgTmpl::Zinter
            ) {
                second_key = Some(pick_read_key(cfg, watermarks, ty, tag, rng));
            }
            k1
        }
        OpKind::Populate => {
            let use_ttl = (spec.mix_key == "set" || spec.mix_key == "json_set")
                && cfg.ttl_ratio > 0.0
                && rng.gen_bool(cfg.ttl_ratio);
            if use_ttl {
                let seq = marks.ttl.fetch_add(1, Ordering::Relaxed);
                set_seq = Some(seq);
                value_size = pick_value_size(cfg.value_size_min, cfg.value_size_max, rng);
                ttl_seconds = Some(cfg.ttl_seconds);
                let key = make_key(cfg, next_index(cfg, seq), ty, KeySegment::Ttl, tag);
                if matches!(spec.tmpl, ArgTmpl::Mset | ArgTmpl::JsonMset) {
                    second_key = Some(make_key(
                        cfg,
                        next_index(cfg, seq + 1),
                        ty,
                        KeySegment::Ttl,
                        tag,
                    ));
                }
                key
            } else {
                let seq = marks.main.fetch_add(1, Ordering::Relaxed);
                set_seq = Some(seq);
                value_size = pick_value_size(cfg.value_size_min, cfg.value_size_max, rng);
                let key = make_key(cfg, next_index(cfg, seq), ty, KeySegment::Main, tag);
                if matches!(
                    spec.tmpl,
                    ArgTmpl::Mset | ArgTmpl::JsonMset | ArgTmpl::TwoKey | ArgTmpl::UnionStore
                ) {
                    second_key = Some(make_key(
                        cfg,
                        next_index(cfg, seq + 1),
                        ty,
                        KeySegment::Main,
                        tag,
                    ));
                }
                if matches!(spec.tmpl, ArgTmpl::UnionStore) {
                    third_key = Some(make_key(
                        cfg,
                        next_index(cfg, seq + 2),
                        ty,
                        KeySegment::Main,
                        tag,
                    ));
                }
                key
            }
        }
    };

    if spec.kind == OpKind::Ttl {
        ttl_seconds = Some(cfg.ttl_seconds);
    }

    PlannedOp {
        op,
        key,
        second_key,
        third_key,
        value_size,
        set_seq,
        ttl_seconds,
    }
}

pub fn plan_batch(
    cfg: &WorkloadConfig,
    watermarks: &Watermarks,
    rng: &mut impl Rng,
) -> Vec<PlannedOp> {
    let total = cfg.mix.total().max(1);
    let slot_tag = slot_tag_for_batch(cfg, rng);
    let tag = slot_tag.as_deref();
    let mut plan = Vec::with_capacity(cfg.pipeline as usize);
    for _ in 0..cfg.pipeline {
        let op = pick_op(&cfg.mix, rng.gen_range(0..total));
        plan.push(plan_single_op(cfg, watermarks, op, tag, rng));
    }
    plan
}

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

fn payload(planned: &PlannedOp, keyspace: u64, value: &[u8]) -> Vec<u8> {
    if let Some(seq) = planned.set_seq {
        build_value_payload(seq, keyspace, planned.value_size.max(1), value)
    } else {
        let size = planned.value_size.min(value.len()).max(1);
        value[..size].to_vec()
    }
}

fn build_op_cmd(planned: &PlannedOp, keyspace: u64, value: &[u8]) -> redis::Cmd {
    let spec = planned.op.spec();
    let mut cmd = redis::cmd(spec.redis);
    let val = payload(planned, keyspace, value);
    let ttl = planned.ttl_seconds.unwrap_or(TTL_SECONDS);

    match spec.tmpl {
        ArgTmpl::Ping => {}
        ArgTmpl::Echo => {
            cmd.arg("loadgen");
        }
        ArgTmpl::Time
        | ArgTmpl::Dbsize
        | ArgTmpl::Command
        | ArgTmpl::Lastsave
        | ArgTmpl::Randomkey => {}
        ArgTmpl::Info => {
            cmd.arg("server");
        }
        ArgTmpl::Eval => {
            cmd.arg(EVAL_SCRIPT).arg(0);
        }
        ArgTmpl::ClusterInfo => {
            cmd.arg("INFO");
        }
        ArgTmpl::ConfigGet => {
            cmd.arg("GET").arg("maxmemory");
        }
        ArgTmpl::ClientId => {
            cmd.arg("ID");
        }
        ArgTmpl::ScriptExists => {
            cmd.arg("EXISTS").arg("00");
        }
        ArgTmpl::Asking | ArgTmpl::Readonly | ArgTmpl::Readwrite => {}
        ArgTmpl::Scan if spec.kind == OpKind::NoKey => {
            cmd.arg("0")
                .arg("MATCH")
                .arg(format!("{}:*", planned.key))
                .arg("COUNT")
                .arg(10);
        }
        ArgTmpl::KeyOnly
        | ArgTmpl::TypeKey
        | ArgTmpl::Dump
        | ArgTmpl::Ttl
        | ArgTmpl::Pttl
        | ArgTmpl::Persist => {
            cmd.arg(&planned.key);
        }
        ArgTmpl::ObjectEncoding => {
            cmd.arg("ENCODING").arg(&planned.key);
        }
        ArgTmpl::KeyValue => {
            cmd.arg(&planned.key).arg(&val);
            if spec.mix_key == "set" {
                if let Some(ttl) = planned.ttl_seconds {
                    cmd.arg("EX").arg(ttl);
                }
            }
        }
        ArgTmpl::KeyField => {
            cmd.arg(&planned.key).arg(FIELD);
        }
        ArgTmpl::KeyFieldValue => {
            cmd.arg(&planned.key).arg(FIELD).arg(&val);
        }
        ArgTmpl::KeyMember => {
            if spec.mix_key == "linsert" {
                cmd.arg(&planned.key).arg("BEFORE").arg(MEMBER).arg(&val);
            } else if spec.mix_key == "lrem" {
                cmd.arg(&planned.key).arg(1).arg(MEMBER);
            } else {
                cmd.arg(&planned.key).arg(MEMBER);
            }
        }
        ArgTmpl::KeyScoreMember => {
            cmd.arg(&planned.key).arg(1).arg(MEMBER);
        }
        ArgTmpl::KeyRange => {
            cmd.arg(&planned.key).arg(0).arg(-1);
        }
        ArgTmpl::KeyIndex => {
            cmd.arg(&planned.key).arg(0);
            if spec.mix_key == "lset" {
                cmd.arg(&val);
            }
        }
        ArgTmpl::KeyBitGet => {
            cmd.arg(&planned.key).arg(0);
        }
        ArgTmpl::KeyBitSet => {
            cmd.arg(&planned.key).arg(0).arg(1);
        }
        ArgTmpl::KeyRangeBytes => {
            cmd.arg(&planned.key).arg(0).arg(32);
        }
        ArgTmpl::KeySetRange => {
            cmd.arg(&planned.key).arg(0).arg(&val);
        }
        ArgTmpl::KeyIncrBy => {
            cmd.arg(&planned.key).arg(1);
        }
        ArgTmpl::KeyIncrByFloat => {
            cmd.arg(&planned.key).arg("1");
        }
        ArgTmpl::KeyHashIncr => {
            cmd.arg(&planned.key).arg(NUM_FIELD).arg(1);
        }
        ArgTmpl::KeyHashIncrFloat => {
            cmd.arg(&planned.key).arg(NUM_FIELD).arg("1");
        }
        ArgTmpl::JsonRoot => {
            cmd.arg(&planned.key).arg("$");
        }
        ArgTmpl::JsonSet => {
            cmd.arg(&planned.key).arg("$").arg(JSON_DOC);
            if let Some(ttl) = planned.ttl_seconds {
                cmd.arg(ttl);
            }
        }
        ArgTmpl::JsonPathStr => {
            cmd.arg(&planned.key).arg("$.s");
        }
        ArgTmpl::JsonPathArr => {
            cmd.arg(&planned.key).arg("$.a");
        }
        ArgTmpl::JsonPathObj => {
            cmd.arg(&planned.key).arg("$.o");
        }
        ArgTmpl::JsonPathNum => {
            cmd.arg(&planned.key).arg("$.v");
        }
        ArgTmpl::JsonNumIncr => {
            cmd.arg(&planned.key).arg("$.v").arg(1);
        }
        ArgTmpl::JsonArrAppend => {
            cmd.arg(&planned.key).arg("$.a").arg("1");
        }
        ArgTmpl::JsonUpdate => {
            cmd.arg(&planned.key).arg("$").arg("$.v").arg("2");
        }
        ArgTmpl::JsonMset => {
            cmd.arg(&planned.key).arg("$").arg(JSON_DOC);
            if let Some(second) = &planned.second_key {
                cmd.arg(second).arg("$").arg(JSON_DOC);
            }
        }
        ArgTmpl::JsonMget => {
            cmd.arg(&planned.key);
            if let Some(second) = &planned.second_key {
                cmd.arg(second);
            }
            cmd.arg("$");
        }
        ArgTmpl::Mset => {
            cmd.arg(&planned.key).arg(&val);
            if let Some(second) = &planned.second_key {
                cmd.arg(second).arg(&val);
            }
        }
        ArgTmpl::Mget => {
            cmd.arg(&planned.key);
            if let Some(second) = &planned.second_key {
                cmd.arg(second);
            }
        }
        ArgTmpl::TwoKey => {
            cmd.arg(&planned.key);
            if let Some(second) = &planned.second_key {
                cmd.arg(second);
            }
            if spec.mix_key == "smove" {
                cmd.arg(MEMBER);
            }
        }
        ArgTmpl::Lmove => {
            cmd.arg(&planned.key);
            if let Some(second) = &planned.second_key {
                cmd.arg(second);
            }
            cmd.arg("LEFT").arg("RIGHT");
        }
        ArgTmpl::Expire => {
            cmd.arg(&planned.key).arg(ttl);
        }
        ArgTmpl::Pexpire => {
            cmd.arg(&planned.key).arg(ttl.saturating_mul(1000));
        }
        ArgTmpl::ExpireAt => {
            cmd.arg(&planned.key).arg(1_800_000_000u64);
        }
        ArgTmpl::PexpireAt => {
            cmd.arg(&planned.key).arg(1_800_000_000_000u64);
        }
        ArgTmpl::Getex => {
            cmd.arg(&planned.key).arg("EX").arg(ttl);
        }
        ArgTmpl::Setex => {
            cmd.arg(&planned.key).arg(ttl).arg(&val);
        }
        ArgTmpl::Psetex => {
            cmd.arg(&planned.key)
                .arg(ttl.saturating_mul(1000))
                .arg(&val);
        }
        ArgTmpl::KeyScan => {
            cmd.arg(&planned.key).arg("0").arg("COUNT").arg(10);
        }
        ArgTmpl::Scan => {
            cmd.arg(&planned.key).arg("0").arg("COUNT").arg(10);
        }
        ArgTmpl::Zcount | ArgTmpl::ZrangeByScore => {
            cmd.arg(&planned.key).arg("-inf").arg("+inf");
        }
        ArgTmpl::ZlexCount | ArgTmpl::ZrangeByLex => {
            cmd.arg(&planned.key).arg("[a").arg("[z");
        }
        ArgTmpl::Union => {
            cmd.arg(&planned.key);
            if let Some(second) = &planned.second_key {
                cmd.arg(second);
            }
        }
        ArgTmpl::UnionStore => {
            cmd.arg(&planned.key);
            if let Some(second) = &planned.second_key {
                cmd.arg(second);
            }
            if let Some(third) = &planned.third_key {
                cmd.arg(third);
            }
        }
        ArgTmpl::Zinter => {
            cmd.arg(2).arg(&planned.key);
            if let Some(second) = &planned.second_key {
                cmd.arg(second);
            }
        }
    }
    cmd
}

pub fn build_pipeline(plan: &[PlannedOp], keyspace: u64, value: &[u8]) -> redis::Pipeline {
    let mut pipe = redis::pipe();
    for planned in plan {
        pipe.add_command(build_op_cmd(planned, keyspace, value));
        pipe.ignore();
    }
    pipe
}
