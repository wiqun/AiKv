use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::StorageAdapter;
use bytes::Bytes;

/// Sorted Set command handler
pub struct ZSetCommands {
    storage: StorageAdapter,
}

impl ZSetCommands {
    pub fn new(storage: StorageAdapter) -> Self {
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

        let count = self.storage.zset_add_in_db(db_index, &key, members)?;
        Ok(RespValue::Integer(count as i64))
    }

    /// ZREM key member [member ...]
    /// Removes the specified members from the sorted set stored at key
    pub fn zrem(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("ZREM".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let members: Vec<Bytes> = args[1..].to_vec();

        let count = self.storage.zset_rem_in_db(db_index, &key, members)?;
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

        match self.storage.zset_score_in_db(db_index, &key, &member)? {
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

        match self
            .storage
            .zset_rank_in_db(db_index, &key, &member, false)?
        {
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

        match self
            .storage
            .zset_rank_in_db(db_index, &key, &member, true)?
        {
            Some(rank) => Ok(RespValue::Integer(rank as i64)),
            None => Ok(RespValue::Null),
        }
    }

    /// ZRANGE key start stop [WITHSCORES]
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

        let members = self
            .storage
            .zset_range_in_db(db_index, &key, start, stop, false)?;

        let mut result = Vec::new();
        for (member, score) in members {
            result.push(RespValue::bulk_string(member));
            if with_scores {
                result.push(RespValue::bulk_string(Bytes::from(score.to_string())));
            }
        }

        Ok(RespValue::Array(Some(result)))
    }

    /// ZREVRANGE key start stop [WITHSCORES]
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

        let members = self
            .storage
            .zset_range_in_db(db_index, &key, start, stop, true)?;

        let mut result = Vec::new();
        for (member, score) in members {
            result.push(RespValue::bulk_string(member));
            if with_scores {
                result.push(RespValue::bulk_string(Bytes::from(score.to_string())));
            }
        }

        Ok(RespValue::Array(Some(result)))
    }

    /// ZRANGEBYSCORE key min max [WITHSCORES]
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

        let members = self
            .storage
            .zset_rangebyscore_in_db(db_index, &key, min, max, false)?;

        let mut result = Vec::new();
        for (member, score) in members {
            result.push(RespValue::bulk_string(member));
            if with_scores {
                result.push(RespValue::bulk_string(Bytes::from(score.to_string())));
            }
        }

        Ok(RespValue::Array(Some(result)))
    }

    /// ZREVRANGEBYSCORE key max min [WITHSCORES]
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

        let members = self
            .storage
            .zset_rangebyscore_in_db(db_index, &key, min, max, true)?;

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
        let count = self.storage.zset_card_in_db(db_index, &key)?;
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

        let count = self.storage.zset_count_in_db(db_index, &key, min, max)?;
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

        let new_score = self
            .storage
            .zset_incrby_in_db(db_index, &key, increment, member)?;
        Ok(RespValue::bulk_string(Bytes::from(new_score.to_string())))
    }
}
