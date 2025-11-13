use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::{StorageAdapter, StoredValue};
use bytes::Bytes;
use std::collections::HashSet;

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

        // Migrated: Logic moved from storage layer to command layer
        let set = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut set = stored.as_set()?.clone();
            let mut count = 0;
            for member in &members {
                if set.insert(member.to_vec()) {
                    count += 1;
                }
            }
            (count, set)
        } else {
            let mut set = HashSet::new();
            let mut count = 0;
            for member in &members {
                if set.insert(member.to_vec()) {
                    count += 1;
                }
            }
            (count, set)
        };

        self.storage
            .set_value(db_index, key, StoredValue::new_set(set.1))?;
        Ok(RespValue::Integer(set.0 as i64))
    }

    /// SREM key member [member ...]
    /// Remove the specified members from the set stored at key
    pub fn srem(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SREM".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let members: Vec<Bytes> = args[1..].to_vec();

        // Migrated: Logic moved from storage layer to command layer
        let mut count = 0;

        if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut set = stored.as_set()?.clone();
            
            for member in &members {
                if set.remove(&member.to_vec()) {
                    count += 1;
                }
            }

            // Update or delete the set
            if set.is_empty() {
                self.storage.delete_from_db(db_index, &key)?;
            } else {
                self.storage
                    .set_value(db_index, key, StoredValue::new_set(set))?;
            }
        }

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

        let is_member = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let set = stored.as_set()?;
            set.contains(&member.to_vec())
        } else {
            false
        };

        Ok(RespValue::Integer(if is_member { 1 } else { 0 }))
    }

    /// SMEMBERS key
    /// Returns all the members of the set value stored at key
    pub fn smembers(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("SMEMBERS".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        
        let members = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let set = stored.as_set()?;
            set.iter().map(|v| Bytes::from(v.clone())).collect()
        } else {
            Vec::new()
        };

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
        
        let count = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let set = stored.as_set()?;
            set.len()
        } else {
            0
        };

        Ok(RespValue::Integer(count as i64))
    }

    /// SPOP key \[count\]
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

        // Migrated: Logic moved from storage layer to command layer
        let mut members = Vec::new();

        if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut set = stored.as_set()?.clone();
            
            let to_remove: Vec<Vec<u8>> = set.iter().take(count).cloned().collect();
            for member in to_remove {
                set.remove(&member);
                members.push(Bytes::from(member));
            }

            // Update or delete the set
            if set.is_empty() {
                self.storage.delete_from_db(db_index, &key)?;
            } else {
                self.storage
                    .set_value(db_index, key, StoredValue::new_set(set))?;
            }
        }

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

    /// SRANDMEMBER key \[count\]
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

        let members = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let set = stored.as_set()?;
            set.iter()
                .take(count.unsigned_abs() as usize)
                .map(|v| Bytes::from(v.clone()))
                .collect()
        } else {
            Vec::new()
        };

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

        let mut result = HashSet::new();
        for key in keys {
            if let Some(stored) = self.storage.get_value(db_index, &key)? {
                let set = stored.as_set()?;
                result.extend(set.iter().cloned());
            }
        }

        Ok(RespValue::Array(Some(
            result
                .into_iter()
                .map(|v| RespValue::bulk_string(Bytes::from(v)))
                .collect(),
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

        let mut result: Option<HashSet<Vec<u8>>> = None;

        for key in keys {
            if let Some(stored) = self.storage.get_value(db_index, &key)? {
                let set = stored.as_set()?;
                if let Some(res) = &mut result {
                    *res = res.intersection(set).cloned().collect();
                } else {
                    result = Some(set.clone());
                }
            } else {
                // If any key doesn't exist, intersection is empty
                return Ok(RespValue::Array(Some(Vec::new())));
            }
        }

        Ok(RespValue::Array(Some(
            result
                .unwrap_or_default()
                .into_iter()
                .map(|v| RespValue::bulk_string(Bytes::from(v)))
                .collect(),
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

        let mut result: HashSet<Vec<u8>> = HashSet::new();

        // Start with the first set
        if let Some(stored) = self.storage.get_value(db_index, &keys[0])? {
            let set = stored.as_set()?;
            result = set.clone();
        }

        // Subtract all other sets
        for key in &keys[1..] {
            if let Some(stored) = self.storage.get_value(db_index, key)? {
                let set = stored.as_set()?;
                result = result.difference(set).cloned().collect();
            }
        }

        Ok(RespValue::Array(Some(
            result
                .into_iter()
                .map(|v| RespValue::bulk_string(Bytes::from(v)))
                .collect(),
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

        let mut result = HashSet::new();
        for key in keys {
            if let Some(stored) = self.storage.get_value(db_index, &key)? {
                let set = stored.as_set()?;
                result.extend(set.iter().cloned());
            }
        }

        let count = result.len();
        self.storage.set_value(db_index, dest, StoredValue::new_set(result))?;

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

        let mut result: Option<HashSet<Vec<u8>>> = None;

        for key in keys {
            if let Some(stored) = self.storage.get_value(db_index, &key)? {
                let set = stored.as_set()?;
                if let Some(res) = &mut result {
                    *res = res.intersection(set).cloned().collect();
                } else {
                    result = Some(set.clone());
                }
            } else {
                // If any key doesn't exist, intersection is empty
                result = Some(HashSet::new());
                break;
            }
        }

        let final_set = result.unwrap_or_default();
        let count = final_set.len();
        self.storage.set_value(db_index, dest, StoredValue::new_set(final_set))?;

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

        let mut result: HashSet<Vec<u8>> = HashSet::new();

        // Start with the first set
        if let Some(stored) = self.storage.get_value(db_index, &keys[0])? {
            let set = stored.as_set()?;
            result = set.clone();
        }

        // Subtract all other sets
        for key in &keys[1..] {
            if let Some(stored) = self.storage.get_value(db_index, key)? {
                let set = stored.as_set()?;
                result = result.difference(set).cloned().collect();
            }
        }

        let count = result.len();
        self.storage.set_value(db_index, dest, StoredValue::new_set(result))?;

        Ok(RespValue::Integer(count as i64))
    }
}
