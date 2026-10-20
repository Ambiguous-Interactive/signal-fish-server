use crate::data_channel::RTCDataChannelId;
use crate::data_channel::internal::RTCDataChannelInternal;
use crate::data_channel::state::RTCDataChannelState;
use crate::peer_connection::transport::dtls::role::RTCDtlsRole;
use crate::peer_connection::transport::sctp::SCTP_MAX_CHANNELS;
use sctp::StreamId;
use shared::error::{Error, Result};
use std::collections::HashMap;

/// Every data channel on a peer connection, indexed by both of its identifiers.
///
/// A channel has two: the [`RTCDataChannelId`] handle the application uses to address it, and
/// the SCTP [`StreamId`] it occupies on the wire. They are deliberately different things.
///
/// The handle is allocated when the channel is created and never changes. The stream id cannot
/// be, because RFC 8832 §6 makes its parity depend on the DTLS role — even for the client, odd
/// for the server — and that role is not known until a remote description has been applied. An
/// in-band channel therefore has no stream id between `create_data_channel` and the SCTP
/// connected procedure, which is exactly the window in which an application wants to hold a
/// reference to it. Keying the collection by the handle is what lets it do so.
///
/// The reverse index exists because the two directions arrive differently: the application and
/// every event name a channel by handle, while SCTP hands us a stream id.
#[derive(Default)]
pub(crate) struct DataChannelRegistry {
    channels: HashMap<RTCDataChannelId, RTCDataChannelInternal>,

    /// Wire stream id -> handle, for every channel that has been assigned one. Doubles as the
    /// authoritative "which stream ids are taken" set consulted when assigning new ones: it is
    /// fed by all three channel origins (locally deferred, out-of-band, and accepted from the
    /// peer), so a generated id cannot collide with one the peer opened first.
    by_stream: HashMap<StreamId, RTCDataChannelId>,

    /// Monotonic handle counter. Never decremented, never reused.
    next_handle: RTCDataChannelId,
}

impl DataChannelRegistry {
    pub(crate) fn new() -> Self {
        Self {
            channels: HashMap::new(),
            by_stream: HashMap::new(),
            next_handle: 0,
        }
    }

    /// Registers `data_channel`, assigning it a fresh handle and indexing its stream id if it
    /// already has one.
    ///
    /// Handles are issued monotonically and never reused, so a handle an application is still
    /// holding can never come to name a different channel after the original closed. (The
    /// stream id it occupied does become available again, as before.) `usize` is what makes
    /// this affordable: exhausting the handle space is not a practical concern, so unlike
    /// stream-id allocation this cannot fail.
    ///
    /// Issuing handles in increasing order also means ascending handle order *is* creation
    /// order, which [`Self::pending_stream_id_handles`] relies on.
    pub(crate) fn insert(&mut self, mut data_channel: RTCDataChannelInternal) -> RTCDataChannelId {
        let handle = self.next_handle;
        self.next_handle += 1;

        // The channel carries its own handle: `pending_stream_id_handles` and the close path
        // both read it back off the value rather than the key.
        data_channel.id = handle;
        if let Some(stream_id) = data_channel.stream_id {
            self.by_stream.insert(stream_id, handle);
        }
        self.channels.insert(handle, data_channel);

        handle
    }

    pub(crate) fn get(&self, handle: &RTCDataChannelId) -> Option<&RTCDataChannelInternal> {
        self.channels.get(handle)
    }

    pub(crate) fn get_mut(
        &mut self,
        handle: &RTCDataChannelId,
    ) -> Option<&mut RTCDataChannelInternal> {
        self.channels.get_mut(handle)
    }

    pub(crate) fn contains(&self, handle: &RTCDataChannelId) -> bool {
        self.channels.contains_key(handle)
    }

    pub(crate) fn remove(&mut self, handle: &RTCDataChannelId) -> Option<RTCDataChannelInternal> {
        let channel = self.channels.remove(handle)?;
        if let Some(stream_id) = channel.stream_id
            && self.by_stream.get(&stream_id) == Some(handle)
        {
            self.by_stream.remove(&stream_id);
        }
        Some(channel)
    }

    /// The handle of the channel occupying `stream_id`, if any.
    pub(crate) fn handle_of_stream(&self, stream_id: &StreamId) -> Option<RTCDataChannelId> {
        self.by_stream.get(stream_id).copied()
    }

    pub(crate) fn get_by_stream(&self, stream_id: &StreamId) -> Option<&RTCDataChannelInternal> {
        self.channels.get(self.by_stream.get(stream_id)?)
    }

    pub(crate) fn get_by_stream_mut(
        &mut self,
        stream_id: &StreamId,
    ) -> Option<&mut RTCDataChannelInternal> {
        let handle = *self.by_stream.get(stream_id)?;
        self.channels.get_mut(&handle)
    }

    /// Removes the channel occupying `stream_id`, freeing that id for reuse.
    pub(crate) fn remove_by_stream(
        &mut self,
        stream_id: &StreamId,
    ) -> Option<RTCDataChannelInternal> {
        let handle = self.by_stream.remove(stream_id)?;
        self.channels.remove(&handle)
    }

    /// Whether `stream_id` is already claimed, by a channel of any origin.
    pub(crate) fn stream_id_in_use(&self, stream_id: &StreamId) -> bool {
        self.by_stream.contains_key(stream_id)
    }

    /// Handles of channels still awaiting a stream id, in creation order.
    ///
    /// Ascending handle order is creation order, because handles are issued monotonically.
    /// Sorting matters: `channels` is a `HashMap`, so without it two runs of the same program
    /// would hand out different stream ids.
    pub(crate) fn pending_stream_id_handles(&self) -> Vec<RTCDataChannelId> {
        let mut handles: Vec<_> = self
            .channels
            .values()
            .filter(|dc| {
                dc.stream_id.is_none() && dc.ready_state == RTCDataChannelState::Connecting
            })
            .map(|dc| dc.id)
            .collect();
        handles.sort_unstable();
        handles
    }

    /// Assigns SCTP stream identifiers to every channel still awaiting one.
    ///
    /// This is the W3C "RTCSctpTransport connected procedure" step that RFC 8832 §6 governs:
    ///
    /// > The peer that initiates opening a data channel selects a stream identifier for which
    /// > the corresponding incoming and outgoing streams are unused. If the side is acting as
    /// > the DTLS client, it MUST choose an even stream identifier; if the side is acting as
    /// > the DTLS server, it MUST choose an odd one.
    ///
    /// `role` must already be resolved. An unresolved role is a caller bug, and returning an
    /// error rather than defaulting is the entire point: silently treating
    /// [`RTCDtlsRole::Auto`] as server parity — which is what the previous
    /// `generate_data_channel_id` did, at channel-creation time when the role could not yet be
    /// known — gave every channel an odd id regardless of role. The endpoint that later
    /// resolved to DTLS client was then holding ids it was not allowed to use, and which the
    /// peer acting as server was handing out at the same time
    /// (<https://github.com/webrtc-rs/rtc/issues/199>).
    ///
    /// `max_channels` is the association's negotiated stream limit, bounding the id space;
    /// `None` before the association reports one. Channels are served in creation order, so
    /// the assignment is deterministic across runs.
    ///
    /// Lives here, on the type that owns the id space, because both callers need it: the SCTP
    /// connected procedure in `DataChannelHandler`, which sees only the registry, and
    /// `RTCPeerConnection::create_data_channel` when an association already exists.
    pub(crate) fn assign_stream_ids(
        &mut self,
        role: RTCDtlsRole,
        max_channels: Option<StreamId>,
    ) -> Result<()> {
        // A cursor must also represent exhaustion after SID 65534.
        let mut next: u32 = match role {
            RTCDtlsRole::Client => 0,
            RTCDtlsRole::Server => 1,
            // Never guess a parity.
            _ => return Err(Error::ErrDataChannelStreamIdNotAssigned),
        };

        // The negotiated stream count bounds stream ids, but only once it is known: a channel
        // may be created before the association exists, and until it connects there is nothing
        // to bound against. This matches W3C §6.1.1.3, which applies the limit at the connected
        // procedure rather than at creation.
        let max = u32::from(max_channels.unwrap_or(SCTP_MAX_CHANNELS));

        for handle in self.pending_stream_id_handles() {
            // Skip ids already claimed — by an out-of-band channel the application pinned, or
            // by one the peer opened first. Carrying the cursor across iterations rather than
            // restarting the scan keeps this a single pass.
            // `max` is a count: every SID strictly below it is valid.
            // The bound proves that each cast below fits the wire SID type.
            while next < max && self.stream_id_in_use(&(next as StreamId)) {
                next += 2;
            }
            if next >= max {
                return Err(Error::ErrMaxDataChannelID);
            }
            self.assign_stream_id(handle, next as StreamId);
            next += 2;
        }

        Ok(())
    }

    /// Binds `handle` to `stream_id`. No-op if the handle is unknown.
    pub(crate) fn assign_stream_id(&mut self, handle: RTCDataChannelId, stream_id: StreamId) {
        if let Some(data_channel) = self.channels.get_mut(&handle) {
            data_channel.stream_id = Some(stream_id);
            self.by_stream.insert(stream_id, handle);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.channels.len()
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &RTCDataChannelInternal> {
        self.channels.values()
    }

    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut RTCDataChannelInternal> {
        self.channels.values_mut()
    }

    /// Removes every channel, in handle order, freeing all stream ids.
    pub(crate) fn drain(&mut self) -> Vec<RTCDataChannelInternal> {
        self.by_stream.clear();
        let mut channels: Vec<_> = self.channels.drain().map(|(_, channel)| channel).collect();
        channels.sort_by_key(|channel| channel.id);
        channels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connecting_channel() -> RTCDataChannelInternal {
        RTCDataChannelInternal {
            ready_state: RTCDataChannelState::Connecting,
            ..Default::default()
        }
    }

    #[test]
    fn failed_creation_removal_releases_only_its_own_stream() {
        let mut registry = DataChannelRegistry::new();
        let pending = registry.insert(connecting_channel());
        assert_eq!(registry.remove(&pending).unwrap().id, pending);
        assert!(registry.remove(&pending).is_none());
        let original = registry.insert(RTCDataChannelInternal {
            stream_id: Some(2),
            ..Default::default()
        });
        let current = registry.insert(RTCDataChannelInternal {
            stream_id: Some(2),
            ..Default::default()
        });
        registry.remove(&original).unwrap();
        assert_eq!(registry.handle_of_stream(&2), Some(current));
        registry.remove(&current).unwrap();
        assert!(!registry.stream_id_in_use(&2));
        assert!(registry.is_empty());
    }

    // Regression #1079: the negotiated count includes SID max - 1.
    #[test]
    fn negotiated_stream_count_admits_every_valid_role_stream() {
        for max in 0..=6 {
            for (role, parity) in [(RTCDtlsRole::Client, 0), (RTCDtlsRole::Server, 1)] {
                let mut registry = DataChannelRegistry::new();
                for expected in (parity..max).step_by(2) {
                    let handle = registry.insert(connecting_channel());
                    assert!(
                        registry.assign_stream_ids(role, Some(max)).is_ok(),
                        "role={role:?}, count={max}, expected SID={expected}"
                    );
                    assert_eq!(registry.get(&handle).unwrap().stream_id, Some(expected));
                }
                let extra = registry.insert(connecting_channel());
                for _ in 0..2 {
                    assert!(matches!(
                        registry.assign_stream_ids(role, Some(max)),
                        Err(Error::ErrMaxDataChannelID)
                    ));
                    assert_eq!(registry.get(&extra).unwrap().stream_id, None);
                }
            }
        }
    }

    #[test]
    fn terminal_pending_channels_do_not_consume_stream_capacity() {
        for state in [RTCDataChannelState::Closing, RTCDataChannelState::Closed] {
            let mut registry = DataChannelRegistry::new();
            let terminal = registry.insert(RTCDataChannelInternal {
                ready_state: state,
                ..Default::default()
            });
            let live = registry.insert(connecting_channel());
            registry
                .assign_stream_ids(RTCDtlsRole::Client, Some(1))
                .unwrap();
            assert_eq!(registry.get(&terminal).unwrap().stream_id, None);
            assert_eq!(registry.get(&live).unwrap().stream_id, Some(0));
        }
    }

    #[test]
    fn full_stream_range_exhausts_without_overflow_and_reuses_final_sid() {
        for (role, max_channels) in [
            (RTCDtlsRole::Client, Some(u16::MAX)),
            (RTCDtlsRole::Server, Some(u16::MAX)),
            (RTCDtlsRole::Client, None),
            (RTCDtlsRole::Server, None),
        ] {
            let mut registry = DataChannelRegistry::new();
            let parity = if role == RTCDtlsRole::Client { 0 } else { 1 };
            let mut final_handle = 0;
            let mut final_sid = 0;
            for sid in (parity..u16::MAX).step_by(2) {
                final_handle = registry.insert(connecting_channel());
                final_sid = sid;
            }
            registry.assign_stream_ids(role, max_channels).unwrap();
            assert_eq!(
                registry.get(&final_handle).unwrap().stream_id,
                Some(final_sid)
            );
            let pending = registry.insert(connecting_channel());
            assert!(matches!(
                registry.assign_stream_ids(role, max_channels),
                Err(Error::ErrMaxDataChannelID)
            ));
            assert_eq!(
                registry.remove_by_stream(&final_sid).unwrap().id,
                final_handle
            );
            registry.assign_stream_ids(role, max_channels).unwrap();
            assert_eq!(registry.get(&pending).unwrap().stream_id, Some(final_sid));
            assert!(
                pending > final_handle,
                "a reused stream needs a fresh handle"
            );
        }
    }
}
