//! Optimized broadcast message handling for zero-cost cloning
//!
//! This module provides efficient message broadcasting primitives that avoid
//! unnecessary cloning when sending the same message to multiple clients.
//!
//! Key optimizations:
//! - `BroadcastMessage`: Arc-wrapped messages for zero-cost cloning during broadcast
//! - `PreSerializedMessage`: Pre-serialized message bytes for avoiding per-client serialization
//! - `SerializationBuffer`: Pooled buffers for message serialization

use bytes::{Bytes, BytesMut};
use serde::Serialize;
use smallvec::SmallVec;
use std::sync::Arc;

use crate::protocol::{PlayerId, ServerMessage};

/// Error type for rkyv serialization operations
#[derive(Debug, Clone, thiserror::Error)]
pub enum RkyvSerializeError {
    #[error("rkyv serialization not yet implemented: {0}")]
    NotImplemented(String),
    #[error("rkyv serialization failed: {0}")]
    SerializationFailed(String),
}

/// Maximum number of clients to stack-allocate for typical room broadcasts
pub const TYPICAL_ROOM_SIZE: usize = 8;

/// A broadcast-optimized message wrapper that uses Arc for zero-cost cloning.
///
/// When broadcasting the same message to N clients, instead of cloning the
/// entire message N times (O(N * message_size)), we clone the Arc N times
/// (O(N * pointer_size)).
#[derive(Debug, Clone)]
pub struct BroadcastMessage {
    inner: Arc<ServerMessage>,
    /// Pre-serialized JSON bytes (lazily computed)
    serialized_json: Option<Arc<Bytes>>,
    /// Pre-serialized MessagePack bytes (lazily computed, reserved for future use)
    #[allow(dead_code)]
    serialized_msgpack: Option<Arc<Bytes>>,
    /// Pre-serialized rkyv bytes (lazily computed)
    serialized_rkyv: Option<Arc<Bytes>>,
}

impl BroadcastMessage {
    /// Create a new broadcast message from a ServerMessage
    #[inline]
    pub fn new(message: ServerMessage) -> Self {
        Self {
            inner: Arc::new(message),
            serialized_json: None,
            serialized_msgpack: None,
            serialized_rkyv: None,
        }
    }

    /// Create a broadcast message with pre-serialized JSON
    pub fn with_json(message: ServerMessage, json_bytes: Bytes) -> Self {
        Self {
            inner: Arc::new(message),
            serialized_json: Some(Arc::new(json_bytes)),
            serialized_msgpack: None,
            serialized_rkyv: None,
        }
    }

    /// Get reference to the underlying message
    #[inline]
    pub fn message(&self) -> &ServerMessage {
        &self.inner
    }

    /// Get or compute serialized JSON bytes
    pub fn get_or_serialize_json(&mut self) -> Result<Arc<Bytes>, serde_json::Error> {
        if let Some(ref bytes) = self.serialized_json {
            return Ok(bytes.clone());
        }

        let json = serde_json::to_vec(&*self.inner)?;
        let bytes = Arc::new(Bytes::from(json));
        self.serialized_json = Some(bytes.clone());
        Ok(bytes)
    }

    /// Get pre-serialized JSON if available
    #[inline]
    pub fn serialized_json(&self) -> Option<&Arc<Bytes>> {
        self.serialized_json.as_ref()
    }

    /// Get or compute serialized rkyv bytes
    ///
    /// TODO: This requires ServerMessage to have rkyv derives added.
    /// Once ServerMessage has #[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)],
    /// this method will perform actual serialization.
    pub fn get_or_serialize_rkyv(&mut self) -> Result<Arc<Bytes>, RkyvSerializeError> {
        if let Some(ref bytes) = self.serialized_rkyv {
            return Ok(bytes.clone());
        }

        // TODO: Replace with actual rkyv serialization once ServerMessage has the derives:
        // let rkyv_bytes = rkyv::to_bytes::<_, 256>(&*self.inner)
        //     .map_err(|e| RkyvSerializeError::SerializationFailed(e.to_string()))?;
        // let bytes = Arc::new(Bytes::copy_from_slice(&rkyv_bytes));
        // self.serialized_rkyv = Some(bytes.clone());
        // Ok(bytes)

        Err(RkyvSerializeError::NotImplemented(
            "ServerMessage does not yet have rkyv derives. Add #[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)] to ServerMessage in protocol.rs".to_string()
        ))
    }

    /// Get pre-serialized rkyv bytes if available
    #[inline]
    pub fn serialized_rkyv(&self) -> Option<&Arc<Bytes>> {
        self.serialized_rkyv.as_ref()
    }

    /// Clone just the Arc (zero-cost)
    #[inline]
    pub fn arc_clone(&self) -> Arc<ServerMessage> {
        self.inner.clone()
    }

    /// Check if this is the only reference to the message
    #[inline]
    pub fn is_unique(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }
}

impl From<ServerMessage> for BroadcastMessage {
    fn from(msg: ServerMessage) -> Self {
        Self::new(msg)
    }
}

impl AsRef<ServerMessage> for BroadcastMessage {
    fn as_ref(&self) -> &ServerMessage {
        &self.inner
    }
}

/// Pre-serialized message for avoiding per-client serialization overhead.
///
/// When broadcasting to many clients with the same encoding preference,
/// serialize once and share the bytes.
#[derive(Debug, Clone)]
pub struct PreSerializedMessage {
    /// The original message (for clients that need different encoding)
    pub message: Arc<ServerMessage>,
    /// Pre-serialized JSON bytes
    pub json_bytes: Option<Arc<Bytes>>,
    /// Pre-serialized binary (MessagePack) bytes
    pub binary_bytes: Option<Arc<Bytes>>,
    /// Pre-serialized rkyv bytes
    pub rkyv_bytes: Option<Arc<Bytes>>,
}

impl PreSerializedMessage {
    /// Create from a message, pre-serializing to JSON
    pub fn from_json(message: ServerMessage) -> Result<Self, serde_json::Error> {
        let json = serde_json::to_vec(&message)?;
        Ok(Self {
            message: Arc::new(message),
            json_bytes: Some(Arc::new(Bytes::from(json))),
            binary_bytes: None,
            rkyv_bytes: None,
        })
    }

    /// Get JSON bytes, serializing if needed
    pub fn get_json_bytes(&self) -> Result<Bytes, serde_json::Error> {
        if let Some(ref bytes) = self.json_bytes {
            return Ok((**bytes).clone());
        }
        let json = serde_json::to_vec(&*self.message)?;
        Ok(Bytes::from(json))
    }

    /// Create from a message, pre-serializing to rkyv
    ///
    /// TODO: This requires ServerMessage to have rkyv derives added.
    /// Once ServerMessage has #[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)],
    /// this method will perform actual serialization.
    pub fn from_rkyv(_message: ServerMessage) -> Result<Self, RkyvSerializeError> {
        // TODO: Replace with actual rkyv serialization once ServerMessage has the derives:
        // let rkyv_bytes = rkyv::to_bytes::<_, 256>(&message)
        //     .map_err(|e| RkyvSerializeError::SerializationFailed(e.to_string()))?;
        // Ok(Self {
        //     message: Arc::new(message),
        //     json_bytes: None,
        //     binary_bytes: None,
        //     rkyv_bytes: Some(Arc::new(Bytes::copy_from_slice(&rkyv_bytes))),
        // })

        Err(RkyvSerializeError::NotImplemented(
            "ServerMessage does not yet have rkyv derives. Add #[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)] to ServerMessage in protocol.rs".to_string()
        ))
    }

    /// Get rkyv bytes, serializing if needed
    ///
    /// TODO: This requires ServerMessage to have rkyv derives added.
    /// Once ServerMessage has #[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)],
    /// this method will perform actual serialization.
    pub fn get_rkyv_bytes(&self) -> Result<Bytes, RkyvSerializeError> {
        if let Some(ref bytes) = self.rkyv_bytes {
            return Ok((**bytes).clone());
        }

        // TODO: Replace with actual rkyv serialization once ServerMessage has the derives:
        // let rkyv_bytes = rkyv::to_bytes::<_, 256>(&*self.message)
        //     .map_err(|e| RkyvSerializeError::SerializationFailed(e.to_string()))?;
        // Ok(Bytes::copy_from_slice(&rkyv_bytes))

        Err(RkyvSerializeError::NotImplemented(
            "ServerMessage does not yet have rkyv derives. Add #[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)] to ServerMessage in protocol.rs".to_string()
        ))
    }
}

/// A pooled buffer for serialization to reduce allocations.
///
/// Typical message sizes:
/// - Control messages: 100-500 bytes
/// - Game data: 100-4000 bytes
/// - Large payloads: up to 64KB
pub struct SerializationBuffer {
    buffer: BytesMut,
    default_capacity: usize,
}

impl SerializationBuffer {
    /// Create a new serialization buffer with default capacity
    pub fn new() -> Self {
        Self::with_capacity(512)
    }

    /// Create with specified capacity
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: BytesMut::with_capacity(capacity),
            default_capacity: capacity,
        }
    }

    /// Serialize a message to JSON, returning frozen Bytes
    pub fn serialize_json<T: Serialize>(&mut self, value: &T) -> Result<Bytes, serde_json::Error> {
        self.buffer.clear();
        // Use serde_json::to_writer for efficiency
        let mut writer = self.buffer.writer();
        serde_json::to_writer(&mut writer, value)?;
        Ok(self.buffer.split().freeze())
    }

    /// Reset buffer to default capacity if it grew too large
    pub fn reset_if_oversized(&mut self, max_size: usize) {
        if self.buffer.capacity() > max_size {
            self.buffer = BytesMut::with_capacity(self.default_capacity);
        }
    }

    /// Get current buffer capacity
    pub fn capacity(&self) -> usize {
        self.buffer.capacity()
    }
}

impl Default for SerializationBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Extension trait for BytesMut to act as a Write implementation
trait BytesMutWriter {
    fn writer(&mut self) -> BytesMutWriteAdapter<'_>;
}

impl BytesMutWriter for BytesMut {
    fn writer(&mut self) -> BytesMutWriteAdapter<'_> {
        BytesMutWriteAdapter(self)
    }
}

struct BytesMutWriteAdapter<'a>(&'a mut BytesMut);

impl std::io::Write for BytesMutWriteAdapter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// List of player IDs optimized for typical room sizes.
/// Stack-allocated for rooms with up to 8 players, heap-allocated for larger.
pub type PlayerIdList = SmallVec<[PlayerId; TYPICAL_ROOM_SIZE]>;

/// Broadcast target specification
#[derive(Debug, Clone)]
pub enum BroadcastTarget {
    /// Send to all players in a room
    Room { players: PlayerIdList },
    /// Send to all players except one
    RoomExcept {
        players: PlayerIdList,
        except: PlayerId,
    },
    /// Send to a specific player
    Player(PlayerId),
}

impl BroadcastTarget {
    /// Create a room broadcast target
    pub fn room(players: impl IntoIterator<Item = PlayerId>) -> Self {
        Self::Room {
            players: players.into_iter().collect(),
        }
    }

    /// Create a room broadcast target excluding one player
    pub fn room_except(players: impl IntoIterator<Item = PlayerId>, except: PlayerId) -> Self {
        Self::RoomExcept {
            players: players.into_iter().collect(),
            except,
        }
    }

    /// Get the number of recipients
    pub fn recipient_count(&self) -> usize {
        match self {
            Self::Room { players } => players.len(),
            Self::RoomExcept { players, .. } => players.len().saturating_sub(1),
            Self::Player(_) => 1,
        }
    }

    /// Iterate over recipient player IDs
    pub fn recipients(&self) -> impl Iterator<Item = PlayerId> + '_ {
        let (players, except) = match self {
            Self::Room { players } => (players.as_slice(), None),
            Self::RoomExcept { players, except } => (players.as_slice(), Some(*except)),
            Self::Player(id) => return PlayerIdIterator::Single(Some(*id)),
        };

        PlayerIdIterator::Filtered {
            inner: players.iter().copied(),
            except,
        }
    }
}

enum PlayerIdIterator<'a> {
    Single(Option<PlayerId>),
    Filtered {
        inner: std::iter::Copied<std::slice::Iter<'a, PlayerId>>,
        except: Option<PlayerId>,
    },
}

impl Iterator for PlayerIdIterator<'_> {
    type Item = PlayerId;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Single(id) => id.take(),
            Self::Filtered { inner, except } => loop {
                let id = inner.next()?;
                if Some(id) != *except {
                    return Some(id);
                }
            },
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Single(Some(_)) => (1, Some(1)),
            Self::Single(None) => (0, Some(0)),
            Self::Filtered { inner, except } => {
                let (min, max) = inner.size_hint();
                if except.is_some() {
                    (min.saturating_sub(1), max)
                } else {
                    (min, max)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn test_broadcast_message_arc_cloning() {
        let msg = ServerMessage::Pong;
        let broadcast = BroadcastMessage::new(msg);

        // Clone should be cheap (Arc increment)
        let clone1 = broadcast.clone();
        let clone2 = broadcast.clone();

        // All clones share the same underlying data
        assert!(Arc::ptr_eq(&broadcast.inner, &clone1.inner));
        assert!(Arc::ptr_eq(&broadcast.inner, &clone2.inner));

        // Reference count should be 3
        assert_eq!(Arc::strong_count(&broadcast.inner), 3);
    }

    #[test]
    fn test_serialization_buffer_reuse() {
        let mut buffer = SerializationBuffer::with_capacity(256);

        // Serialize multiple messages - buffer can be reused for successive serializations
        let msg1 = ServerMessage::Pong;
        let bytes1 = buffer.serialize_json(&msg1).unwrap();
        assert!(!bytes1.is_empty());

        let msg2 = ServerMessage::RoomLeft;
        let bytes2 = buffer.serialize_json(&msg2).unwrap();
        assert!(!bytes2.is_empty());

        // Verify the serializations produced valid JSON
        let json1: serde_json::Value = serde_json::from_slice(&bytes1).unwrap();
        let json2: serde_json::Value = serde_json::from_slice(&bytes2).unwrap();
        assert!(json1.is_object());
        assert!(json2.is_object());
    }

    #[test]
    fn test_player_id_list_stack_allocation() {
        let mut list: PlayerIdList = SmallVec::new();

        // Add 8 players (should stay on stack)
        for _ in 0..8 {
            list.push(Uuid::new_v4());
        }
        assert!(!list.spilled(), "Should be stack-allocated for 8 players");

        // Add 9th player (should spill to heap)
        list.push(Uuid::new_v4());
        assert!(list.spilled(), "Should spill to heap for 9 players");
    }

    #[test]
    fn test_broadcast_target_recipients() {
        let players: Vec<_> = (0..4).map(|_| Uuid::new_v4()).collect();
        let except = players[1];

        let target = BroadcastTarget::room_except(players, except);

        let recipients: Vec<_> = target.recipients().collect();
        assert_eq!(recipients.len(), 3);
        assert!(!recipients.contains(&except));
    }

    #[test]
    fn test_pre_serialized_message() {
        let msg = ServerMessage::Pong;
        let pre = PreSerializedMessage::from_json(msg).unwrap();

        // Should have pre-serialized JSON
        assert!(pre.json_bytes.is_some());

        // Getting bytes should return same data
        let bytes1 = pre.get_json_bytes().unwrap();
        let bytes2 = pre.get_json_bytes().unwrap();
        assert_eq!(bytes1, bytes2);
    }
}
