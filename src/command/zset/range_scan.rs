//! 大 ZSET 的有界范围读: 只扫 score 索引中需要的条数, 不加载全部成员.
//!
//! score 索引 key 为 `前缀 + sortable(score) + member`, 字节序即 (score, member) 序,
//! 与 `sorted_by_score` 的排序一致, 所以范围和分页可以直接落到索引的有界扫描上.
//! 只处理 header 形式的大 ZSET; 小集合 (整包存储) 和不支持有界扫描的后端返回 `None`, 调用方回退到全量路径.

use crate::command::zset::helpers::{normalize_range, ScoreBound};
use crate::error::Result;
use crate::storage::zset_subkey::{
    is_zset_header, score_index_entry, score_prefix, sortable_score,
};
use crate::storage::{AiDbEngine, KvStorage};

/// 有序读取的结果: (member, score).
pub(crate) type ScoredMembers = Vec<(Vec<u8>, f64)>;

/// `ZRANGEBYSCORE` / `ZREVRANGEBYSCORE` 的解析结果. `min` / `max` 已按升序语义摆好.
#[derive(Debug, Clone)]
pub(crate) struct ScoreWindow {
    pub(crate) min: ScoreBound,
    pub(crate) max: ScoreBound,
    pub(crate) reverse: bool,
    pub(crate) offset: usize,
    /// `None` 表示不限.
    pub(crate) count: Option<usize>,
}

impl ScoreWindow {
    /// 需要从索引里取多少条才够 `offset + count`.
    pub(crate) fn fetch_limit(&self) -> usize {
        match self.count {
            Some(count) => self.offset.saturating_add(count),
            None => usize::MAX,
        }
    }
}

/// 索引里 `-0.0` 排在 `+0.0` 前面, 但两者 score 相等. 边界按值相等处理, 所以 0 要落到两者的外侧.
fn zero_low(value: f64) -> f64 {
    if value == 0.0 {
        -0.0
    } else {
        value
    }
}

fn zero_high(value: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else {
        value
    }
}

fn bound_key(prefix: &[u8], value: f64) -> Vec<u8> {
    let mut key = prefix.to_vec();
    key.extend_from_slice(&sortable_score(value));
    key
}

/// score 范围对应的索引 key 半开区间 `[start, end)`. 空区间返回 `None`.
pub(crate) fn score_scan_bounds(
    prefix: &[u8],
    min: &ScoreBound,
    max: &ScoreBound,
) -> Option<(Vec<u8>, Vec<u8>)> {
    if min.value.is_nan() || max.value.is_nan() {
        return None;
    }
    let start = if min.inclusive {
        bound_key(prefix, zero_low(min.value))
    } else {
        AiDbEngine::prefix_end(&bound_key(prefix, zero_high(min.value)))?
    };
    let end = if max.inclusive {
        AiDbEngine::prefix_end(&bound_key(prefix, zero_high(max.value)))?
    } else {
        bound_key(prefix, zero_low(max.value))
    };
    (start < end).then_some((start, end))
}

/// 跳过 `offset` 条, 取 `count` 条, 并把索引 key 解码成 (member, score).
pub(crate) fn decode_page(
    prefix: &[u8],
    pairs: Vec<(Vec<u8>, Vec<u8>)>,
    offset: usize,
    count: Option<usize>,
) -> Vec<(Vec<u8>, f64)> {
    let page = pairs.into_iter().skip(offset);
    let take = count.unwrap_or(usize::MAX);
    page.take(take)
        .filter_map(|(key, _)| score_index_entry(prefix, &key))
        .collect()
}

/// 按序号读取的扫描计划: 从离目标更近的一端扫, 只取 `fetch` 条.
#[derive(Debug, Clone, Copy)]
pub(crate) struct IndexPlan {
    /// `true` 从升序头部扫, `false` 从尾部向前扫.
    pub(crate) from_head: bool,
    pub(crate) fetch: usize,
    skip: usize,
    take: usize,
    flip: bool,
}

impl IndexPlan {
    /// `len` 是集合当前成员数; 范围为空时返回 `None`. 序号规整沿用 `normalize_range`.
    pub(crate) fn new(len: usize, start: i64, stop: i64, reverse: bool) -> Option<Self> {
        if len == 0 {
            return None;
        }
        let (s, e) = normalize_range(len, start, stop);
        if s > e {
            return None;
        }
        // 换算成升序下标区间 [lo, hi].
        let (lo, hi) = if reverse {
            (len - 1 - e, len - 1 - s)
        } else {
            (s, e)
        };
        let from_head = hi < len - lo;
        let (fetch, skip) = if from_head {
            (hi + 1, lo)
        } else {
            (len - lo, len - 1 - hi)
        };
        Some(Self {
            from_head,
            fetch,
            skip,
            take: hi - lo + 1,
            flip: !from_head != reverse,
        })
    }

    /// 把扫描结果 (头部扫为升序, 尾部扫为降序) 裁成目标区间并摆成请求的顺序.
    pub(crate) fn apply<T>(&self, scanned: Vec<T>) -> Vec<T> {
        let mut out: Vec<T> = scanned
            .into_iter()
            .skip(self.skip)
            .take(self.take)
            .collect();
        if self.flip {
            out.reverse();
        }
        out
    }
}

fn score_index_bounds(db: usize, key: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let prefix = score_prefix(&AiDbEngine::encode_key(db, key));
    let end = AiDbEngine::prefix_end(&prefix).expect("score prefix ends with a tag byte");
    (prefix, end)
}

/// 已提交状态下按 score 范围读取. `Ok(None)`: 不是 header 形式的大 ZSET, 或后端不支持有界扫描.
pub(crate) async fn by_score(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    window: &ScoreWindow,
) -> Result<Option<ScoredMembers>> {
    let Some(stored) = storage.get_typed(db, key).await? else {
        return Ok(None);
    };
    if is_zset_header(&stored).is_none() {
        return Ok(None);
    }
    let (prefix, _) = score_index_bounds(db, key);
    let Some((start, end)) = score_scan_bounds(&prefix, &window.min, &window.max) else {
        return Ok(Some(Vec::new()));
    };
    let pairs = match storage
        .raw_range_scan(start, end, window.fetch_limit(), window.reverse)
        .await
    {
        Ok(pairs) => pairs,
        Err(_) => return Ok(None),
    };
    Ok(Some(decode_page(
        &prefix,
        pairs,
        window.offset,
        window.count,
    )))
}

/// 已提交状态下按序号读取 (`ZRANGE` / `ZREVRANGE`). `Ok(None)` 含义同 `by_score`.
pub(crate) async fn by_index(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    start: i64,
    stop: i64,
    reverse: bool,
) -> Result<Option<ScoredMembers>> {
    let Some(stored) = storage.get_typed(db, key).await? else {
        return Ok(None);
    };
    let Some(len) = is_zset_header(&stored) else {
        return Ok(None);
    };
    let Some(plan) = IndexPlan::new(len as usize, start, stop, reverse) else {
        return Ok(Some(Vec::new()));
    };
    let (prefix, end) = score_index_bounds(db, key);
    let pairs = match storage
        .raw_range_scan(prefix.clone(), end, plan.fetch, !plan.from_head)
        .await
    {
        Ok(pairs) => pairs,
        Err(_) => return Ok(None),
    };
    let entries = pairs
        .into_iter()
        .filter_map(|(key, _)| score_index_entry(&prefix, &key))
        .collect();
    Ok(Some(plan.apply(entries)))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::command::zset::helpers::{
        apply_limit, normalize_range, score_in_range, sorted_by_score, ScoreBound,
    };
    use crate::storage::zset_subkey::{score_key, score_prefix};
    use crate::storage::AiDbEngine;

    fn bound(value: f64, inclusive: bool) -> ScoreBound {
        ScoreBound { value, inclusive }
    }

    /// 与存储层相同的 score 索引: 按 (score 字节序, member) 排序的 engine key.
    fn index_keys(zset: &[(&str, f64)]) -> (Vec<u8>, Vec<Vec<u8>>) {
        let encoded = AiDbEngine::encode_key(0, b"z");
        let mut keys: Vec<Vec<u8>> = zset
            .iter()
            .map(|(member, score)| score_key(&encoded, *score, member.as_bytes()))
            .collect();
        keys.sort();
        (score_prefix(&encoded), keys)
    }

    fn simulate_scan(
        keys: &[Vec<u8>],
        start: &[u8],
        end: &[u8],
        limit: usize,
        reverse: bool,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut hit: Vec<Vec<u8>> = keys
            .iter()
            .filter(|k| k.as_slice() >= start && k.as_slice() < end)
            .cloned()
            .collect();
        if reverse {
            hit.reverse();
        }
        hit.into_iter()
            .take(limit)
            .map(|k| (k, Vec::new()))
            .collect()
    }

    #[test]
    fn score_window_matches_full_sort_oracle() {
        let zset = [
            ("a", f64::NEG_INFINITY),
            ("b", -5.0),
            ("c", -1.5),
            ("d", -0.0),
            ("e", 0.0),
            ("f", 1.0),
            ("g", 1.0),
            ("h", 1.0),
            ("i", 2.5),
            ("j", 7.0),
            ("k", f64::INFINITY),
        ];
        let map: BTreeMap<Vec<u8>, f64> = zset
            .iter()
            .map(|(m, s)| (m.as_bytes().to_vec(), *s))
            .collect();
        let (prefix, keys) = index_keys(&zset);
        let values = [
            f64::NEG_INFINITY,
            -5.0,
            -1.5,
            0.0,
            1.0,
            2.5,
            3.0,
            7.0,
            f64::INFINITY,
        ];
        let mut checked = 0;
        for &lo in &values {
            for &hi in &values {
                for (lo_in, hi_in) in [(true, true), (true, false), (false, true), (false, false)] {
                    for reverse in [false, true] {
                        for offset in [0usize, 1, 3, 50] {
                            for count in [None, Some(0), Some(2), Some(100)] {
                                let window = ScoreWindow {
                                    min: bound(lo, lo_in),
                                    max: bound(hi, hi_in),
                                    reverse,
                                    offset,
                                    count,
                                };
                                let mut expected: Vec<(Vec<u8>, f64)> =
                                    sorted_by_score(&map, reverse)
                                        .into_iter()
                                        .filter(|(_, s)| {
                                            score_in_range(*s, &window.min, &window.max)
                                        })
                                        .collect();
                                apply_limit(&mut expected, offset, count);

                                let got = match score_scan_bounds(&prefix, &window.min, &window.max)
                                {
                                    None => Vec::new(),
                                    Some((start, end)) => {
                                        let pairs = simulate_scan(
                                            &keys,
                                            &start,
                                            &end,
                                            window.fetch_limit(),
                                            reverse,
                                        );
                                        decode_page(&prefix, pairs, offset, count)
                                    }
                                };
                                assert_eq!(
                                    got, expected,
                                    "window lo={lo} lo_in={lo_in} hi={hi} hi_in={hi_in} rev={reverse} off={offset} cnt={count:?}"
                                );
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(checked > 1000);
    }

    #[test]
    fn nan_bound_yields_empty_range() {
        let prefix = score_prefix(&AiDbEngine::encode_key(0, b"z"));
        assert!(score_scan_bounds(&prefix, &bound(f64::NAN, true), &bound(1.0, true)).is_none());
        assert!(score_scan_bounds(&prefix, &bound(0.0, true), &bound(f64::NAN, true)).is_none());
    }

    #[test]
    fn index_plan_matches_full_sort_oracle() {
        for len in 1usize..=7 {
            let ascending: Vec<usize> = (0..len).collect();
            for start in -9i64..=9 {
                for stop in -9i64..=9 {
                    for reverse in [false, true] {
                        let mut sorted = ascending.clone();
                        if reverse {
                            sorted.reverse();
                        }
                        let (s, e) = normalize_range(len, start, stop);
                        let expected: Vec<usize> = if s > e {
                            Vec::new()
                        } else {
                            sorted[s..=e].to_vec()
                        };

                        let got = match IndexPlan::new(len, start, stop, reverse) {
                            None => Vec::new(),
                            Some(plan) => {
                                let scanned: Vec<usize> = if plan.from_head {
                                    ascending.iter().copied().take(plan.fetch).collect()
                                } else {
                                    ascending.iter().rev().copied().take(plan.fetch).collect()
                                };
                                plan.apply(scanned)
                            }
                        };
                        assert_eq!(
                            got, expected,
                            "len={len} start={start} stop={stop} rev={reverse}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn index_plan_scans_only_the_nearer_end() {
        let head = IndexPlan::new(1000, 0, 9, false).unwrap();
        assert!(head.from_head);
        assert_eq!(head.fetch, 10);
        let tail = IndexPlan::new(1000, -10, -1, false).unwrap();
        assert!(!tail.from_head);
        assert_eq!(tail.fetch, 10);
        let rev_head = IndexPlan::new(1000, 0, 0, true).unwrap();
        assert!(!rev_head.from_head);
        assert_eq!(rev_head.fetch, 1);
    }
    mod direct {
        use std::sync::Arc;

        use bytes::Bytes;
        use tempfile::TempDir;

        use crate::command::CommandRouter;
        use crate::protocol::RespValue;
        use crate::storage::zset_subkey::THREAD_LOAD_CALLS;
        use crate::storage::{AiDbEngine, KvStorage, KvStorageAdapter};

        fn b(s: &str) -> Bytes {
            Bytes::copy_from_slice(s.as_bytes())
        }

        async fn exec(router: &CommandRouter, cmd: &str, args: &[&str]) -> RespValue {
            let args: Vec<Bytes> = args.iter().map(|a| b(a)).collect();
            let mut db = 0;
            router.execute(cmd, &args, &mut db).await.unwrap()
        }

        fn strings(resp: RespValue) -> Vec<String> {
            let RespValue::Array(Some(items)) = resp else {
                panic!("expected array: {resp:?}");
            };
            items
                .into_iter()
                .map(|i| match i {
                    RespValue::BulkString(Some(raw)) => String::from_utf8(raw.to_vec()).unwrap(),
                    other => panic!("expected bulk: {other:?}"),
                })
                .collect()
        }

        /// 300 个成员: 分数 0..=99 各 3 个 (制造并列), 外加负数与小数.
        async fn big_zset(dir: &TempDir) -> (CommandRouter, Vec<(String, f64)>) {
            let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
            let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
            let router = CommandRouter::new(storage);
            let mut oracle = Vec::new();
            for i in 0..300 {
                let score = ((i * 7) % 100) as f64 - 10.0 + if i % 5 == 0 { 0.5 } else { 0.0 };
                let member = format!("m{i:03}");
                exec(&router, "ZADD", &["z", &score.to_string(), &member]).await;
                oracle.push((member, score));
            }
            oracle.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap().then_with(|| a.0.cmp(&b.0)));
            (router, oracle)
        }

        fn loads() -> usize {
            THREAD_LOAD_CALLS.with(|c| c.get())
        }

        #[tokio::test]
        async fn range_reads_match_oracle_without_loading_all_members() {
            let dir = TempDir::new().unwrap();
            let (router, asc) = big_zset(&dir).await;
            let desc: Vec<_> = asc.iter().rev().cloned().collect();
            let names = |v: &[(String, f64)]| v.iter().map(|(m, _)| m.clone()).collect::<Vec<_>>();
            let before = loads();

            let got = strings(exec(&router, "ZRANGE", &["z", "0", "9"]).await);
            assert_eq!(got, names(&asc[0..10]));
            let got = strings(exec(&router, "ZRANGE", &["z", "-5", "-1"]).await);
            assert_eq!(got, names(&asc[295..300]));
            let got = strings(exec(&router, "ZREVRANGE", &["z", "0", "4"]).await);
            assert_eq!(got, names(&desc[0..5]));
            let got = strings(exec(&router, "ZREVRANGE", &["z", "100", "109"]).await);
            assert_eq!(got, names(&desc[100..110]));
            let got = strings(exec(&router, "ZRANGE", &["z", "290", "999"]).await);
            assert_eq!(got, names(&asc[290..300]));

            let got = strings(
                exec(
                    &router,
                    "ZRANGEBYSCORE",
                    &["z", "10", "20", "LIMIT", "2", "5"],
                )
                .await,
            );
            let expect: Vec<_> = asc
                .iter()
                .filter(|(_, s)| *s >= 10.0 && *s <= 20.0)
                .skip(2)
                .take(5)
                .cloned()
                .collect();
            assert_eq!(got, names(&expect));
            let got = strings(
                exec(
                    &router,
                    "ZREVRANGEBYSCORE",
                    &["z", "(50", "(-3", "LIMIT", "0", "7"],
                )
                .await,
            );
            let expect: Vec<_> = desc
                .iter()
                .filter(|(_, s)| *s < 50.0 && *s > -3.0)
                .take(7)
                .cloned()
                .collect();
            assert_eq!(got, names(&expect));
            let got = strings(
                exec(
                    &router,
                    "ZRANGEBYSCORE",
                    &["z", "-inf", "+inf", "LIMIT", "0", "3", "WITHSCORES"],
                )
                .await,
            );
            assert_eq!(got.len(), 6);
            assert_eq!(got[0], asc[0].0);

            let RespValue::BulkString(Some(score)) = exec(&router, "ZSCORE", &["z", "m007"]).await
            else {
                panic!("zscore");
            };
            let expect = asc.iter().find(|(m, _)| m == "m007").unwrap().1;
            assert_eq!(
                String::from_utf8(score.to_vec()).unwrap(),
                expect.to_string()
            );
            assert_eq!(
                exec(&router, "ZSCORE", &["z", "nope"]).await,
                RespValue::BulkString(None)
            );

            assert_eq!(loads() - before, 0, "范围读与点查不应该加载全部成员");
        }

        #[tokio::test]
        async fn unbounded_reads_still_match_oracle() {
            let dir = TempDir::new().unwrap();
            let (router, asc) = big_zset(&dir).await;
            let got = strings(exec(&router, "ZRANGEBYSCORE", &["z", "-inf", "+inf"]).await);
            assert_eq!(got.len(), 300);
            assert_eq!(got[0], asc[0].0);
            assert_eq!(got[299], asc[299].0);
            let got = strings(exec(&router, "ZRANGE", &["z", "0", "-1"]).await);
            assert_eq!(got.len(), 300);
            let got = strings(
                exec(
                    &router,
                    "ZRANGEBYSCORE",
                    &["z", "5", "10", "LIMIT", "1", "-1"],
                )
                .await,
            );
            let expect = asc
                .iter()
                .filter(|(_, s)| *s >= 5.0 && *s <= 10.0)
                .skip(1)
                .count();
            assert_eq!(got.len(), expect);
        }
    }
}
