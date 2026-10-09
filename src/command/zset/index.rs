//! 内存中的分数索引。磁盘上的 `ValueType::ZSet` 仍是 member → score。

use std::collections::BTreeMap;
use std::ops::Bound;

use super::helpers::ScoreBound;

#[derive(Clone, Debug)]
enum Key {
    At {
        score: f64,
        member: Vec<u8>,
    },
    /// 同一分数的全部成员之后。只用作范围边界，不写入树。
    After {
        score: f64,
    },
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for Key {}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let score = self
            .score()
            .partial_cmp(&other.score())
            .unwrap_or(std::cmp::Ordering::Equal);
        score.then_with(|| self.tie_break(other))
    }
}

impl Key {
    fn score(&self) -> f64 {
        match self {
            Key::At { score, .. } | Key::After { score } => *score,
        }
    }

    fn tie_break(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Key::At { member: left, .. }, Key::At { member: right, .. }) => left.cmp(right),
            (Key::At { .. }, Key::After { .. }) => std::cmp::Ordering::Less,
            (Key::After { .. }, Key::At { .. }) => std::cmp::Ordering::Greater,
            (Key::After { .. }, Key::After { .. }) => std::cmp::Ordering::Equal,
        }
    }
}

/// member 映射之外的分数序。成员字节在索引里再存一份，换后续范围查询不再整表排序。
#[derive(Clone, Debug, Default)]
pub(crate) struct ScoreIndex {
    by_score: BTreeMap<Key, ()>,
}

impl ScoreIndex {
    pub(crate) fn from_members(members: &BTreeMap<Vec<u8>, f64>) -> Self {
        let mut index = Self::default();
        for (member, score) in members {
            index.insert(*score, member.clone());
        }
        index
    }

    pub(crate) fn insert(&mut self, score: f64, member: Vec<u8>) {
        self.by_score.insert(Key::At { score, member }, ());
    }

    pub(crate) fn remove(&mut self, score: f64, member: &[u8]) {
        self.by_score.remove(&Key::At {
            score,
            member: member.to_vec(),
        });
    }

    pub(crate) fn len(&self) -> usize {
        self.by_score.len()
    }

    pub(crate) fn rank(&self, member: &[u8], reverse: bool) -> Option<usize> {
        let iter: Box<dyn Iterator<Item = &[u8]> + '_> = if reverse {
            Box::new(self.members_rev())
        } else {
            Box::new(self.members_fwd())
        };
        iter.enumerate().find_map(
            |(rank, item)| {
                if item == member {
                    Some(rank)
                } else {
                    None
                }
            },
        )
    }

    /// 半开区间 `[start, end)`。`reverse` 时 0 是最高分。
    pub(crate) fn collect_rank_window(
        &self,
        start: usize,
        end: usize,
        reverse: bool,
    ) -> Vec<(Vec<u8>, f64)> {
        if start >= end {
            return Vec::new();
        }
        self.ordered(reverse)
            .skip(start)
            .take(end - start)
            .collect()
    }

    /// `count == None` 表示从 offset 取到范围结束。
    pub(crate) fn collect_score_window(
        &self,
        min: &ScoreBound,
        max: &ScoreBound,
        reverse: bool,
        offset: usize,
        count: Option<usize>,
    ) -> Vec<(Vec<u8>, f64)> {
        let (start_key, end_key) = score_bounds(min, max);
        if start_key.cmp(&end_key) != std::cmp::Ordering::Less {
            return Vec::new();
        }
        let iter = self
            .by_score
            .range((Bound::Included(start_key), Bound::Excluded(end_key)));
        let picked: Box<dyn Iterator<Item = (Vec<u8>, f64)> + '_> = if reverse {
            Box::new(iter.rev().filter_map(|(key, _)| key_member(key)))
        } else {
            Box::new(iter.filter_map(|(key, _)| key_member(key)))
        };
        match count {
            Some(count) => picked.skip(offset).take(count).collect(),
            None => picked.skip(offset).collect(),
        }
    }

    pub(crate) fn collect_extreme(&self, count: usize, max: bool) -> Vec<(Vec<u8>, f64)> {
        self.ordered(max).take(count).collect()
    }

    fn ordered(&self, reverse: bool) -> Box<dyn Iterator<Item = (Vec<u8>, f64)> + '_> {
        if reverse {
            Box::new(self.by_score.keys().rev().filter_map(key_member))
        } else {
            Box::new(self.by_score.keys().filter_map(key_member))
        }
    }

    fn members_fwd(&self) -> impl Iterator<Item = &[u8]> {
        self.by_score.keys().filter_map(|key| match key {
            Key::At { member, .. } => Some(member.as_slice()),
            Key::After { .. } => None,
        })
    }

    fn members_rev(&self) -> impl Iterator<Item = &[u8]> {
        self.by_score.iter().rev().filter_map(|(key, _)| match key {
            Key::At { member, .. } => Some(member.as_slice()),
            Key::After { .. } => None,
        })
    }
}

fn key_member(key: &Key) -> Option<(Vec<u8>, f64)> {
    match key {
        Key::At { score, member } => Some((member.clone(), *score)),
        Key::After { .. } => None,
    }
}

fn score_bounds(min: &ScoreBound, max: &ScoreBound) -> (Key, Key) {
    let (min_score, min_inclusive) = min.endpoint();
    let (max_score, max_inclusive) = max.endpoint();
    let start = if min_inclusive {
        Key::At {
            score: min_score,
            member: Vec::new(),
        }
    } else {
        Key::After { score: min_score }
    };
    let end = if max_inclusive {
        Key::After { score: max_score }
    } else {
        Key::At {
            score: max_score,
            member: Vec::new(),
        }
    };
    (start, end)
}

/// 脚本写缓冲里的有序集合。提交时只拿走 `members`。
#[derive(Clone, Debug)]
pub(crate) struct ZSetBuf {
    members: BTreeMap<Vec<u8>, f64>,
    index: ScoreIndex,
}

impl ZSetBuf {
    pub(crate) fn new() -> Self {
        Self {
            members: BTreeMap::new(),
            index: ScoreIndex::default(),
        }
    }

    pub(crate) fn from_parts(members: BTreeMap<Vec<u8>, f64>, index: Option<ScoreIndex>) -> Self {
        let index = index.unwrap_or_else(|| ScoreIndex::from_members(&members));
        Self { members, index }
    }

    pub(crate) fn members(&self) -> &BTreeMap<Vec<u8>, f64> {
        &self.members
    }

    pub(crate) fn index(&self) -> &ScoreIndex {
        &self.index
    }

    pub(crate) fn into_members(self) -> BTreeMap<Vec<u8>, f64> {
        self.members
    }

    pub(crate) fn insert(&mut self, member: Vec<u8>, score: f64) -> Option<f64> {
        let old = self.members.insert(member.clone(), score);
        if let Some(old_score) = old {
            self.index.remove(old_score, &member);
        }
        self.index.insert(score, member);
        old
    }

    pub(crate) fn remove(&mut self, member: &[u8]) -> Option<f64> {
        let old = self.members.remove(member)?;
        self.index.remove(old, member);
        Some(old)
    }
}

impl Default for ZSetBuf {
    fn default() -> Self {
        Self::new()
    }
}
