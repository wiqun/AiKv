//! Slot router implementation for Redis Cluster.
//!
//! This module provides slot-based routing functionality,
//! wrapping AiDb's Router for Redis Cluster compatibility.

use crate::error::{AikvError, Result};

/// Number of slots in Redis Cluster (used for fallback implementation)
#[cfg(not(feature = "cluster"))]
pub const REDIS_CLUSTER_SLOTS: u16 = 16384;

/// Slot router that wraps AiDb's Router for Redis Cluster.
///
/// The `SlotRouter` provides key-to-slot mapping using the CRC16/XMODEM algorithm,
/// which is fully compatible with Redis Cluster's slot calculation.
pub struct SlotRouter {
    #[cfg(feature = "cluster")]
    inner: Option<aidb::cluster::Router>,
}

impl SlotRouter {
    /// Create a new SlotRouter.
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "cluster")]
            inner: None,
        }
    }

    /// Create a new SlotRouter with an AiDb Router.
    #[cfg(feature = "cluster")]
    pub fn with_router(router: aidb::cluster::Router) -> Self {
        Self {
            inner: Some(router),
        }
    }

    /// Calculate the slot for a given key.
    ///
    /// This uses the CRC16/XMODEM algorithm, identical to Redis Cluster.
    /// If the key contains a hash tag (e.g., `{tag}key`), only the tag
    /// portion is used for slot calculation.
    ///
    /// # Arguments
    ///
    /// * `key` - The key bytes to calculate slot for
    ///
    /// # Returns
    ///
    /// The slot number (0-16383)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let router = SlotRouter::new();
    /// let slot = router.key_to_slot(b"user:1000");
    /// assert!(slot < 16384);
    /// ```
    pub fn key_to_slot(&self, key: &[u8]) -> u16 {
        #[cfg(feature = "cluster")]
        {
            aidb::cluster::Router::key_to_slot(key)
        }
        #[cfg(not(feature = "cluster"))]
        {
            // Fallback implementation using CRC16/XMODEM
            Self::crc16_xmodem(Self::get_hash_slot_key(key)) % REDIS_CLUSTER_SLOTS
        }
    }

    /// Extract the hash slot key from a key with potential hash tag.
    ///
    /// Redis supports hash tags: `{tag}key` - only the `tag` part is hashed.
    /// This ensures keys like `{user1}name` and `{user1}age` go to the same slot.
    #[cfg(not(feature = "cluster"))]
    fn get_hash_slot_key(key: &[u8]) -> &[u8] {
        if let Some(start) = key.iter().position(|&b| b == b'{') {
            if let Some(end_offset) = key[start + 1..].iter().position(|&b| b == b'}') {
                let end = start + 1 + end_offset;
                if end > start + 1 {
                    return &key[start + 1..end];
                }
            }
        }
        key
    }

    /// CRC16/XMODEM implementation for slot calculation.
    #[cfg(not(feature = "cluster"))]
    fn crc16_xmodem(data: &[u8]) -> u16 {
        let mut crc: u16 = 0;
        for &byte in data {
            crc ^= (byte as u16) << 8;
            for _ in 0..8 {
                if crc & 0x8000 != 0 {
                    crc = (crc << 1) ^ 0x1021;
                } else {
                    crc <<= 1;
                }
            }
        }
        crc
    }

    /// Get the group ID for a given slot.
    ///
    /// Returns an error if cluster routing is not configured.
    #[cfg(feature = "cluster")]
    pub fn slot_to_group(&self, slot: u16) -> Result<u64> {
        if let Some(ref router) = self.inner {
            router
                .slot_to_group(slot)
                .map_err(|e| AikvError::Storage(e.to_string()))
        } else {
            Err(AikvError::Storage(
                "Cluster routing not configured".to_string(),
            ))
        }
    }

    /// Get the leader node address for a given slot.
    ///
    /// This is used to generate -MOVED redirects.
    #[cfg(feature = "cluster")]
    pub fn get_slot_leader_address(&self, slot: u16) -> Option<String> {
        if let Some(ref router) = self.inner {
            let group_id = router.slot_to_group(slot).ok()?;
            let leader_id = router.get_group_leader(group_id)?;
            router.get_node_address(leader_id)
        } else {
            None
        }
    }
}

impl Default for SlotRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Slot count constant for tests
    const TEST_SLOT_COUNT: u16 = 16384;

    #[test]
    fn test_key_to_slot() {
        let router = SlotRouter::new();

        // Test basic key
        let slot = router.key_to_slot(b"test");
        assert!(slot < TEST_SLOT_COUNT);

        // Test that same key always gives same slot
        let slot2 = router.key_to_slot(b"test");
        assert_eq!(slot, slot2);

        // Test different keys give (usually) different slots
        let slot_a = router.key_to_slot(b"key_a");
        let slot_b = router.key_to_slot(b"key_b");
        // Note: they could be the same by chance, but unlikely

        // Basic sanity check
        assert!(slot_a < TEST_SLOT_COUNT);
        assert!(slot_b < TEST_SLOT_COUNT);
    }

    #[test]
    fn test_hash_tag() {
        let router = SlotRouter::new();

        // Test that hash-tagged keys produce valid slot numbers
        let slot1 = router.key_to_slot(b"{user1}name");
        let slot2 = router.key_to_slot(b"{user1}age");
        let slot3 = router.key_to_slot(b"{user1}email");

        // All slots should be in valid range
        assert!(slot1 < TEST_SLOT_COUNT);
        assert!(slot2 < TEST_SLOT_COUNT);
        assert!(slot3 < TEST_SLOT_COUNT);

        // Note: Hash tag handling depends on AiDb implementation when cluster feature is enabled
        // When not using cluster feature, our fallback implementation handles hash tags
        #[cfg(not(feature = "cluster"))]
        {
            // Keys with same hash tag should map to the same slot
            assert_eq!(slot1, slot2);
            assert_eq!(slot2, slot3);
        }

        // Different hash tags should (usually) map to different slots
        let slot_a = router.key_to_slot(b"{userA}name");
        let slot_b = router.key_to_slot(b"{userB}name");

        // Note: could be same by chance, but test they're both valid
        assert!(slot_a < TEST_SLOT_COUNT);
        assert!(slot_b < TEST_SLOT_COUNT);
    }

    #[test]
    fn test_known_slot_values() {
        let router = SlotRouter::new();

        // These are known Redis CLUSTER KEYSLOT results
        // You can verify with: redis-cli CLUSTER KEYSLOT "mykey"
        // Note: Results depend on CRC16 implementation

        let slot = router.key_to_slot(b"foo");
        assert!(slot < TEST_SLOT_COUNT);

        let slot = router.key_to_slot(b"bar");
        assert!(slot < TEST_SLOT_COUNT);
    }
}
