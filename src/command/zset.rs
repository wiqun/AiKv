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
        if args.len() < 3 || args.len().is_multiple_of(2) {
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

    /// ZRANGEBYSCORE key min max \[WITHSCORES\]
    /// Returns all the elements in the sorted set at key with a score between min and max
    pub fn zrangebyscore(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 3 {
            return Err(AikvError::WrongArgCount("ZRANGEBYSCORE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let min = String::from_utf8_lossy(&args[1])
            .parse::<f64>()
            .map_err(|_| AikvError::InvalidArgument("invalid min score".to_string()))?;
        let max = String::from_utf8_lossy(&args[2])
            .parse::<f64>()
            .map_err(|_| AikvError::InvalidArgument("invalid max score".to_string()))?;

        let with_scores =
            args.len() > 3 && String::from_utf8_lossy(&args[3]).to_uppercase() == "WITHSCORES";

        // Migrated: Logic moved from storage layer to command layer
        let members = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            let mut result: Vec<_> = zset
                .iter()
                .filter(|(_, s)| **s >= min && **s <= max)
                .map(|(m, s)| (Bytes::from(m.to_vec()), *s))
                .collect();

            result.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            result
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

    /// ZREVRANGEBYSCORE key max min \[WITHSCORES\]
    /// Returns all the elements in the sorted set at key with a score between max and min (in reverse order)
    pub fn zrevrangebyscore(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 3 {
            return Err(AikvError::WrongArgCount("ZREVRANGEBYSCORE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let max = String::from_utf8_lossy(&args[1])
            .parse::<f64>()
            .map_err(|_| AikvError::InvalidArgument("invalid max score".to_string()))?;
        let min = String::from_utf8_lossy(&args[2])
            .parse::<f64>()
            .map_err(|_| AikvError::InvalidArgument("invalid min score".to_string()))?;

        let with_scores =
            args.len() > 3 && String::from_utf8_lossy(&args[3]).to_uppercase() == "WITHSCORES";

        // Migrated: Logic moved from storage layer to command layer
        let members = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            let mut result: Vec<_> = zset
                .iter()
                .filter(|(_, s)| **s >= min && **s <= max)
                .map(|(m, s)| (Bytes::from(m.to_vec()), *s))
                .collect();

            result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap()); // Reverse order
            result
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
        let min = String::from_utf8_lossy(&args[1])
            .parse::<f64>()
            .map_err(|_| AikvError::InvalidArgument("invalid min score".to_string()))?;
        let max = String::from_utf8_lossy(&args[2])
            .parse::<f64>()
            .map_err(|_| AikvError::InvalidArgument("invalid max score".to_string()))?;

        // Migrated: Logic moved from storage layer to command layer
        let count = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let zset = stored.as_zset()?;
            zset.values().filter(|s| **s >= min && **s <= max).count()
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
}
