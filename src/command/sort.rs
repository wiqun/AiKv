//! `SORT` / `SORT_RO` 命令实现.
//!
//! 支持 `List` (inline / subkey), `Set` (inline / subkey), `ZSet` (inline / subkey),
//! 支持 `BY pattern`, `LIMIT offset count`, `GET pattern`, `ASC|DESC`, `ALPHA`, `STORE dest`.

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::sync::Arc;

use bytes::Bytes;

use crate::command::router::{self, KeyLock};
use crate::command::script::transaction::ScriptTransaction;
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::{
    list_subkey, subkey, zset_subkey, AiDbEngine, CollectionKind, KvStorage, ScriptIo,
    StorageEngineKind, StoredValue, ValueType,
};

#[derive(Default)]
pub(crate) struct SortOptions {
    pub by: Option<Vec<u8>>,
    pub dont_sort: bool,
    pub limit: Option<(i64, i64)>,
    pub get_patterns: Vec<Vec<u8>>,
    pub desc: bool,
    pub alpha: bool,
    pub store: Option<Vec<u8>>,
}

pub(crate) fn parse_sort_options(args: &[Bytes], readonly: bool) -> Result<SortOptions> {
    let mut opts = SortOptions::default();
    let mut i = 1;
    while i < args.len() {
        let opt = args[i].to_ascii_uppercase();
        match opt.as_slice() {
            b"BY" => {
                if i + 1 >= args.len() {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                let pat = args[i + 1].to_vec();
                if pat.eq_ignore_ascii_case(b"nosort") {
                    opts.dont_sort = true;
                    opts.by = None;
                } else {
                    opts.dont_sort = false;
                    opts.by = Some(pat);
                }
                i += 2;
            }
            b"LIMIT" => {
                if i + 2 >= args.len() {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                let offset = parse_i64_arg(&args[i + 1])?;
                let count = parse_i64_arg(&args[i + 2])?;
                opts.limit = Some((offset, count));
                i += 3;
            }
            b"GET" => {
                if i + 1 >= args.len() {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                opts.get_patterns.push(args[i + 1].to_vec());
                i += 2;
            }
            b"ASC" => {
                opts.desc = false;
                i += 1;
            }
            b"DESC" => {
                opts.desc = true;
                i += 1;
            }
            b"ALPHA" => {
                opts.alpha = true;
                i += 1;
            }
            b"STORE" => {
                if readonly || i + 1 >= args.len() {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                opts.store = Some(args[i + 1].to_vec());
                i += 2;
            }
            _ => return Err(Error::Command("ERR syntax error".into())),
        }
    }
    Ok(opts)
}

fn parse_i64_arg(raw: &[u8]) -> Result<i64> {
    let s = std::str::from_utf8(raw)
        .map_err(|_| Error::Command("ERR value is not an integer or out of range".into()))?;
    s.parse::<i64>()
        .map_err(|_| Error::Command("ERR value is not an integer or out of range".into()))
}

enum PatternTarget {
    SelfElement,
    StringKey(Vec<u8>),
    HashField { key: Vec<u8>, field: Vec<u8> },
}

fn expand_pattern(pattern: &[u8], element: &[u8]) -> PatternTarget {
    if pattern == b"#" {
        return PatternTarget::SelfElement;
    }
    if let Some(arrow_pos) = pattern.windows(2).position(|w| w == b"->") {
        let key_pat = &pattern[..arrow_pos];
        let field = pattern[arrow_pos + 2..].to_vec();
        let key = substitute_star(key_pat, element);
        return PatternTarget::HashField { key, field };
    }
    PatternTarget::StringKey(substitute_star(pattern, element))
}

fn substitute_star(pattern: &[u8], element: &[u8]) -> Vec<u8> {
    if let Some(pos) = pattern.iter().position(|&b| b == b'*') {
        let mut out = Vec::with_capacity(pattern.len() - 1 + element.len());
        out.extend_from_slice(&pattern[..pos]);
        out.extend_from_slice(element);
        out.extend_from_slice(&pattern[pos + 1..]);
        out
    } else {
        pattern.to_vec()
    }
}

#[cfg(feature = "cluster")]
fn check_same_slot(base_key: &[u8], other_key: &[u8]) -> Result<()> {
    if crate::cluster::state::CLUSTER_STATE_MGR.get().is_some() {
        crate::cluster::router::check_cross_slot(&[base_key, other_key]).map_err(Error::Command)?;
    }
    Ok(())
}

#[cfg(not(feature = "cluster"))]
fn check_same_slot(_base_key: &[u8], _other_key: &[u8]) -> Result<()> {
    Ok(())
}

#[derive(Clone)]
struct SortEntry {
    element: Vec<u8>,
    weight_bytes: Option<Vec<u8>>,
    weight_num: f64,
}

fn build_sort_entries(
    elements: Vec<Vec<u8>>,
    weights: Vec<Option<Vec<u8>>>,
    opts: &SortOptions,
) -> Result<Vec<SortEntry>> {
    let mut entries = Vec::with_capacity(elements.len());
    for (element, w_opt) in elements.into_iter().zip(weights) {
        let weight_num = if !opts.dont_sort && !opts.alpha {
            let bytes = match &w_opt {
                Some(b) => b.as_slice(),
                None if opts.by.is_none() => element.as_slice(),
                None => b"0",
            };
            let s = std::str::from_utf8(bytes).map_err(|_| {
                Error::Command("ERR One or more scores can't be converted into double".into())
            })?;
            let v: f64 = s.trim().parse().map_err(|_| {
                Error::Command("ERR One or more scores can't be converted into double".into())
            })?;
            if v.is_nan() {
                return Err(Error::Command(
                    "ERR One or more scores can't be converted into double".into(),
                ));
            }
            v
        } else {
            0.0
        };
        entries.push(SortEntry {
            element,
            weight_bytes: w_opt,
            weight_num,
        });
    }

    if !opts.dont_sort {
        entries.sort_by(|a, b| {
            let ord = if opts.alpha {
                let lhs = a.weight_bytes.as_deref().unwrap_or(if opts.by.is_none() {
                    a.element.as_slice()
                } else {
                    b""
                });
                let rhs = b.weight_bytes.as_deref().unwrap_or(if opts.by.is_none() {
                    b.element.as_slice()
                } else {
                    b""
                });
                lhs.cmp(rhs)
            } else {
                a.weight_num
                    .partial_cmp(&b.weight_num)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| a.element.cmp(&b.element))
            };
            if opts.desc {
                ord.reverse()
            } else {
                ord
            }
        });
    }

    if let Some((offset, count)) = opts.limit {
        let start = offset.max(0) as usize;
        if start >= entries.len() || count == 0 {
            entries.clear();
        } else if count < 0 {
            entries.drain(..start);
        } else {
            let end = start.saturating_add(count as usize).min(entries.len());
            entries = entries[start..end].to_vec();
        }
    }

    Ok(entries)
}

pub struct SortCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
}

impl SortCommands {
    pub fn new(storage: Arc<dyn KvStorage>, key_lock: Arc<KeyLock>) -> Self {
        Self { storage, key_lock }
    }

    pub async fn sort(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        self.execute(db, args, false).await
    }

    pub async fn sort_ro(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        self.execute(db, args, true).await
    }

    async fn execute(&self, db: usize, args: &[Bytes], readonly: bool) -> Result<RespValue> {
        let cmd_name = if readonly { "SORT_RO" } else { "SORT" };
        router::require_min_args(cmd_name, args, 1)?;
        let key = &args[0];
        let opts = parse_sort_options(args, readonly)?;

        if let Some(ref dest) = opts.store {
            check_same_slot(key, dest)?;
        }

        let elements = self.load_sortable_elements(db, key).await?;
        let weights = if let Some(ref by_pat) = opts.by {
            let mut out = Vec::with_capacity(elements.len());
            for el in &elements {
                out.push(self.lookup_pattern(db, key, by_pat, el).await?);
            }
            out
        } else {
            vec![None; elements.len()]
        };

        let sorted = build_sort_entries(elements, weights, &opts)?;
        let mut results: Vec<Option<Vec<u8>>> = Vec::new();
        if opts.get_patterns.is_empty() {
            for e in sorted {
                results.push(Some(e.element));
            }
        } else {
            for e in &sorted {
                for pat in &opts.get_patterns {
                    results.push(self.lookup_pattern(db, key, pat, &e.element).await?);
                }
            }
        }

        if let Some(dest) = opts.store {
            let _lock = self.key_lock.lock(&dest).await;
            let list_vals: VecDeque<Vec<u8>> =
                results.into_iter().map(|v| v.unwrap_or_default()).collect();
            let count = list_vals.len() as i64;
            if list_vals.is_empty() {
                let _ = self.storage.delete(db, &dest).await?;
            } else if self.storage.engine_kind() == StorageEngineKind::AiDb
                && list_vals.len() > list_subkey::list_max_inline()
            {
                let ops = list_subkey::replace_list_ops(db, &dest, &list_vals, None);
                self.storage.write_batch(db, ops).await?;
            } else {
                self.storage
                    .set_typed(db, &dest, StoredValue::new_list(list_vals))
                    .await?;
            }
            return Ok(router::integer(count));
        }

        Ok(RespValue::Array(Some(
            results
                .into_iter()
                .map(|v| match v {
                    Some(b) => router::bulk(b),
                    None => router::nil_bulk(),
                })
                .collect(),
        )))
    }

    async fn load_sortable_elements(&self, db: usize, key: &[u8]) -> Result<Vec<Vec<u8>>> {
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(Vec::new());
        };
        match stored.value {
            ValueType::List(list) => Ok(list.into_iter().collect()),
            ValueType::Set(set) => Ok(set.into_iter().collect()),
            ValueType::ZSet(zset) => Ok(zset.into_keys().collect()),
            ValueType::CollectionHeader {
                kind: CollectionKind::List,
                version,
                ..
            } => {
                let encoded = AiDbEngine::encode_key(db, key);
                Ok(
                    list_subkey::load_list_full(self.storage.as_ref(), db, &encoded, version)
                        .await?
                        .into_iter()
                        .collect(),
                )
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                version,
                ..
            } => {
                let encoded = AiDbEngine::encode_key(db, key);
                let prefix = subkey::set_subkey_prefix(&encoded, version);
                let out = Arc::new(std::sync::Mutex::new(Vec::new()));
                let out_c = Arc::clone(&out);
                self.storage
                    .raw_subkey_for_each(
                        db,
                        prefix,
                        Box::new(move |k, _| {
                            if let Some((CollectionKind::Set, member)) = subkey::decode_subkey(&k) {
                                out_c.lock().unwrap().push(member);
                            }
                            Ok(())
                        }),
                    )
                    .await?;
                Ok(Arc::try_unwrap(out).unwrap().into_inner().unwrap())
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::ZSet,
                version,
                ..
            } => {
                let map =
                    zset_subkey::load_members(self.storage.as_ref(), db, key, version).await?;
                Ok(map.into_keys().collect())
            }
            _ => Err(router::wrongtype()),
        }
    }

    async fn lookup_pattern(
        &self,
        db: usize,
        base_key: &[u8],
        pattern: &[u8],
        element: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        match expand_pattern(pattern, element) {
            PatternTarget::SelfElement => Ok(Some(element.to_vec())),
            PatternTarget::StringKey(target_key) => {
                check_same_slot(base_key, &target_key)?;
                let Some(stored) = self.storage.get_typed(db, &target_key).await? else {
                    return Ok(None);
                };
                Ok(match stored.value {
                    ValueType::String(v) => Some(v),
                    _ => None,
                })
            }
            PatternTarget::HashField {
                key: target_key,
                field,
            } => {
                check_same_slot(base_key, &target_key)?;
                let Some(stored) = self.storage.get_typed(db, &target_key).await? else {
                    return Ok(None);
                };
                match stored.value {
                    ValueType::Hash(map) => Ok(map.get(&field).cloned()),
                    ValueType::CollectionHeader {
                        kind: CollectionKind::Hash,
                        version,
                        ..
                    } => {
                        let encoded = AiDbEngine::encode_key(db, &target_key);
                        let sk = subkey::encode_hash_field_key(&encoded, version, &field);
                        self.storage.raw_subkey_get(db, sk).await
                    }
                    _ => Ok(None),
                }
            }
        }
    }
}

/// 同步版本 `SORT` / `SORT_RO` 供 Lua `SCRIPT_COMMANDS` 调用.
pub(crate) fn exec_sort_sync(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
    readonly: bool,
) -> Result<RespValue> {
    let cmd_name = if readonly { "SORT_RO" } else { "SORT" };
    if args.is_empty() {
        return Err(router::wrong_args(cmd_name, ""));
    }
    let key = &args[0];
    let opts = parse_sort_options(args, readonly)?;
    if let Some(ref dest) = opts.store {
        check_same_slot(key, dest)?;
    }

    let elements: Vec<Vec<u8>> = match txn.get_value(storage, key)? {
        None => Vec::new(),
        Some(stored) => match stored.value {
            ValueType::List(list) => list.into_iter().collect(),
            ValueType::Set(set) => set.into_iter().collect(),
            ValueType::ZSet(zset) => zset.into_keys().collect(),
            ValueType::CollectionHeader {
                kind: CollectionKind::ZSet,
                ..
            } => txn
                .visible_subkey_members(storage, key)?
                .into_keys()
                .collect(),
            _ => return Err(router::wrongtype()),
        },
    };

    let weights = if let Some(ref by_pat) = opts.by {
        let mut out = Vec::with_capacity(elements.len());
        for el in &elements {
            out.push(lookup_pattern_sync(storage, txn, key, by_pat, el)?);
        }
        out
    } else {
        vec![None; elements.len()]
    };

    let sorted = build_sort_entries(elements, weights, &opts)?;
    let mut results: Vec<Option<Vec<u8>>> = Vec::new();
    if opts.get_patterns.is_empty() {
        for e in sorted {
            results.push(Some(e.element));
        }
    } else {
        for e in &sorted {
            for pat in &opts.get_patterns {
                results.push(lookup_pattern_sync(storage, txn, key, pat, &e.element)?);
            }
        }
    }

    if let Some(dest) = opts.store {
        let list_vals: VecDeque<Vec<u8>> =
            results.into_iter().map(|v| v.unwrap_or_default()).collect();
        let count = list_vals.len() as i64;
        if list_vals.is_empty() {
            txn.delete(dest);
        } else {
            txn.replace_stored(dest, StoredValue::new_list(list_vals))?;
        }
        return Ok(router::integer(count));
    }

    Ok(RespValue::Array(Some(
        results
            .into_iter()
            .map(|v| match v {
                Some(b) => router::bulk(b),
                None => router::nil_bulk(),
            })
            .collect(),
    )))
}

fn lookup_pattern_sync(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    base_key: &[u8],
    pattern: &[u8],
    element: &[u8],
) -> Result<Option<Vec<u8>>> {
    match expand_pattern(pattern, element) {
        PatternTarget::SelfElement => Ok(Some(element.to_vec())),
        PatternTarget::StringKey(target_key) => {
            check_same_slot(base_key, &target_key)?;
            txn.get(storage, &target_key)
        }
        PatternTarget::HashField {
            key: target_key,
            field,
        } => {
            check_same_slot(base_key, &target_key)?;
            if txn.uses_subkey_hash(storage, &target_key)? {
                txn.point_hash_field(storage, &target_key, &field)
            } else {
                Ok(txn
                    .with_hash(storage, &target_key, |h| Ok(h.get(&field).cloned()))?
                    .flatten())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryEngine;
    use std::collections::{HashMap, HashSet};

    #[tokio::test]
    async fn sort_and_sort_ro_by_get_alpha_limit_store() {
        let storage = MemoryEngine::new(1);
        let cmds = SortCommands::new(storage.clone(), Arc::new(KeyLock::new(64)));

        let mut set = HashSet::new();
        set.insert(b"10".to_vec());
        set.insert(b"2".to_vec());
        set.insert(b"1".to_vec());
        storage
            .set_typed(0, b"nums", StoredValue::new_set(set))
            .await
            .unwrap();

        // 数值 ASC
        let res = cmds.sort(0, &[Bytes::from_static(b"nums")]).await.unwrap();
        assert_eq!(
            res,
            RespValue::Array(Some(vec![
                router::bulk(b"1".to_vec()),
                router::bulk(b"2".to_vec()),
                router::bulk(b"10".to_vec()),
            ]))
        );

        // ALPHA DESC LIMIT 1 2
        let res = cmds
            .sort_ro(
                0,
                &[
                    Bytes::from_static(b"nums"),
                    Bytes::from_static(b"ALPHA"),
                    Bytes::from_static(b"DESC"),
                    Bytes::from_static(b"LIMIT"),
                    Bytes::from_static(b"1"),
                    Bytes::from_static(b"2"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(
            res,
            RespValue::Array(Some(vec![
                router::bulk(b"10".to_vec()),
                router::bulk(b"1".to_vec()),
            ]))
        );

        // SORT_RO 拒绝 STORE
        assert!(cmds
            .sort_ro(
                0,
                &[
                    Bytes::from_static(b"nums"),
                    Bytes::from_static(b"STORE"),
                    Bytes::from_static(b"dest"),
                ]
            )
            .await
            .is_err());

        // BY & GET Hash field + STORE
        for (id, w, name) in [
            ("1", "30", "alice"),
            ("2", "10", "bob"),
            ("10", "20", "carol"),
        ] {
            let mut h = HashMap::new();
            h.insert(b"weight".to_vec(), w.as_bytes().to_vec());
            h.insert(b"name".to_vec(), name.as_bytes().to_vec());
            storage
                .set_typed(
                    0,
                    format!("obj_{id}").as_bytes(),
                    StoredValue {
                        value: ValueType::Hash(h),
                        expires_at: None,
                    },
                )
                .await
                .unwrap();
        }
        let stored_count = cmds
            .sort(
                0,
                &[
                    Bytes::from_static(b"nums"),
                    Bytes::from_static(b"BY"),
                    Bytes::from_static(b"obj_*->weight"),
                    Bytes::from_static(b"GET"),
                    Bytes::from_static(b"#"),
                    Bytes::from_static(b"GET"),
                    Bytes::from_static(b"obj_*->name"),
                    Bytes::from_static(b"STORE"),
                    Bytes::from_static(b"sorted_out"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(stored_count, router::integer(6));
    }
}
