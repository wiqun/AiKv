//! 脚本内子键布局 ZSET 的读写视图.
//!
//! 存储上的 `Z` (member → score) 与 `R` (分数索引) 加上本次脚本的成员增量 `ZSetDelta`
//! 有序合并, 读取只触达所需的分数区间, 不展开整表; 写入只更新增量, 提交时一次 `CollectionWrite`.
//! 增量中出现过的成员以增量为准, 存储中的同名成员被屏蔽.

use std::cmp::Ordering;
use std::collections::HashMap;

use super::ScriptTransaction;
use crate::error::Result;
use crate::storage::collection::{MemberDelta, ZSetDelta};
use crate::storage::subkey::ScoreSpan;
use crate::storage::KvStorage;
use crate::storage::{CollectionKind, StoredBatchOp, ValueType};

/// `(member, score)`, 按分数 (再按成员字节) 排序.
type Entry = (Vec<u8>, f64);

impl ScriptTransaction {
    /// 子键布局 ZSET 的增量来源: 先看 burst 覆盖层, 再看本次装入的 header. 其它情况返回 None.
    fn seed_zset(&self, key: &[u8]) -> Option<ZSetDelta> {
        match self.overlay_get(key) {
            Some(StoredBatchOp::ZSetDelta(delta)) => return Some(delta),
            Some(_) => return None,
            None => {}
        }
        let seen = self.seen.lock().unwrap();
        let stored = seen.get(key)?.cached.as_ref()?;
        match stored.value {
            ValueType::CollectionHeader {
                kind: CollectionKind::ZSet,
                count,
                version,
            } => Some(ZSetDelta::new(version, count, stored.expires_at)),
            _ => None,
        }
    }

    /// 只读快照: 子键布局 ZSET 返回当前增量 (成员可能为空), 其它情况返回 None. 不改变脚本状态.
    pub(in crate::command::script) async fn zset_snapshot(
        &self,
        storage: &dyn KvStorage,
        key: &[u8],
    ) -> Result<Option<ZSetDelta>> {
        if let Some(delta) = self.zset_deltas.get(key) {
            return Ok(Some(delta.clone()));
        }
        if self.write_buffer.contains_key(key) {
            return Ok(None);
        }
        self.ensure_cached(storage, key).await?;
        Ok(self.seed_zset(key))
    }

    /// 写路径: 若 key 是子键布局的 ZSET, 把它的增量装入本次脚本并返回 true.
    /// 内联 ZSET、其它类型与不存在的 key 返回 false, 由调用方走内联路径.
    pub(in crate::command::script) async fn zset_sub(
        &mut self,
        storage: &dyn KvStorage,
        key: &[u8],
    ) -> Result<bool> {
        if self.zset_deltas.contains_key(key) {
            return Ok(true);
        }
        if self.write_buffer.contains_key(key) {
            return Ok(false);
        }
        self.ensure_cached(storage, key).await?;
        match self.seed_zset(key) {
            Some(delta) => {
                self.zset_deltas.insert(key.to_vec(), delta);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// 写路径取得已装入的增量 (调用前须已 `zset_sub` 返回 true).
    pub(in crate::command::script) fn zset_delta_mut(
        &mut self,
        key: &[u8],
    ) -> Option<&mut ZSetDelta> {
        self.zset_deltas.get_mut(key)
    }
}

fn cmp_entries(a: &Entry, b: &Entry) -> Ordering {
    a.1.partial_cmp(&b.1)
        .unwrap_or(Ordering::Equal)
        .then_with(|| a.0.cmp(&b.0))
}

/// 按方向比较: `reverse` 时分数与成员都取降序.
fn dir_cmp(a: &Entry, b: &Entry, reverse: bool) -> Ordering {
    let order = cmp_entries(a, b);
    if reverse {
        order.reverse()
    } else {
        order
    }
}

/// 成员在磁盘上的分数 (仅查询不在增量中的成员).
async fn disk_scores(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    delta: &ZSetDelta,
    members: &[&[u8]],
) -> Result<HashMap<Vec<u8>, Option<f64>>> {
    let probe: Vec<&[u8]> = members
        .iter()
        .copied()
        .filter(|m| !delta.members.contains_key(*m))
        .collect();
    if probe.is_empty() {
        return Ok(HashMap::new());
    }
    let scores = storage
        .zset_subkey_scores(db, key, delta.version, &probe)
        .await?;
    Ok(probe.iter().map(|m| m.to_vec()).zip(scores).collect())
}

/// 成员的 (磁盘分数, 当前分数); 增量中的成员以增量记录为准.
fn member_state(
    delta: &ZSetDelta,
    disk: &HashMap<Vec<u8>, Option<f64>>,
    member: &[u8],
) -> (Option<f64>, Option<f64>) {
    match delta.members.get(member) {
        Some(change) => (change.on_disk, change.current),
        None => {
            let on_disk = disk.get(member).copied().flatten();
            (on_disk, on_disk)
        }
    }
}

/// 成员当前分数.
pub(in crate::command::script) async fn score_of(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    delta: &ZSetDelta,
    member: &[u8],
) -> Result<Option<f64>> {
    if let Some(change) = delta.members.get(member) {
        return Ok(change.current);
    }
    let mut scores = storage
        .zset_subkey_scores(db, key, delta.version, &[member])
        .await?;
    Ok(scores.pop().flatten())
}

/// 当前视图中分数落在 `span` 内的成员, 按 `reverse` 方向取第 `offset` 条起的至多 `limit` 条.
///
/// 存储侧多取 `|增量|` 条: 增量屏蔽的存储成员至多这么多, 因此合并后的前缀是正确的.
#[allow(clippy::too_many_arguments)]
pub(in crate::command::script) async fn range(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    delta: &ZSetDelta,
    span: &ScoreSpan,
    reverse: bool,
    offset: usize,
    limit: Option<usize>,
) -> Result<Vec<Entry>> {
    if limit == Some(0) {
        return Ok(Vec::new());
    }
    let fetch = limit.map(|n| offset.saturating_add(n).saturating_add(delta.members.len()));
    let stored = storage
        .zset_range(db, key, delta.version, span, reverse, 0, fetch)
        .await?;
    let mut live = stored
        .into_iter()
        .filter(|(member, _)| !delta.members.contains_key(member.as_slice()))
        .peekable();
    let mut fresh: Vec<Entry> = delta
        .members
        .iter()
        .filter_map(|(member, change)| {
            change
                .current
                .filter(|score| span.contains(*score))
                .map(|score| (member.clone(), score))
        })
        .collect();
    fresh.sort_by(|a, b| dir_cmp(a, b, reverse));
    let mut fresh = fresh.into_iter().peekable();

    let want = limit.map(|n| offset.saturating_add(n));
    let mut merged = Vec::new();
    while want.is_none_or(|n| merged.len() < n) {
        let take_live = match (live.peek(), fresh.peek()) {
            (Some(l), Some(f)) => dir_cmp(l, f, reverse) == Ordering::Less,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        let next = if take_live { live.next() } else { fresh.next() };
        if let Some(entry) = next {
            merged.push(entry);
        }
    }
    Ok(merged.into_iter().skip(offset).collect())
}

/// 成员在当前视图中的名次 (0 起, 按 `reverse` 计). 不存在返回 None. 复杂度 O(N).
pub(in crate::command::script) async fn rank_of(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    delta: &ZSetDelta,
    member: &[u8],
    reverse: bool,
) -> Result<Option<usize>> {
    if score_of(storage, db, key, delta, member).await?.is_none() {
        return Ok(None);
    }
    let all = range(storage, db, key, delta, &ScoreSpan::FULL, reverse, 0, None).await?;
    Ok(all.iter().position(|(m, _)| m.as_slice() == member))
}

/// ZADD: 写入增量, 返回新增成员数 (与内联路径一致, 同名成员只计一次).
pub(in crate::command::script) async fn add(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    delta: &mut ZSetDelta,
    pairs: &[(Vec<u8>, f64)],
) -> Result<i64> {
    let members: Vec<&[u8]> = pairs.iter().map(|(m, _)| m.as_slice()).collect();
    let disk = disk_scores(storage, db, key, delta, &members).await?;
    let mut added = 0i64;
    for (member, score) in pairs {
        let (on_disk, prior) = member_state(delta, &disk, member);
        if prior.is_none() {
            added += 1;
        }
        delta.members.insert(
            member.clone(),
            MemberDelta {
                on_disk,
                current: Some(*score),
            },
        );
    }
    Ok(added)
}

/// ZREM: 写入增量, 返回实际删除的成员数.
pub(in crate::command::script) async fn remove(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    delta: &mut ZSetDelta,
    members: &[&[u8]],
) -> Result<i64> {
    let disk = disk_scores(storage, db, key, delta, members).await?;
    let mut removed = 0i64;
    for member in members {
        let (on_disk, prior) = member_state(delta, &disk, member);
        if prior.is_none() {
            continue;
        }
        removed += 1;
        delta.members.insert(
            member.to_vec(),
            MemberDelta {
                on_disk,
                current: None,
            },
        );
    }
    Ok(removed)
}
