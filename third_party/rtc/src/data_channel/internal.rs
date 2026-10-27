use crate::data_channel::RTCDataChannelId;
use crate::data_channel::parameters::DataChannelParameters;
use crate::data_channel::state::RTCDataChannelState;
use datachannel::data_channel::DataChannelConfig;
use sansio::Protocol;
use sctp::{PayloadProtocolIdentifier, ReliabilityType, StreamId};
use shared::error::{Error, Result};
use std::time::Instant;

#[derive(Clone)]
pub(crate) struct RTCDataChannelInternal {
    /// Connection-local handle. Assigned by `DataChannelRegistry::insert`, stable for the
    /// channel's lifetime, and the key this channel lives under. NOT the wire value — the
    /// differing widths (`usize` vs `StreamId`) are what stop the two being confused.
    ///
    /// Carried on the value as well as the key so the close and assignment paths can read it
    /// back without a reverse lookup.
    pub(crate) id: RTCDataChannelId,

    /// The RFC 8832 §6 SCTP stream identifier.
    ///
    /// `None` until the SCTP connected procedure assigns it, which cannot happen before the
    /// DTLS role is resolved because the role decides the parity: even for the client, odd for
    /// the server. Out-of-band (`negotiated`) channels and channels accepted from the peer
    /// have it from birth, since their id is already fixed by the application or the wire.
    pub(crate) stream_id: Option<StreamId>,
    pub(crate) label: String,
    pub(crate) ordered: bool,
    pub(crate) max_packet_life_time: Option<u16>,
    pub(crate) max_retransmits: Option<u16>,
    pub(crate) protocol: String,
    pub(crate) negotiated: bool,
    pub(crate) ready_state: RTCDataChannelState,
    pub(crate) buffered_amount_high_threshold: u32,
    pub(crate) buffered_amount_low_threshold: u32,
    /// Send-buffer bytes (at least one per message) that SCTP has not yet
    /// released (acknowledged or abandoned). Incremented synchronously at the app
    /// send boundary and decremented on SCTP buffer-release events, so it accounts
    /// for bytes still in the app→core→SCTP send pipeline — not just the SCTP
    /// stream's own `buffered_amount`, which counts only post-packetization. Used
    /// for synchronous send back-pressure.
    pub(crate) outstanding_bytes: usize,

    /// Deadline by which an in-band channel's DCEP handshake must complete.
    /// Set when the channel is dialed; cleared on handshake completion or close.
    pub(crate) handshake_deadline: Option<Instant>,

    /// Set when the DCEP handshake times out and `OnClose` has already been
    /// emitted. Prevents `SCTPStreamClosed` from emitting/counting a second close.
    pub(crate) close_emitted: bool,

    pub(crate) data_channel: Option<::datachannel::data_channel::DataChannel>,
}

impl Default for RTCDataChannelInternal {
    fn default() -> Self {
        Self {
            id: 0,
            stream_id: None,
            label: "".to_string(),
            ordered: false,
            max_packet_life_time: None,
            max_retransmits: None,
            protocol: "".to_string(),
            negotiated: false,
            ready_state: RTCDataChannelState::default(),
            buffered_amount_high_threshold: u32::MAX,
            buffered_amount_low_threshold: 0,
            outstanding_bytes: 0,
            handshake_deadline: None,
            close_emitted: false,
            data_channel: None,
        }
    }
}

impl RTCDataChannelInternal {
    /// Creates the DataChannel object before the networking is set up.
    ///
    /// The handle is left at its placeholder; [`DataChannelRegistry::insert`] assigns the real
    /// one. The stream id comes from `params.negotiated` — `Some` for an out-of-band channel,
    /// whose id the application already fixed, and `None` for an in-band one, whose id has to
    /// wait for the DTLS role.
    ///
    /// [`DataChannelRegistry::insert`]: crate::data_channel::registry::DataChannelRegistry::insert
    pub(crate) fn new(params: DataChannelParameters) -> Self {
        Self {
            id: 0,
            stream_id: params.negotiated,
            label: params.label,
            protocol: params.protocol,
            negotiated: params.negotiated.is_some(),
            ordered: params.ordered,
            max_packet_life_time: params.max_packet_life_time,
            max_retransmits: params.max_retransmits,
            ready_state: RTCDataChannelState::Connecting,
            buffered_amount_high_threshold: u32::MAX,
            buffered_amount_low_threshold: 0,
            outstanding_bytes: 0,
            handshake_deadline: None,
            close_emitted: false,
            data_channel: None,
        }
    }

    pub(crate) fn dial(&mut self, association_handle: usize) -> Result<()> {
        // A channel cannot be dialed before its stream id exists, and the stream id cannot
        // exist before the DTLS role does. Making that an error rather than a fallback is the
        // whole point: guessing a parity here is what issue #199 was.
        let stream_id = self
            .stream_id
            .ok_or(Error::ErrDataChannelStreamIdNotAssigned)?;

        let (channel_type, reliability_parameter) =
            ::datachannel::data_channel::DataChannel::get_channel_type_and_reliability_parameter(
                self.ordered,
                self.max_retransmits,
                self.max_packet_life_time,
            );

        let config = ::datachannel::data_channel::DataChannelConfig {
            channel_type,
            priority: ::datachannel::message::message_channel_open::CHANNEL_PRIORITY_NORMAL,
            reliability_parameter,
            label: self.label.clone(),
            protocol: self.protocol.clone(),
            negotiated: self.negotiated,
        };

        let mut data_channel =
            ::datachannel::data_channel::DataChannel::dial(config, association_handle, stream_id)?;
        data_channel.set_buffered_amount_low_threshold(self.buffered_amount_low_threshold)?;
        data_channel.set_buffered_amount_high_threshold(self.buffered_amount_high_threshold)?;

        self.data_channel = Some(data_channel);

        // An in-band channel stays `Connecting` until the peer's `DATA_CHANNEL_ACK`
        // is processed in `DataChannelHandler::handle_read`. An out-of-band
        // `negotiated` channel has no DCEP handshake, so it is open immediately.
        self.ready_state = if self.negotiated {
            RTCDataChannelState::Open
        } else {
            RTCDataChannelState::Connecting
        };

        Ok(())
    }

    pub(crate) fn accept(
        association_handle: usize,
        stream_id: StreamId,
        ppi: PayloadProtocolIdentifier,
        buf: &[u8],
    ) -> Result<Self> {
        let data_channel = ::datachannel::data_channel::DataChannel::accept(
            DataChannelConfig::default(),
            association_handle,
            stream_id,
            ppi,
            buf,
        )?;

        let data_channel_config = data_channel.config();

        let (unordered, reliability_type) =
            ::datachannel::data_channel::DataChannel::get_reliability_params(
                data_channel_config.channel_type,
            );

        // Keep the DCEP delivery mode in the public channel properties. Reliable
        // channels ignore the reliability parameter (RFC 8832 section 5.1).
        // Partial modes must fit the WebRTC API's u16 properties; truncation
        // could report a nonzero retransmit limit as zero.
        let checked_parameter = || {
            u16::try_from(data_channel_config.reliability_parameter).map_err(|_| {
                Error::Other("DCEP reliability parameter exceeds the WebRTC u16 range".into())
            })
        };
        let (max_packet_life_time, max_retransmits) = match reliability_type {
            ReliabilityType::Reliable => (None, None),
            ReliabilityType::Rexmit => (None, Some(checked_parameter()?)),
            ReliabilityType::Timed => (Some(checked_parameter()?), None),
            _ => return Err(Error::Other("Unsupported DCEP reliability mode".into())),
        };

        // `negotiated: None` — the peer opened this channel in-band over DCEP, so it is not an
        // out-of-band channel however well-known its stream id now is. The stream id is set
        // below instead of through `negotiated`, which would also flip the `negotiated` flag.
        let mut data_channel_internal = RTCDataChannelInternal::new(DataChannelParameters {
            label: data_channel_config.label.clone(),
            protocol: data_channel_config.protocol.clone(),
            ordered: !unordered,
            max_packet_life_time,
            max_retransmits,
            negotiated: None,
        });
        // Known from the wire: no deferral needed, and it must be registered so locally
        // generated ids cannot collide with it.
        data_channel_internal.stream_id = Some(stream_id);
        data_channel_internal.data_channel = Some(data_channel);
        data_channel_internal.ready_state = RTCDataChannelState::Open;

        Ok(data_channel_internal)
    }

    pub(crate) fn close(&mut self) -> Result<()> {
        if let Some(data_channel) = self.data_channel.as_mut() {
            data_channel.close()?;
            self.ready_state = RTCDataChannelState::Closing;
        } else {
            self.ready_state = RTCDataChannelState::Closed;
        }
        self.handshake_deadline = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_connection::RTCPeerConnectionBuilder;
    use datachannel::message::Message;
    use datachannel::message::message_channel_open::{ChannelType, DataChannelOpen};
    use shared::marshal::Marshal;

    fn install_connected_stream(
        pc: &mut crate::peer_connection::RTCPeerConnection,
        id: crate::data_channel::RTCDataChannelId,
        ordered: bool,
    ) -> Result<()> {
        let mut association = crate::peer_connection::handler::sctp::established_test_association(
            crate::peer_connection::transport::dtls::role::RTCDtlsRole::Client,
        );
        association
            .open_stream(0, PayloadProtocolIdentifier::String)?
            .set_reliability_params(
                !ordered,
                if ordered {
                    sctp::ReliabilityType::Reliable
                } else {
                    sctp::ReliabilityType::Rexmit
                },
                0,
            )?;
        pc.pipeline_context
            .sctp_handler_context
            .sctp_transport
            .sctp_associations
            .insert(sctp::AssociationHandle(0), association);
        pc.pipeline_context
            .sctp_handler_context
            .sctp_transport
            .internal_buffer
            .resize(65_536, 0);
        // The injected ACK below represents an OPEN already sent by this fixture.
        let channel = pc
            .data_channels
            .get_mut(&id)
            .unwrap()
            .data_channel
            .as_mut()
            .unwrap();
        while channel.poll_write().is_some() {}
        Ok(())
    }

    fn acknowledge_channel(
        pc: &mut crate::peer_connection::RTCPeerConnection,
        now: Instant,
    ) -> Result<()> {
        use crate::peer_connection::message::internal::{
            DTLSMessage, RTCMessageInternal, TaggedRTCMessageInternal,
        };
        use datachannel::message::message_channel_ack::DataChannelAck;
        let ack = Message::DataChannelAck(DataChannelAck {}).marshal()?;
        pc.get_datachannel_handler()
            .handle_read(TaggedRTCMessageInternal {
                now,
                transport: Default::default(),
                message: RTCMessageInternal::Dtls(DTLSMessage::Sctp(
                    datachannel::data_channel::DataChannelMessage {
                        association_handle: 0,
                        stream_id: 0,
                        ppi: PayloadProtocolIdentifier::Dcep,
                        payload: bytes::BytesMut::from(&ack[..]),
                        negotiated: false,
                    },
                )),
            })
    }

    // Regression #1260: filler credit must not release a nonempty successor.
    #[test]
    fn empty_message_credit_preserves_unreleased_successor_bytes() -> Result<()> {
        use crate::peer_connection::event::{RTCEventInternal, TaggedRTCEventInternal};
        for ordered in [true, false] {
            for binary in [true, false] {
                let now = Instant::now();
                let mut pc = RTCPeerConnectionBuilder::new().build(now)?;
                let mut internal = RTCDataChannelInternal::new(DataChannelParameters {
                    ordered,
                    max_retransmits: (!ordered).then_some(0),
                    negotiated: Some(0),
                    ..Default::default()
                });
                internal.dial(0)?;
                let id = pc.data_channels.insert(internal);
                install_connected_stream(&mut pc, id, ordered)?;
                if binary {
                    pc.data_channel(id).unwrap().send(now, Default::default())?;
                } else {
                    pc.data_channel(id).unwrap().send_text(now, "")?;
                }
                assert_eq!(pc.data_channel(id).unwrap().outstanding_bytes(), 1);
                pc.data_channel(id).unwrap().send_text(now, "successor")?;
                assert_eq!(pc.data_channel(id).unwrap().outstanding_bytes(), 10);
                for (released, remaining) in [(1, 9), (4, 5), (5, 0)] {
                    pc.get_datachannel_handler()
                        .handle_event(TaggedRTCEventInternal {
                            now,
                            event: RTCEventInternal::SCTPBufferReleased(0, 0, released),
                        })?;
                    assert_eq!(pc.data_channel(id).unwrap().outstanding_bytes(), remaining);
                }
                pc.data_channel(id).unwrap().close()?;
                let result = pc.data_channel(id).unwrap().send_text(now, "");
                assert_eq!(result, Err(Error::ErrDataChannelClosed));
                assert_eq!(pc.data_channel(id).unwrap().outstanding_bytes(), 0);
            }
        }
        Ok(())
    }

    // Regression #1091: allocated streams still wait for DCEP before gameplay.
    #[test]
    fn post_connected_channel_rejects_gameplay_until_dcep_ack() -> Result<()> {
        for ordered in [true, false] {
            for text in [true, false] {
                let now = Instant::now();
                let mut pc = RTCPeerConnectionBuilder::new().build(now)?;
                let mut internal = RTCDataChannelInternal::new(DataChannelParameters {
                    ordered,
                    max_retransmits: (!ordered).then_some(0),
                    ..Default::default()
                });
                internal.stream_id = Some(0);
                internal.dial(0)?;
                assert_eq!(internal.ready_state, RTCDataChannelState::Connecting);
                assert!(internal.data_channel.is_some());
                let id = pc.data_channels.insert(internal);
                install_connected_stream(&mut pc, id, ordered)?;
                let mut channel = pc.data_channel(id).expect("registered connecting channel");
                let result = if text {
                    channel.send_text(now, "before-ack")
                } else {
                    channel.send(now, bytes::BytesMut::from("before-ack"))
                };
                assert!(
                    matches!(result, Err(Error::ErrDataChannelNotOpen)),
                    "Connecting stream accepted gameplay: ordered={ordered}, text={text}, result={result:?}"
                );
                assert_eq!(channel.outstanding_bytes(), 0);
                assert!(
                    pc.get_endpoint_handler().poll_write().is_none(),
                    "rejected send must not enqueue gameplay"
                );
                acknowledge_channel(&mut pc, now)?;
                let mut channel = pc.data_channel(id).expect("acknowledged channel");
                assert_eq!(channel.ready_state(), RTCDataChannelState::Open);
                if text {
                    channel.send_text(now, "after-ack")?;
                } else {
                    channel.send(now, bytes::BytesMut::from("after-ack"))?;
                }
                assert_eq!(channel.outstanding_bytes(), 9);
                channel.close()?;
                assert!(matches!(
                    channel.send_text(now, "closed"),
                    Err(Error::ErrDataChannelClosed)
                ));
            }
        }
        Ok(())
    }

    // Regression #1121: late SCTP refusal cannot leave sent credit or a queued body.
    #[test]
    fn stream_reset_admission_failures_leave_no_payload_or_sent_credit() -> Result<()> {
        use crate::data_channel::RTCDataChannelMessage;
        use crate::peer_connection::message::{
            RTCMessage, TaggedRTCMessage,
            internal::{DTLSMessage, RTCMessageInternal},
        };
        for ordered in [true, false] {
            for binary in [false, true] {
                for oversized in [false, true] {
                    let now = Instant::now();
                    let mut pc = RTCPeerConnectionBuilder::new().build(now)?;
                    let mut dc = RTCDataChannelInternal::new(DataChannelParameters {
                        ordered,
                        max_retransmits: (!ordered).then_some(0),
                        negotiated: Some(0),
                        ..Default::default()
                    });
                    dc.dial(0)?;
                    let id = pc.data_channels.insert(dc);
                    install_connected_stream(&mut pc, id, ordered)?;
                    let result = if oversized {
                        /*
                           The low-level Protocol API does not use RTCDataChannel's
                           public size preflight. SCTP must still refuse it honestly.
                        */
                        pc.handle_write(TaggedRTCMessage {
                            now,
                            message: RTCMessage::DataChannelMessage(
                                id,
                                RTCDataChannelMessage {
                                    is_string: !binary,
                                    data: bytes::BytesMut::from(&vec![b'x'; 65_537][..]),
                                },
                            ),
                        })
                    } else {
                        pc.pipeline_context
                            .sctp_handler_context
                            .sctp_transport
                            .sctp_associations
                            .get_mut(&sctp::AssociationHandle(0))
                            .unwrap()
                            .shutdown()?;
                        if binary {
                            pc.data_channel(id)
                                .unwrap()
                                .send(now, bytes::BytesMut::from("rejected"))
                        } else {
                            pc.data_channel(id).unwrap().send_text(now, "rejected")
                        }
                    };
                    assert_eq!(
                        result,
                        Err(if oversized {
                            Error::ErrOutboundPacketTooLarge
                        } else {
                            Error::ErrDataChannelClosed
                        })
                    );
                    assert_eq!(pc.data_channel(id).unwrap().outstanding_bytes(), 0);
                    assert!(
                        pc.pipeline_context
                            .stats
                            .data_channels
                            .get(&id)
                            .is_none_or(|stats| stats.messages_sent == 0 && stats.bytes_sent == 0)
                    );
                    assert!(
                        pc.pipeline_context
                            .datachannel_handler_context
                            .write_outs
                            .is_empty()
                    );
                    assert!(
                        pc.data_channels
                            .get_mut(&id)
                            .unwrap()
                            .data_channel
                            .as_mut()
                            .unwrap()
                            .poll_write()
                            .is_none()
                    );
                    while let Some(message) = pc.get_sctp_handler().poll_write() {
                        if let RTCMessageInternal::Dtls(DTLSMessage::Raw(raw)) = message.message {
                            let mut offset = 12;
                            while offset + 4 <= raw.len() {
                                assert_ne!(
                                    raw[offset], 0,
                                    "rejected payload must never reach the wire"
                                );
                                let length = usize::from(u16::from_be_bytes([
                                    raw[offset + 2],
                                    raw[offset + 3],
                                ]));
                                offset += length.next_multiple_of(4);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    // Regression #1075: remote reset cannot overtake input accepted on the same stream.
    #[test]
    fn remote_stream_reset_preserves_queued_input_order() -> Result<()> {
        use crate::peer_connection::event::{RTCDataChannelEvent, RTCPeerConnectionEvent};
        use crate::peer_connection::message::{
            RTCMessage,
            internal::{
                DTLSMessage, RTCMessageInternal, SctpStreamEvent, TaggedRTCMessageInternal,
            },
        };
        for ordered in [true, false] {
            let now = Instant::now();
            let mut pc = RTCPeerConnectionBuilder::new().build(now)?;
            let mut dc = RTCDataChannelInternal::new(DataChannelParameters {
                ordered,
                max_retransmits: (!ordered).then_some(0),
                negotiated: Some(0),
                ..Default::default()
            });
            dc.dial(0)?;
            dc.ready_state = RTCDataChannelState::Open;
            let id = pc.data_channels.insert(dc);
            for message in [
                RTCMessageInternal::Dtls(DTLSMessage::Sctp(
                    datachannel::data_channel::DataChannelMessage {
                        association_handle: 0,
                        stream_id: 0,
                        ppi: PayloadProtocolIdentifier::String,
                        payload: bytes::BytesMut::from("last-gameplay-receipt"),
                        negotiated: true,
                    },
                )),
                RTCMessageInternal::Dtls(DTLSMessage::SctpStreamEvent(SctpStreamEvent::Closed(
                    0, 0,
                ))),
            ] {
                pc.get_datachannel_handler()
                    .handle_read(TaggedRTCMessageInternal {
                        now,
                        transport: Default::default(),
                        message,
                    })?;
            }
            pc.handle_read(shared::TaggedBytesMut {
                now,
                transport: Default::default(),
                message: Default::default(),
            })?;
            // Match the wrapper: lifecycle output is polled before input.
            while let Some(event) = pc.poll_event() {
                assert!(
                    !matches!(event, RTCPeerConnectionEvent::OnDataChannel(RTCDataChannelEvent::OnClose(event_id)) if event_id == id),
                    "remote reset overtook earlier input: ordered={ordered}"
                );
            }
            let first = pc
                .poll_data_read()
                .expect("earlier input remains available");
            assert!(
                matches!(first.message, RTCMessage::DataChannelMessage(event_id, ref message)
                if event_id == id && message.data == b"last-gameplay-receipt"[..])
            );
            assert!(
                matches!(pc.poll_data_read().expect("remote reset follows input").message,
                RTCMessage::DataChannelClosed(event_id) if event_id == id)
            );
            assert!(pc.poll_data_read().is_none());
        }
        Ok(())
    }

    // Regressions #1076/#1121: accepted writes enter SCTP before local reset.
    #[test]
    fn stream_reset_accepted_send_precedes_local_close_in_sctp_write_order() -> Result<()> {
        use crate::peer_connection::message::internal::{DTLSMessage, RTCMessageInternal};
        for (ordered, negotiated) in [(true, true), (false, true), (true, false), (false, false)] {
            let now = Instant::now();
            let mut pc = RTCPeerConnectionBuilder::new().build(now)?;
            let mut dc = RTCDataChannelInternal::new(DataChannelParameters {
                ordered,
                max_retransmits: (!ordered).then_some(0),
                negotiated: negotiated.then_some(0),
                ..Default::default()
            });
            dc.stream_id = Some(0);
            dc.dial(0)?;
            if negotiated {
                while dc.data_channel.as_mut().unwrap().poll_write().is_some() {}
            }
            let id = pc.data_channels.insert(dc);
            install_connected_stream(&mut pc, id, ordered)?;
            if !negotiated {
                acknowledge_channel(&mut pc, now)?;
            }
            pc.data_channel(id)
                .unwrap()
                .send_text(now, "final-before-reset")?;
            assert_eq!(pc.data_channel(id).unwrap().outstanding_bytes(), 18);
            pc.data_channel(id).unwrap().close()?;
            pc.data_channel(id).unwrap().close()?;
            assert_eq!(
                pc.data_channel(id).unwrap().ready_state(),
                RTCDataChannelState::Closing
            );
            assert!(
                pc.data_channel(id)
                    .unwrap()
                    .send_text(now, "too-late")
                    .is_err()
            );
            /*
               Both application writes and local close now enter SCTP before
               the transport flush. Inspect the real wire, not a deferred queue.
            */
            while let Some(message) = pc.get_datachannel_handler().poll_write() {
                pc.get_sctp_handler().handle_write(message)?;
            }
            let mut kinds = Vec::new();
            let mut bodies = Vec::new();
            while let Some(message) = pc.get_sctp_handler().poll_write() {
                if let RTCMessageInternal::Dtls(DTLSMessage::Raw(raw)) = message.message {
                    let mut offset = 12;
                    while offset + 4 <= raw.len() {
                        let length =
                            usize::from(u16::from_be_bytes([raw[offset + 2], raw[offset + 3]]));
                        kinds.push(raw[offset]);
                        if raw[offset] == 0 {
                            bodies.push(bytes::Bytes::copy_from_slice(
                                &raw[offset + 16..offset + length],
                            ));
                        }
                        offset += length.next_multiple_of(4);
                    }
                }
            }
            assert_eq!(
                kinds,
                [0, 130],
                "ordered={ordered}, negotiated={negotiated}: one DATA before one reset"
            );
            assert_eq!(bodies, [bytes::Bytes::from_static(b"final-before-reset")]);
        }
        Ok(())
    }

    #[test]
    fn stream_reset_local_close_stays_closing_and_repeated_close_queues_one_reset() -> Result<()> {
        let mut pc = RTCPeerConnectionBuilder::new().build(Instant::now())?;
        let mut dc = RTCDataChannelInternal::new(DataChannelParameters {
            negotiated: Some(0),
            ..Default::default()
        });
        dc.dial(0)?;
        while dc.data_channel.as_mut().unwrap().poll_write().is_some() {}
        let id = pc.data_channels.insert(dc);
        pc.pipeline_context.data_read_outs.push_back(
            crate::peer_connection::message::TaggedRTCMessage {
                now: Instant::now(),
                message: crate::peer_connection::message::RTCMessage::DataChannelMessage(
                    id,
                    crate::data_channel::RTCDataChannelMessage {
                        is_string: true,
                        data: bytes::BytesMut::from("before-close"),
                    },
                ),
            },
        );
        pc.data_channel(id).unwrap().close()?;
        assert_eq!(
            pc.data_channel(id).unwrap().ready_state(),
            RTCDataChannelState::Closing
        );
        pc.data_channel(id).unwrap().close()?;
        while let Some(msg) = pc.get_endpoint_handler().poll_write() {
            pc.get_datachannel_handler().handle_write(msg)?;
        }
        // The handler drains preceding DATA, then its reset on poll_write.
        let mut writes = Vec::new();
        while let Some(msg) = pc.get_datachannel_handler().poll_write() {
            writes.push(msg);
        }
        assert_eq!(writes.len(), 1, "repeated close requests one reset");
        let dc = pc
            .data_channels
            .get_mut(&id)
            .unwrap()
            .data_channel
            .as_mut()
            .unwrap();
        assert!(
            dc.poll_write().is_none(),
            "reset already drained by handler"
        );
        assert!(
            dc.poll_write().is_none(),
            "repeated close requests no second reset"
        );
        pc.get_datachannel_handler().handle_event(
            crate::peer_connection::event::TaggedRTCEventInternal {
                now: Instant::now(),
                event: crate::peer_connection::event::RTCEventInternal::SCTPStreamClosed(0, 0),
            },
        )?;
        while let Some(event) = pc.poll_event() {
            assert!(
                !matches!(event,
                    crate::peer_connection::event::RTCPeerConnectionEvent::OnDataChannel(
                        crate::peer_connection::event::RTCDataChannelEvent::OnClose(event_id)
                    ) if event_id == id
                ),
                "immediate reset must not overtake queued closing and input"
            );
        }
        assert!(matches!(
            pc.poll_read().unwrap().message,
            crate::peer_connection::message::RTCMessage::DataChannelMessage(event_id, _)
                if event_id == id
        ));
        assert!(matches!(
            pc.poll_data_read().unwrap().message,
            crate::peer_connection::message::RTCMessage::DataChannelClosing(event_id)
                if event_id == id
        ));
        assert!(matches!(
            pc.poll_data_read().unwrap().message,
            crate::peer_connection::message::RTCMessage::DataChannelClosed(event_id)
                if event_id == id
        ));
        assert!(
            pc.poll_data_read().is_none(),
            "repeated close publishes no duplicate"
        );
        Ok(())
    }

    #[test]
    fn unconnected_local_close_reports_both_terminal_markers_once() -> Result<()> {
        for negotiated in [None, Some(0)] {
            let mut pc = RTCPeerConnectionBuilder::new().build(Instant::now())?;
            let id = pc
                .create_data_channel(
                    "unconnected",
                    Some(crate::data_channel::RTCDataChannelInit {
                        negotiated,
                        ..Default::default()
                    }),
                )?
                .id();
            assert!(pc.data_channels.get(&id).unwrap().data_channel.is_none());
            pc.data_channel(id).unwrap().close()?;
            pc.data_channel(id).unwrap().close()?;
            assert_eq!(
                pc.data_channel(id).unwrap().ready_state(),
                RTCDataChannelState::Closed
            );
            assert!(matches!(pc.poll_data_read().unwrap().message,
                crate::peer_connection::message::RTCMessage::DataChannelClosing(event_id) if event_id == id));
            assert!(
                matches!(pc.poll_data_read().expect("no SCTP reset can provide the final close").message,
                crate::peer_connection::message::RTCMessage::DataChannelClosed(event_id) if event_id == id)
            );
            assert!(pc.poll_data_read().is_none());
            assert_eq!(
                pc.pipeline_context
                    .stats
                    .peer_connection
                    .data_channels_opened,
                0
            );
            assert_eq!(
                pc.pipeline_context
                    .stats
                    .peer_connection
                    .data_channels_closed,
                1
            );
            assert_eq!(
                pc.pipeline_context.stats.data_channels[&id].state,
                RTCDataChannelState::Closed
            );
            pc.get_datachannel_handler().handle_event(
                crate::peer_connection::event::TaggedRTCEventInternal {
                    now: Instant::now(),
                    event: crate::peer_connection::event::RTCEventInternal::DTLSClosed,
                },
            )?;
            assert!(
                pc.poll_read().is_none(),
                "transport cleanup adds no duplicate close"
            );
            while let Some(event) = pc.poll_event() {
                assert!(!matches!(event,
                    crate::peer_connection::event::RTCPeerConnectionEvent::OnDataChannel(
                        crate::peer_connection::event::RTCDataChannelEvent::OnClose(event_id)
                    ) if event_id == id));
            }
            assert!(pc.data_channels.get(&id).is_none());
            assert_eq!(
                pc.pipeline_context
                    .stats
                    .peer_connection
                    .data_channels_closed,
                1
            );
        }
        Ok(())
    }

    #[test]
    fn remote_dcep_delivery_modes_preserve_properties_and_reciprocal_config() -> Result<()> {
        let cases = [
            (ChannelType::Reliable, true, false, false),
            (ChannelType::ReliableUnordered, false, false, false),
            (ChannelType::PartialReliableRexmit, true, true, false),
            (
                ChannelType::PartialReliableRexmitUnordered,
                false,
                true,
                false,
            ),
            (ChannelType::PartialReliableTimed, true, false, true),
            (
                ChannelType::PartialReliableTimedUnordered,
                false,
                false,
                true,
            ),
        ];
        let mut pc = RTCPeerConnectionBuilder::new().build(Instant::now())?;
        let mut stream_id = 0;
        for (channel_type, ordered, retransmit, timed) in cases {
            for parameter in [0, 1, u32::from(u16::MAX), u32::from(u16::MAX) + 1, u32::MAX] {
                let wire = Message::DataChannelOpen(DataChannelOpen {
                    channel_type,
                    priority: 256,
                    reliability_parameter: parameter,
                    label: b"remote".to_vec(),
                    protocol: b"test".to_vec(),
                })
                .marshal()?;
                let result = RTCDataChannelInternal::accept(
                    0,
                    stream_id,
                    PayloadProtocolIdentifier::Dcep,
                    &wire,
                );
                stream_id += 1;
                if (retransmit || timed) && parameter > u32::from(u16::MAX) {
                    assert!(
                        result.is_err(),
                        "must reject out-of-range {channel_type:?} parameter {parameter}"
                    );
                    continue;
                }
                let accepted = result?;
                // Replies must use the remote channel's exact wire delivery mode.
                let config = accepted.data_channel.as_ref().unwrap().config();
                assert_eq!(config.channel_type, channel_type);
                assert_eq!(config.reliability_parameter, parameter);
                let id = pc.data_channels.insert(accepted);
                let channel = pc.data_channel(id).unwrap();
                assert_eq!(
                    channel.ordered(),
                    ordered,
                    "{channel_type:?} parameter {parameter}"
                );
                assert_eq!(
                    channel.max_retransmits(),
                    retransmit.then(|| u16::try_from(parameter).unwrap()),
                    "{channel_type:?} parameter {parameter}"
                );
                assert_eq!(
                    channel.max_packet_life_time(),
                    timed.then(|| u16::try_from(parameter).unwrap()),
                    "{channel_type:?} parameter {parameter}"
                );
                assert_eq!(channel.label(), "remote");
                assert_eq!(channel.protocol(), "test");
                assert!(!channel.negotiated());
                assert_eq!(channel.ready_state(), RTCDataChannelState::Open);
            }
        }
        Ok(())
    }
}
