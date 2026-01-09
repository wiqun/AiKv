use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::{StorageEngine, StoredValue};
use bytes::Bytes;
use std::collections::BTreeMap;

/// Sorted Set command handler
pub struct ZSetCommands {
    storage: StorageEngine,
}

impl ZSetCommands {
    pub fn new(storage: StorageEngine) -> Self {
        Self {
            storage,
        }
    }

    /// ZADD key score member [score member ...]
    /// Adds all the specified members with the specified scores to the sorted set stored at key
    pub fn zadd(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 3 || args.len() % 2 == 0 {
            return Err(AikvError::WrongArgCount("ZADD".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let mut members = Vec::new();

        for i in (1..args.len()).step_by(2) {
            let score = String::from_utf8_lossy(&args[i])
                .parse::<f64>()
                .map_err(|_| AikvError::InvalidArgument("invalid score".to_string()))?;
            let member = args[i + 1].clone();
            members.push((score, member));
        }

        // Migrated: Logic moved from storage layer to command layer
        let zset = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut zset = stored.as_zset()?.clone();
            let mut count = 0;
            for (score, member) in &members {
                if zset.insert(member.to_vec(), *score).is_none() {
                    count += 1;
                }
            }
            (count, zset)
        } else {
            let mut zset = BTreeMap::new();
            let mut count = 0;
            for (score, member) in &members {
                if zset.insert(member.to_vec(), *score).is_none() {
                    count += 1;
                }
            }
            (count, zset)
        };

        self.storage
            .set_value(db_index, key, StoredValue::new_zset(zset.1))?;
        Ok(RespValue::Integer(zset.0 as i64))
    }

    /// ZREM key member [member ...]
    /// Removes the specified members from the sorted set stored at key
    pub fn zrem(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("ZREM".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let members: Vec<Bytes> = args[1..].to_vec();

        // Migrated: Logic moved from storage layer to command layer
        let mut count = 0;

        if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut zset = stored.as_zset()?.clone();

            for member in &members {
                if zset.remove(&member.to_vec()).is_some() {
                    count += 1;
                }
            }

            // Update or delete the zset
            if zset.is_empty() {
                self.storage.delete_from_db(db_index, &key)?;
            } else {
                self.storage
                    .set_value(db_index, key, StoredValue::new_zset(zset))?;
            }
        }

        Ok(RespValue::Integer(count as i64))
    }

    /// ZSCORE key member
    /// Returns the score of member in the sorted set at key
    pub fn zscore(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("ZSCORE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let member = args[1].clone();

        // Migrated: Logic moved from storage layer to command layer
        let score = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            zset.get(&member.to_vec()).copied()
        } else {
            None
        };

        match score {
            Some(score) => Ok(RespValue::bulk_string(Bytes::from(score.to_string()))),
            None => Ok(RespValue::Null),
        }
    }

    /// ZRANK key member
    /// Returns the rank of member in the sorted set stored at key, with the scores ordered from low to high
    pub fn zrank(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("ZRANK".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let member = args[1].clone();

        // Migrated: Logic moved from storage layer to command layer
        let rank = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            let member_vec = member.to_vec();

            if !zset.contains_key(&member_vec) {
                None
            } else {
                // Create sorted vec by score
                let mut sorted: Vec<_> = zset.iter().collect();
                sorted.sort_by(|a, b| a.1.partial_cmp(b.1).unwrap());

                sorted.iter().position(|(m, _)| *m == &member_vec)
            }
        } else {
            None
        };

        match rank {
            Some(rank) => Ok(RespValue::Integer(rank as i64)),
            None => Ok(RespValue::Null),
        }
    }

    /// ZREVRANK key member
    /// Returns the rank of member in the sorted set stored at key, with the scores ordered from high to low
    pub fn zrevrank(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("ZREVRANK".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let member = args[1].clone();

        // Migrated: Logic moved from storage layer to command layer
        let rank = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            let member_vec = member.to_vec();

            if !zset.contains_key(&member_vec) {
                None
            } else {
                // Create sorted vec by score (reversed)
                let mut sorted: Vec<_> = zset.iter().collect();
                sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());

                sorted.iter().position(|(m, _)| *m == &member_vec)
            }
        } else {
            None
        };

        match rank {
            Some(rank) => Ok(RespValue::Integer(rank as i64)),
            None => Ok(RespValue::Null),
        }
    }

    /// ZRANGE key start stop \[WITHSCORES\]
    /// Returns the specified range of elements in the sorted set stored at key
    pub fn zrange(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 3 {
            return Err(AikvError::WrongArgCount("ZRANGE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let start = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid start index".to_string()))?;
        let stop = String::from_utf8_lossy(&args[2])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid stop index".to_string()))?;

        let with_scores =
            args.len() > 3 && String::from_utf8_lossy(&args[3]).to_uppercase() == "WITHSCORES";

        // Migrated: Logic moved from storage layer to command layer
        let members = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            let mut sorted: Vec<_> = zset.iter().collect();
            sorted.sort_by(|a, b| a.1.partial_cmp(b.1).unwrap());

            let len = sorted.len() as i64;
            let start_idx = if start < 0 {
                (len + start).max(0)
            } else {
                start.min(len)
            } as usize;
            let stop_idx = if stop < 0 {
                (len + stop).max(-1) + 1
            } else {
                (stop + 1).min(len)
            } as usize;

            if start_idx >= stop_idx {
                Vec::new()
            } else {
                sorted
                    .iter()
                    .skip(start_idx)
                    .take(stop_idx - start_idx)
                    .map(|(m, s)| (Bytes::from(m.to_vec()), **s))
                    .collect()
            }
        } else {
            Vec::new()
        };

        let mut result = Vec::new();
        for (member, score) in members {
            result.push(RespValue::bulk_string(member));
            if with_scores {
                result.push(RespValue::bulk_string(Bytes::from(score.to_string())));
            }
        }

        Ok(RespValue::Array(Some(result)))
    }

    /// ZREVRANGE key start stop \[WITHSCORES\]
    /// Returns the specified range of elements in the sorted set stored at key, with scores ordered from high to low
    pub fn zrevrange(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 3 {
            return Err(AikvError::WrongArgCount("ZREVRANGE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let start = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid start index".to_string()))?;
        let stop = String::from_utf8_lossy(&args[2])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid stop index".to_string()))?;

        let with_scores =
            args.len() > 3 && String::from_utf8_lossy(&args[3]).to_uppercase() == "WITHSCORES";

        // Migrated: Logic moved from storage layer to command layer
        let members = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            let mut sorted: Vec<_> = zset.iter().collect();
            sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap()); // Reverse order

            let len = sorted.len() as i64;
            let start_idx = if start < 0 {
                (len + start).max(0)
            } else {
                start.min(len)
            } as usize;
            let stop_idx = if stop < 0 {
                (len + stop).max(-1) + 1
            } else {
                (stop + 1).min(len)
            } as usize;

            if start_idx >= stop_idx {
                Vec::new()
            } else {
                sorted
                    .iter()
                    .skip(start_idx)
                    .take(stop_idx - start_idx)
                    .map(|(m, s)| (Bytes::from(m.to_vec()), **s))
                    .collect()
            }
        } else {
            Vec::new()
        };

        let mut result = Vec::new();
        for (member, score) in members {
            result.push(RespValue::bulk_string(member));
            if with_scores {
                result.push(RespValue::bulk_string(Bytes::from(score.to_string())));
            }
        }

        Ok(RespValue::Array(Some(result)))
    }

    /// ZRANGEBYSCORE key min max \[WITHSCORES\] \[LIMIT offset count\]
    /// Returns all the elements in the sorted set at key with a score between min and max
    pub fn zrangebyscore(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 3 {
            return Err(AikvError::WrongArgCount("ZRANGEBYSCORE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let min_str = String::from_utf8_lossy(&args[1]).to_string();
        let max_str = String::from_utf8_lossy(&args[2]).to_string();

        // Parse optional arguments: WITHSCORES and LIMIT offset count
        let mut with_scores = false;
        let mut offset: usize = 0;
        let mut count: Option<i64> = None;

        let mut i = 3;
        while i < args.len() {
            let arg = String::from_utf8_lossy(&args[i]).to_uppercase();
            match arg.as_str() {
                "WITHSCORES" => {
                    with_scores = true;
                    i += 1;
                }
                "LIMIT" => {
                    if i + 2 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    offset = String::from_utf8_lossy(&args[i + 1]).parse().map_err(|_| {
                        AikvError::InvalidArgument("ERR value is not an integer".to_string())
                    })?;
                    count = Some(
                        String::from_utf8_lossy(&args[i + 2])
                            .parse::<i64>()
                            .map_err(|_| {
                                AikvError::InvalidArgument(
                                    "ERR value is not an integer".to_string(),
                                )
                            })?,
                    );
                    i += 3;
                }
                _ => {
                    return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                }
            }
        }

        // Parse min and max bounds
        let min_bound = Self::parse_score_bound(&min_str)?;
        let max_bound = Self::parse_score_bound(&max_str)?;

        // Migrated: Logic moved from storage layer to command layer
        let members = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            let mut result: Vec<_> = zset
                .iter()
                .filter(|(_, s)| Self::score_in_range(**s, &min_bound, &max_bound))
                .map(|(m, s)| (Bytes::from(m.to_vec()), *s))
                .collect();

            result.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

            // Apply LIMIT offset count (count of -1 means no limit)
            let result = result.into_iter().skip(offset);
            match count {
                Some(c) if c >= 0 => result.take(c as usize).collect(),
                _ => result.collect(), // -1 or None means no limit
            }
        } else {
            Vec::new()
        };

        let mut result = Vec::new();
        for (member, score) in members {
            result.push(RespValue::bulk_string(member));
            if with_scores {
                result.push(RespValue::bulk_string(Bytes::from(score.to_string())));
            }
        }

        Ok(RespValue::Array(Some(result)))
    }

    /// ZREVRANGEBYSCORE key max min \[WITHSCORES\] \[LIMIT offset count\]
    /// Returns all the elements in the sorted set at key with a score between max and min (in reverse order)
    pub fn zrevrangebyscore(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 3 {
            return Err(AikvError::WrongArgCount("ZREVRANGEBYSCORE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let max_str = String::from_utf8_lossy(&args[1]).to_string();
        let min_str = String::from_utf8_lossy(&args[2]).to_string();

        // Parse optional arguments: WITHSCORES and LIMIT offset count
        let mut with_scores = false;
        let mut offset: usize = 0;
        let mut count: Option<i64> = None;

        let mut i = 3;
        while i < args.len() {
            let arg = String::from_utf8_lossy(&args[i]).to_uppercase();
            match arg.as_str() {
                "WITHSCORES" => {
                    with_scores = true;
                    i += 1;
                }
                "LIMIT" => {
                    if i + 2 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    offset = String::from_utf8_lossy(&args[i + 1]).parse().map_err(|_| {
                        AikvError::InvalidArgument("ERR value is not an integer".to_string())
                    })?;
                    count = Some(
                        String::from_utf8_lossy(&args[i + 2])
                            .parse::<i64>()
                            .map_err(|_| {
                                AikvError::InvalidArgument(
                                    "ERR value is not an integer".to_string(),
                                )
                            })?,
                    );
                    i += 3;
                }
                _ => {
                    return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                }
            }
        }

        // Parse min and max bounds
        let min_bound = Self::parse_score_bound(&min_str)?;
        let max_bound = Self::parse_score_bound(&max_str)?;

        // Migrated: Logic moved from storage layer to command layer
        let members = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            let mut result: Vec<_> = zset
                .iter()
                .filter(|(_, s)| Self::score_in_range(**s, &min_bound, &max_bound))
                .map(|(m, s)| (Bytes::from(m.to_vec()), *s))
                .collect();

            result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap()); // Reverse order

            // Apply LIMIT offset count (count of -1 means no limit)
            let result = result.into_iter().skip(offset);
            match count {
                Some(c) if c >= 0 => result.take(c as usize).collect(),
                _ => result.collect(), // -1 or None means no limit
            }
        } else {
            Vec::new()
        };

        let mut result = Vec::new();
        for (member, score) in members {
            result.push(RespValue::bulk_string(member));
            if with_scores {
                result.push(RespValue::bulk_string(Bytes::from(score.to_string())));
            }
        }

        Ok(RespValue::Array(Some(result)))
    }

    /// ZCARD key
    /// Returns the sorted set cardinality (number of elements) of the sorted set stored at key
    pub fn zcard(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("ZCARD".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        // Migrated: Logic moved from storage layer to command layer
        let count = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            zset.len()
        } else {
            0
        };

        Ok(RespValue::Integer(count as i64))
    }

    /// ZCOUNT key min max
    /// Returns the number of elements in the sorted set at key with a score between min and max
    pub fn zcount(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("ZCOUNT".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let min_str = String::from_utf8_lossy(&args[1]).to_string();
        let max_str = String::from_utf8_lossy(&args[2]).to_string();

        // Parse min and max bounds using the same logic as ZRANGEBYSCORE
        let min_bound = Self::parse_score_bound(&min_str)?;
        let max_bound = Self::parse_score_bound(&max_str)?;

        // Migrated: Logic moved from storage layer to command layer
        let count = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            zset.values()
                .filter(|s| Self::score_in_range(**s, &min_bound, &max_bound))
                .count()
        } else {
            0
        };

        Ok(RespValue::Integer(count as i64))
    }

    /// ZINCRBY key increment member
    /// Increments the score of member in the sorted set stored at key by increment
    pub fn zincrby(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("ZINCRBY".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let increment = String::from_utf8_lossy(&args[1])
            .parse::<f64>()
            .map_err(|_| AikvError::InvalidArgument("invalid increment".to_string()))?;
        let member = args[2].clone();

        // Migrated: Logic moved from storage layer to command layer
        let zset = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut zset = stored.as_zset()?.clone();
            let member_vec = member.to_vec();
            let current = zset.get(&member_vec).copied().unwrap_or(0.0);
            let new_score = current + increment;
            zset.insert(member_vec, new_score);
            (new_score, zset)
        } else {
            let mut zset = BTreeMap::new();
            let new_score = increment; // Starting from 0.0 + increment
            zset.insert(member.to_vec(), new_score);
            (new_score, zset)
        };

        self.storage
            .set_value(db_index, key, StoredValue::new_zset(zset.1))?;
        Ok(RespValue::bulk_string(Bytes::from(zset.0.to_string())))
    }

    /// ZSCAN key cursor [MATCH pattern] [COUNT count]
    /// Incrementally iterates over members and scores of a sorted set
    pub fn zscan(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("ZSCAN".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let cursor = String::from_utf8_lossy(&args[1])
            .parse::<usize>()
            .map_err(|_| AikvError::InvalidArgument("ERR invalid cursor".to_string()))?;

        // Parse optional arguments
        let mut pattern: Option<String> = None;
        let mut count: usize = 10; // Default count

        let mut i = 2;
        while i < args.len() {
            let option = String::from_utf8_lossy(&args[i]).to_uppercase();
            match option.as_str() {
                "MATCH" => {
                    if i + 1 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    i += 1;
                    pattern = Some(String::from_utf8_lossy(&args[i]).to_string());
                }
                "COUNT" => {
                    if i + 1 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    i += 1;
                    count = String::from_utf8_lossy(&args[i])
                        .parse::<usize>()
                        .map_err(|_| {
                            AikvError::InvalidArgument(
                                "ERR value is not an integer or out of range".to_string(),
                            )
                        })?;
                }
                _ => {
                    return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                }
            }
            i += 1;
        }

        let (next_cursor, members) = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            let all_members: Vec<(Vec<u8>, f64)> =
                zset.iter().map(|(k, v)| (k.clone(), *v)).collect();

            // Filter by pattern if provided
            let filtered: Vec<(Vec<u8>, f64)> = if let Some(ref pat) = pattern {
                all_members
                    .into_iter()
                    .filter(|(m, _)| {
                        let member_str = String::from_utf8_lossy(m);
                        Self::glob_match(pat, &member_str)
                    })
                    .collect()
            } else {
                all_members
            };

            let total = filtered.len();
            if cursor >= total {
                (0, Vec::new())
            } else {
                let end = (cursor + count).min(total);
                let members: Vec<(Vec<u8>, f64)> = filtered[cursor..end].to_vec();
                let next = if end >= total { 0 } else { end };
                (next, members)
            }
        } else {
            (0, Vec::new())
        };

        // Build response: [cursor, [member, score, member, score, ...]]
        let cursor_str = Bytes::from(next_cursor.to_string());
        let mut members_arr: Vec<RespValue> = Vec::with_capacity(members.len() * 2);
        for (member, score) in members {
            members_arr.push(RespValue::bulk_string(Bytes::from(member)));
            members_arr.push(RespValue::bulk_string(Bytes::from(score.to_string())));
        }

        Ok(RespValue::Array(Some(vec![
            RespValue::bulk_string(cursor_str),
            RespValue::Array(Some(members_arr)),
        ])))
    }

    /// ZPOPMIN key \[count\]
    /// Removes and returns members with the lowest scores in a sorted set
    pub fn zpopmin(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("ZPOPMIN".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = if args.len() > 1 {
            String::from_utf8_lossy(&args[1])
                .parse::<usize>()
                .map_err(|_| {
                    AikvError::InvalidArgument(
                        "ERR value is not an integer or out of range".to_string(),
                    )
                })?
        } else {
            1
        };

        if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut zset = stored.as_zset()?.clone();

            // Sort by score (ascending)
            let mut sorted: Vec<(Vec<u8>, f64)> =
                zset.iter().map(|(k, v)| (k.clone(), *v)).collect();
            sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

            let to_pop = count.min(sorted.len());
            let popped: Vec<(Vec<u8>, f64)> = sorted.into_iter().take(to_pop).collect();

            // Remove popped elements from zset
            for (member, _) in &popped {
                zset.remove(member);
            }

            // Update or delete the zset
            if zset.is_empty() {
                self.storage.delete_from_db(db_index, &key)?;
            } else {
                self.storage
                    .set_value(db_index, key, StoredValue::new_zset(zset))?;
            }

            // Build response
            let mut result = Vec::with_capacity(popped.len() * 2);
            for (member, score) in popped {
                result.push(RespValue::bulk_string(Bytes::from(member)));
                result.push(RespValue::bulk_string(Bytes::from(score.to_string())));
            }
            Ok(RespValue::Array(Some(result)))
        } else {
            Ok(RespValue::Array(Some(vec![])))
        }
    }

    /// ZPOPMAX key \[count\]
    /// Removes and returns members with the highest scores in a sorted set
    pub fn zpopmax(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("ZPOPMAX".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = if args.len() > 1 {
            String::from_utf8_lossy(&args[1])
                .parse::<usize>()
                .map_err(|_| {
                    AikvError::InvalidArgument(
                        "ERR value is not an integer or out of range".to_string(),
                    )
                })?
        } else {
            1
        };

        if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut zset = stored.as_zset()?.clone();

            // Sort by score (descending)
            let mut sorted: Vec<(Vec<u8>, f64)> =
                zset.iter().map(|(k, v)| (k.clone(), *v)).collect();
            sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            let to_pop = count.min(sorted.len());
            let popped: Vec<(Vec<u8>, f64)> = sorted.into_iter().take(to_pop).collect();

            // Remove popped elements from zset
            for (member, _) in &popped {
                zset.remove(member);
            }

            // Update or delete the zset
            if zset.is_empty() {
                self.storage.delete_from_db(db_index, &key)?;
            } else {
                self.storage
                    .set_value(db_index, key, StoredValue::new_zset(zset))?;
            }

            // Build response
            let mut result = Vec::with_capacity(popped.len() * 2);
            for (member, score) in popped {
                result.push(RespValue::bulk_string(Bytes::from(member)));
                result.push(RespValue::bulk_string(Bytes::from(score.to_string())));
            }
            Ok(RespValue::Array(Some(result)))
        } else {
            Ok(RespValue::Array(Some(vec![])))
        }
    }

    /// ZRANGEBYLEX key min max [LIMIT offset count]
    /// Returns all elements in the sorted set with a value between min and max (lexicographically)
    /// All elements must have the same score for lexicographical ordering to work correctly
    pub fn zrangebylex(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 3 {
            return Err(AikvError::WrongArgCount("ZRANGEBYLEX".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let min_str = String::from_utf8_lossy(&args[1]).to_string();
        let max_str = String::from_utf8_lossy(&args[2]).to_string();

        // Parse LIMIT options
        let mut offset: usize = 0;
        let mut limit_count: Option<usize> = None;

        if args.len() > 3 {
            let opt = String::from_utf8_lossy(&args[3]).to_uppercase();
            if opt == "LIMIT" {
                if args.len() < 6 {
                    return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                }
                offset = String::from_utf8_lossy(&args[4])
                    .parse::<usize>()
                    .map_err(|_| {
                        AikvError::InvalidArgument(
                            "ERR value is not an integer or out of range".to_string(),
                        )
                    })?;
                limit_count = Some(String::from_utf8_lossy(&args[5]).parse::<usize>().map_err(
                    |_| {
                        AikvError::InvalidArgument(
                            "ERR value is not an integer or out of range".to_string(),
                        )
                    },
                )?);
            }
        }

        let members = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;

            // Sort by member lexicographically
            let mut sorted: Vec<Vec<u8>> = zset.keys().cloned().collect();
            sorted.sort();

            // Filter by lex range
            let filtered: Vec<Vec<u8>> = sorted
                .into_iter()
                .filter(|m| Self::in_lex_range(m, &min_str, &max_str))
                .collect();

            // Apply LIMIT
            let start = offset.min(filtered.len());
            let result: Vec<Vec<u8>> = if let Some(cnt) = limit_count {
                filtered.into_iter().skip(start).take(cnt).collect()
            } else {
                filtered.into_iter().skip(start).collect()
            };

            result
        } else {
            Vec::new()
        };

        Ok(RespValue::Array(Some(
            members
                .into_iter()
                .map(|m| RespValue::bulk_string(Bytes::from(m)))
                .collect(),
        )))
    }

    /// ZREVRANGEBYLEX key max min [LIMIT offset count]
    /// Returns all elements in the sorted set with a value between max and min (lexicographically, reversed)
    pub fn zrevrangebylex(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 3 {
            return Err(AikvError::WrongArgCount("ZREVRANGEBYLEX".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let max_str = String::from_utf8_lossy(&args[1]).to_string();
        let min_str = String::from_utf8_lossy(&args[2]).to_string();

        // Parse LIMIT options
        let mut offset: usize = 0;
        let mut limit_count: Option<usize> = None;

        if args.len() > 3 {
            let opt = String::from_utf8_lossy(&args[3]).to_uppercase();
            if opt == "LIMIT" {
                if args.len() < 6 {
                    return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                }
                offset = String::from_utf8_lossy(&args[4])
                    .parse::<usize>()
                    .map_err(|_| {
                        AikvError::InvalidArgument(
                            "ERR value is not an integer or out of range".to_string(),
                        )
                    })?;
                limit_count = Some(String::from_utf8_lossy(&args[5]).parse::<usize>().map_err(
                    |_| {
                        AikvError::InvalidArgument(
                            "ERR value is not an integer or out of range".to_string(),
                        )
                    },
                )?);
            }
        }

        let members = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;

            // Sort by member lexicographically (reversed)
            let mut sorted: Vec<Vec<u8>> = zset.keys().cloned().collect();
            sorted.sort();
            sorted.reverse();

            // Filter by lex range
            let filtered: Vec<Vec<u8>> = sorted
                .into_iter()
                .filter(|m| Self::in_lex_range(m, &min_str, &max_str))
                .collect();

            // Apply LIMIT
            let start = offset.min(filtered.len());
            let result: Vec<Vec<u8>> = if let Some(cnt) = limit_count {
                filtered.into_iter().skip(start).take(cnt).collect()
            } else {
                filtered.into_iter().skip(start).collect()
            };

            result
        } else {
            Vec::new()
        };

        Ok(RespValue::Array(Some(
            members
                .into_iter()
                .map(|m| RespValue::bulk_string(Bytes::from(m)))
                .collect(),
        )))
    }

    /// ZLEXCOUNT key min max
    /// Returns the number of elements in the sorted set with a value between min and max (lexicographically)
    pub fn zlexcount(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("ZLEXCOUNT".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let min_str = String::from_utf8_lossy(&args[1]).to_string();
        let max_str = String::from_utf8_lossy(&args[2]).to_string();

        let count = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            zset.keys()
                .filter(|m| Self::in_lex_range(m, &min_str, &max_str))
                .count()
        } else {
            0
        };

        Ok(RespValue::Integer(count as i64))
    }

    /// Check if a member is within a lexicographical range
    fn in_lex_range(member: &[u8], min: &str, max: &str) -> bool {
        let member_str = String::from_utf8_lossy(member);

        // Parse min bound
        let min_ok = if min == "-" {
            true
        } else if min.starts_with('[') {
            member_str.as_ref() >= &min[1..]
        } else if min.starts_with('(') {
            member_str.as_ref() > &min[1..]
        } else {
            return false;
        };

        // Parse max bound
        let max_ok = if max == "+" {
            true
        } else if max.starts_with('[') {
            member_str.as_ref() <= &max[1..]
        } else if max.starts_with('(') {
            member_str.as_ref() < &max[1..]
        } else {
            return false;
        };

        min_ok && max_ok
    }

    /// Simple glob pattern matching for ZSCAN
    fn glob_match(pattern: &str, text: &str) -> bool {
        let mut pattern_chars = pattern.chars().peekable();
        let mut text_chars = text.chars().peekable();

        while let Some(p) = pattern_chars.next() {
            match p {
                '*' => {
                    while pattern_chars.peek() == Some(&'*') {
                        pattern_chars.next();
                    }
                    if pattern_chars.peek().is_none() {
                        return true;
                    }
                    let remaining_pattern: String = pattern_chars.collect();
                    while text_chars.peek().is_some() {
                        let remaining_text: String = text_chars.clone().collect();
                        if Self::glob_match(&remaining_pattern, &remaining_text) {
                            return true;
                        }
                        text_chars.next();
                    }
                    return Self::glob_match(&remaining_pattern, "");
                }
                '?' => {
                    if text_chars.next().is_none() {
                        return false;
                    }
                }
                c => {
                    if text_chars.next() != Some(c) {
                        return false;
                    }
                }
            }
        }
        text_chars.peek().is_none()
    }

    /// Parse Redis score bound syntax
    fn parse_score_bound(s: &str) -> Result<ScoreBound> {
        if s == "-inf" {
            return Ok(ScoreBound::NegativeInfinity);
        }
        if s == "+inf" {
            return Ok(ScoreBound::PositiveInfinity);
        }

        if let Some(stripped) = s.strip_prefix('(') {
            let score = stripped
                .parse::<f64>()
                .map_err(|_| AikvError::InvalidArgument("invalid score".to_string()))?;
            Ok(ScoreBound::Exclusive(score))
        } else if let Some(stripped) = s.strip_prefix('[') {
            let score = stripped
                .parse::<f64>()
                .map_err(|_| AikvError::InvalidArgument("invalid score".to_string()))?;
            Ok(ScoreBound::Inclusive(score))
        } else {
            let score = s
                .parse::<f64>()
                .map_err(|_| AikvError::InvalidArgument("invalid score".to_string()))?;
            Ok(ScoreBound::Inclusive(score))
        }
    }

    /// Check if a score is within the specified bounds
    fn score_in_range(score: f64, min: &ScoreBound, max: &ScoreBound) -> bool {
        let min_ok = match min {
            ScoreBound::NegativeInfinity => true,
            ScoreBound::Inclusive(min_score) => score >= *min_score,
            ScoreBound::Exclusive(min_score) => score > *min_score,
            ScoreBound::PositiveInfinity => false,
        };

        let max_ok = match max {
            ScoreBound::PositiveInfinity => true,
            ScoreBound::Inclusive(max_score) => score <= *max_score,
            ScoreBound::Exclusive(max_score) => score < *max_score,
            ScoreBound::NegativeInfinity => false,
        };

        min_ok && max_ok
    }
}

#[derive(Debug, Clone)]
enum ScoreBound {
    NegativeInfinity,
    Inclusive(f64),
    Exclusive(f64),
    PositiveInfinity,
}
