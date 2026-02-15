//! Zero-copy serialization utilities using rkyv.
//!
//! This module provides a high-level API for efficient serialization and deserialization
//! using the rkyv library, optimized for performance-critical relay and signaling operations.
//!
//! # Examples
//!
//! ```rust,ignore
//! use signal_fish_server::rkyv_utils::{RkyvSerializer, zero_copy_access, deserialize};
//!
//! // Serialize data
//! let serializer = RkyvSerializer::new();
//! let bytes = serializer.serialize(&my_struct)?;
//!
//! // Zero-copy access (no deserialization needed)
//! let archived = zero_copy_access::<MyStruct>(&bytes)?;
//!
//! // Full deserialization when needed
//! let deserialized: MyStruct = deserialize::<MyStruct>(&bytes)?;
//! ```

use bytes::Bytes;
use rkyv::api::high::{HighDeserializer, HighSerializer, HighValidator};
use rkyv::rancor::Error as RancorError;
use rkyv::ser::allocator::ArenaHandle;
use rkyv::util::AlignedVec;
use rkyv::{Archive, Deserialize};
use std::fmt;

// Re-export commonly used rkyv types for convenience
pub use rkyv;
pub use rkyv::rancor::Error as RkyvRancorError;
pub use rkyv::Archived;
pub use rkyv::Deserialize as RkyvDeserialize;
pub use rkyv::Serialize as RkyvSerialize;

/// Error types for rkyv serialization and deserialization operations.
#[derive(Debug)]
pub enum RkyvError {
    /// Error during serialization
    Serialization(String),
    /// Error during deserialization
    Deserialization(String),
    /// Error during validation (alignment, checksum, etc.)
    Validation(String),
    /// Invalid buffer alignment
    InvalidAlignment { required: usize, actual: usize },
}

impl fmt::Display for RkyvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialization(msg) => write!(f, "Serialization error: {msg}"),
            Self::Deserialization(msg) => write!(f, "Deserialization error: {msg}"),
            Self::Validation(msg) => write!(f, "Validation error: {msg}"),
            Self::InvalidAlignment { required, actual } => {
                write!(
                    f,
                    "Invalid alignment: required {required} bytes, got {actual} bytes"
                )
            }
        }
    }
}

impl std::error::Error for RkyvError {}

/// Efficient serializer for rkyv data structures.
///
/// Provides a convenient wrapper around rkyv's high-level serialization API.
#[derive(Debug, Default)]
pub struct RkyvSerializer;

impl RkyvSerializer {
    /// Create a new serializer
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Serialize a value to bytes using rkyv.
    ///
    /// The returned `Bytes` can be used for zero-copy access via `zero_copy_access`
    /// or full deserialization via `deserialize`.
    ///
    /// # Errors
    ///
    /// Returns `RkyvError::Serialization` if serialization fails.
    pub fn serialize<T>(&self, value: &T) -> Result<Bytes, RkyvError>
    where
        T: for<'a> rkyv::Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RancorError>>,
    {
        let vec = rkyv::to_bytes::<RancorError>(value)
            .map_err(|e| RkyvError::Serialization(e.to_string()))?;

        Ok(Bytes::from(vec.into_vec()))
    }

    /// Serialize a value and return the underlying AlignedVec for maximum efficiency.
    ///
    /// This avoids the allocation from converting to `Bytes`, useful when the
    /// serialized data will be immediately consumed.
    ///
    /// # Errors
    ///
    /// Returns `RkyvError::Serialization` if serialization fails.
    pub fn serialize_aligned<T>(&self, value: &T) -> Result<AlignedVec, RkyvError>
    where
        T: for<'a> rkyv::Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RancorError>>,
    {
        rkyv::to_bytes::<RancorError>(value).map_err(|e| RkyvError::Serialization(e.to_string()))
    }
}

/// Zero-copy access to archived data without full deserialization.
///
/// This validates the buffer and returns a reference to the archived representation.
/// No heap allocations or copying occurs.
///
/// # Safety
///
/// This function validates the alignment and integrity of the archived data.
///
/// # Errors
///
/// Returns `RkyvError::Validation` if the buffer is invalid or improperly aligned.
///
/// # Example
///
/// ```rust,ignore
/// let archived = zero_copy_access::<MyStruct>(&bytes)?;
/// // Use archived.field_name to access fields without deserializing
/// ```
pub fn zero_copy_access<T>(bytes: &[u8]) -> Result<&T, RkyvError>
where
    T: rkyv::Portable + for<'a> rkyv::bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    rkyv::access::<T, RancorError>(bytes).map_err(|e| RkyvError::Validation(e.to_string()))
}

/// Deserialize archived data back to its original type.
///
/// This performs a full deserialization, allocating new memory for the result.
/// Use `zero_copy_access` instead if you can work with the archived representation.
///
/// # Errors
///
/// Returns `RkyvError` if deserialization or validation fails.
///
/// # Example
///
/// ```rust,ignore
/// let original: MyStruct = deserialize::<MyStruct>(&bytes)?;
/// ```
pub fn deserialize<T>(bytes: &[u8]) -> Result<T, RkyvError>
where
    T: Archive,
    T::Archived: for<'a> rkyv::bytecheck::CheckBytes<HighValidator<'a, RancorError>>
        + Deserialize<T, HighDeserializer<RancorError>>,
{
    rkyv::from_bytes::<T, RancorError>(bytes).map_err(|e| RkyvError::Deserialization(e.to_string()))
}

/// Check if a buffer has the correct alignment for type T.
///
/// # Example
///
/// ```rust,ignore
/// if !is_aligned::<ArchivedMyStruct>(&bytes) {
///     return Err(RkyvError::InvalidAlignment {
///         required: std::mem::align_of::<ArchivedMyStruct>(),
///         actual: bytes.as_ptr() as usize % std::mem::align_of::<ArchivedMyStruct>(),
///     });
/// }
/// ```
#[must_use]
pub fn is_aligned<T>(bytes: &[u8]) -> bool {
    let alignment = std::mem::align_of::<T>();
    (bytes.as_ptr() as usize).is_multiple_of(alignment)
}

/// Validate the alignment of a buffer for type T.
///
/// # Errors
///
/// Returns `RkyvError::InvalidAlignment` if the buffer is not properly aligned.
pub fn validate_alignment<T>(bytes: &[u8]) -> Result<(), RkyvError> {
    let alignment = std::mem::align_of::<T>();
    let actual = bytes.as_ptr() as usize % alignment;

    if actual != 0 {
        return Err(RkyvError::InvalidAlignment {
            required: alignment,
            actual,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rkyv::{Archive, Deserialize, Serialize};

    #[derive(Archive, Deserialize, Serialize, Debug, PartialEq)]
    #[rkyv(compare(PartialEq))]
    #[rkyv(derive(Debug))]
    struct TestStruct {
        id: u64,
        name: String,
        active: bool,
    }

    #[test]
    fn test_serialization_roundtrip() {
        let original = TestStruct {
            id: 42,
            name: "test".to_string(),
            active: true,
        };

        let serializer = RkyvSerializer::new();
        let bytes = serializer
            .serialize(&original)
            .expect("serialization failed");

        let deserialized: TestStruct = deserialize(&bytes).expect("deserialization failed");

        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_zero_copy_access() {
        let original = TestStruct {
            id: 42,
            name: "test".to_string(),
            active: true,
        };

        let serializer = RkyvSerializer::new();
        let bytes = serializer
            .serialize(&original)
            .expect("serialization failed");

        let archived = zero_copy_access::<ArchivedTestStruct>(&bytes).expect("access failed");

        assert_eq!(archived.id, 42);
        assert_eq!(archived.name.as_str(), "test");
        assert!(archived.active);
    }

    #[test]
    fn test_default_serializer() {
        let serializer = RkyvSerializer;
        let original = TestStruct {
            id: 1,
            name: "large".to_string(),
            active: false,
        };

        let bytes = serializer
            .serialize(&original)
            .expect("serialization failed");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_alignment_validation() {
        let original = TestStruct {
            id: 99,
            name: "align_test".to_string(),
            active: true,
        };

        let serializer = RkyvSerializer::new();
        let bytes = serializer
            .serialize(&original)
            .expect("serialization failed");

        // AlignedVec guarantees proper alignment
        assert!(is_aligned::<ArchivedTestStruct>(&bytes));
        assert!(validate_alignment::<ArchivedTestStruct>(&bytes).is_ok());
    }

    #[test]
    fn test_error_display() {
        let err = RkyvError::Serialization("test error".to_string());
        assert_eq!(err.to_string(), "Serialization error: test error");

        let err = RkyvError::InvalidAlignment {
            required: 8,
            actual: 4,
        };
        assert_eq!(
            err.to_string(),
            "Invalid alignment: required 8 bytes, got 4 bytes"
        );
    }

    #[test]
    fn test_serialize_aligned() {
        let original = TestStruct {
            id: 123,
            name: "aligned".to_string(),
            active: false,
        };

        let serializer = RkyvSerializer::new();
        let aligned = serializer
            .serialize_aligned(&original)
            .expect("serialization failed");

        // Convert to slice and deserialize
        let deserialized: TestStruct =
            deserialize(aligned.as_ref()).expect("deserialization failed");
        assert_eq!(original, deserialized);
    }
}
