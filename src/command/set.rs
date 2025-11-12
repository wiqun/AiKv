use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::StorageAdapter;
use bytes::Bytes;

/// Set command handler
pub struct SetCommands {
    storage: StorageAdapter,
}

impl SetCommands {
    pub fn new(storage: StorageAdapter) -> Self {
        Self {
            storage,
        }
    }

    /// SADD key member [member ...]
    /// Add the specified members to the set stored at key
    pub fn sadd(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SADD".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let members: Vec<Bytes> = args[1..].to_vec();

        let count = self.storage.set_add_in_db(db_index, &key, members)?;
        Ok(RespValue::Integer(count as i64))
    }

    /// SREM key member [member ...]
    /// Remove the specified members from the set stored at key
    pub fn srem(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SREM".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let members: Vec<Bytes> = args[1..].to_vec();

        let count = self.storage.set_rem_in_db(db_index, &key, members)?;
        Ok(RespValue::Integer(count as i64))
    }

    /// SISMEMBER key member
    /// Returns if member is a member of the set stored at key
    pub fn sismember(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("SISMEMBER".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let member = args[1].clone();

        let is_member = self.storage.set_ismember_in_db(db_index, &key, &member)?;
        Ok(RespValue::Integer(if is_member { 1 } else { 0 }))
    }

    /// SMEMBERS key
    /// Returns all the members of the set value stored at key
    pub fn smembers(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("SMEMBERS".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let members = self.storage.set_members_in_db(db_index, &key)?;
        Ok(RespValue::Array(Some(
            members.into_iter().map(RespValue::bulk_string).collect(),
        )))
    }

    /// SCARD key
    /// Returns the set cardinality (number of elements) of the set stored at key
    pub fn scard(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("SCARD".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = self.storage.set_card_in_db(db_index, &key)?;
        Ok(RespValue::Integer(count as i64))
    }

    /// SPOP key [count]
    /// Remove and return one or multiple random members from the set value stored at key
    pub fn spop(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("SPOP".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = if args.len() > 1 {
            String::from_utf8_lossy(&args[1])
                .parse::<usize>()
                .map_err(|_| AikvError::InvalidArgument("invalid count".to_string()))?
        } else {
            1
        };

        let members = self.storage.set_pop_in_db(db_index, &key, count)?;

        if members.is_empty() {
            Ok(RespValue::Null)
        } else if count == 1 {
            Ok(RespValue::bulk_string(members[0].clone()))
        } else {
            Ok(RespValue::Array(Some(
                members.into_iter().map(RespValue::bulk_string).collect(),
            )))
        }
    }

    /// SRANDMEMBER key [count]
    /// Return one or multiple random members from the set value stored at key
    pub fn srandmember(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("SRANDMEMBER".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = if args.len() > 1 {
            String::from_utf8_lossy(&args[1])
                .parse::<i64>()
                .map_err(|_| AikvError::InvalidArgument("invalid count".to_string()))?
        } else {
            1
        };

        let members = self.storage.set_randmember_in_db(db_index, &key, count)?;

        if members.is_empty() && args.len() == 1 {
            Ok(RespValue::Null)
        } else if count == 1 && args.len() == 1 {
            Ok(RespValue::bulk_string(members[0].clone()))
        } else {
            Ok(RespValue::Array(Some(
                members.into_iter().map(RespValue::bulk_string).collect(),
            )))
        }
    }

    /// SUNION key [key ...]
    /// Returns the members of the set resulting from the union of all the given sets
    pub fn sunion(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("SUNION".to_string()));
        }

        let keys: Vec<String> = args
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let members = self.storage.set_union_in_db(db_index, &keys)?;
        Ok(RespValue::Array(Some(
            members.into_iter().map(RespValue::bulk_string).collect(),
        )))
    }

    /// SINTER key [key ...]
    /// Returns the members of the set resulting from the intersection of all the given sets
    pub fn sinter(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("SINTER".to_string()));
        }

        let keys: Vec<String> = args
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let members = self.storage.set_inter_in_db(db_index, &keys)?;
        Ok(RespValue::Array(Some(
            members.into_iter().map(RespValue::bulk_string).collect(),
        )))
    }

    /// SDIFF key [key ...]
    /// Returns the members of the set resulting from the difference between the first set and all the successive sets
    pub fn sdiff(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("SDIFF".to_string()));
        }

        let keys: Vec<String> = args
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let members = self.storage.set_diff_in_db(db_index, &keys)?;
        Ok(RespValue::Array(Some(
            members.into_iter().map(RespValue::bulk_string).collect(),
        )))
    }

    /// SUNIONSTORE destination key [key ...]
    /// Store the members of the set resulting from the union of all the given sets
    pub fn sunionstore(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SUNIONSTORE".to_string()));
        }

        let dest = String::from_utf8_lossy(&args[0]).to_string();
        let keys: Vec<String> = args[1..]
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let count = self.storage.set_unionstore_in_db(db_index, &dest, &keys)?;
        Ok(RespValue::Integer(count as i64))
    }

    /// SINTERSTORE destination key [key ...]
    /// Store the members of the set resulting from the intersection of all the given sets
    pub fn sinterstore(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SINTERSTORE".to_string()));
        }

        let dest = String::from_utf8_lossy(&args[0]).to_string();
        let keys: Vec<String> = args[1..]
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let count = self.storage.set_interstore_in_db(db_index, &dest, &keys)?;
        Ok(RespValue::Integer(count as i64))
    }

    /// SDIFFSTORE destination key [key ...]
    /// Store the members of the set resulting from the difference between the first set and all the successive sets
    pub fn sdiffstore(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SDIFFSTORE".to_string()));
        }

        let dest = String::from_utf8_lossy(&args[0]).to_string();
        let keys: Vec<String> = args[1..]
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let count = self.storage.set_diffstore_in_db(db_index, &dest, &keys)?;
        Ok(RespValue::Integer(count as i64))
    }
}
