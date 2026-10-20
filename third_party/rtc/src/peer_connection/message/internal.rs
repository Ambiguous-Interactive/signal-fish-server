use crate::data_channel::RTCDataChannelId;
use crate::data_channel::message::RTCDataChannelMessage;
use crate::media_stream::track::MediaStreamTrackId;
use crate::peer_connection::event::RTCEventInternal;
use bytes::BytesMut;
use datachannel::data_channel::DataChannelMessage;
use interceptor::Packet;
use shared::TransportContext;
use shared::marshal::MarshalSize;
use std::time::Instant;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum DataChannelEvent {
    Open,
    Message(RTCDataChannelMessage),
    Close,
    ClosedAfterLocalClose,
    BufferedAmountLow,
    BufferedAmountHigh,
}

#[derive(Debug, Clone)]
pub(crate) struct ApplicationMessage {
    pub(crate) data_channel_id: RTCDataChannelId,
    pub(crate) data_channel_event: DataChannelEvent,
}

#[derive(Debug, Clone)]
pub(crate) struct TrackPacket {
    pub(crate) track_id: MediaStreamTrackId,
    pub(crate) packet: Packet,
}

#[derive(Debug, Clone)]
pub(crate) enum STUNMessage {
    Raw(BytesMut),
}

#[derive(Debug, Clone)]
pub(crate) enum SctpStreamEvent {
    Closed(usize, sctp::StreamId),
    BufferReleased(usize, sctp::StreamId, usize),
    BufferedAmountLow(usize, sctp::StreamId),
    BufferedAmountHigh(usize, sctp::StreamId),
}

impl From<SctpStreamEvent> for RTCEventInternal {
    fn from(event: SctpStreamEvent) -> Self {
        match event {
            SctpStreamEvent::Closed(association, stream) => {
                Self::SCTPStreamClosed(association, stream)
            }
            SctpStreamEvent::BufferReleased(association, stream, bytes) => {
                Self::SCTPBufferReleased(association, stream, bytes)
            }
            SctpStreamEvent::BufferedAmountLow(association, stream) => {
                Self::SCTPBufferedAmountLow(association, stream)
            }
            SctpStreamEvent::BufferedAmountHigh(association, stream) => {
                Self::SCTPBufferedAmountHigh(association, stream)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum DTLSMessage {
    Raw(BytesMut),
    Sctp(DataChannelMessage),
    // Ordered stream notification, tied to the generation before the next OPEN.
    SctpStreamEvent(SctpStreamEvent),
    // Local control for an existing, remotely opened SCTP stream. It carries
    // no wire message and must precede the channel's ACK and application data.
    SctpStreamConfig {
        association_handle: usize,
        stream_id: sctp::StreamId,
        unordered: bool,
        reliability_type: sctp::ReliabilityType,
        reliability_parameter: u32,
    },
    SctpStreamReject {
        association_handle: usize,
        stream_id: sctp::StreamId,
    },
    DataChannel(ApplicationMessage),
}

#[derive(Debug, Clone)]
pub(crate) enum RTPMessage {
    Raw(BytesMut),
    Packet(Packet),
    TrackPacket(TrackPacket),
}

#[derive(Debug, Clone)]
pub(crate) enum RTCMessageInternal {
    Raw(BytesMut),
    Stun(STUNMessage),
    Dtls(DTLSMessage),
    Rtp(RTPMessage),
}

impl RTCMessageInternal {
    /// Returns the size in bytes of the message payload.
    pub(crate) fn len(&self) -> usize {
        match self {
            RTCMessageInternal::Raw(bytes) => bytes.len(),
            RTCMessageInternal::Stun(STUNMessage::Raw(bytes)) => bytes.len(),
            RTCMessageInternal::Dtls(msg) => match msg {
                DTLSMessage::Raw(bytes) => bytes.len(),
                DTLSMessage::Sctp(dcm) => dcm.payload.len(),
                DTLSMessage::SctpStreamConfig { .. }
                | DTLSMessage::SctpStreamReject { .. }
                | DTLSMessage::SctpStreamEvent(_) => 0,
                DTLSMessage::DataChannel(app_msg) => match &app_msg.data_channel_event {
                    DataChannelEvent::Open
                    | DataChannelEvent::Close
                    | DataChannelEvent::ClosedAfterLocalClose
                    | DataChannelEvent::BufferedAmountLow
                    | DataChannelEvent::BufferedAmountHigh => 0,
                    DataChannelEvent::Message(rtc_dcm) => rtc_dcm.data.len(),
                },
            },
            RTCMessageInternal::Rtp(msg) => match msg {
                RTPMessage::Raw(bytes) => bytes.len(),
                RTPMessage::Packet(packet) => match packet {
                    // RTP header is typically 12 bytes + CSRC + extensions
                    Packet::Rtp(rtp) => rtp.marshal_size(),
                    // For RTCP, estimate based on packet count (typically 24-32 bytes per packet)
                    Packet::Rtcp(rtcp_packets) => {
                        let mut rtcp_packet_size = 0;
                        for rtcp_packet in rtcp_packets {
                            rtcp_packet_size += rtcp_packet.marshal_size();
                        }
                        rtcp_packet_size
                    }
                    _ => 0, // Future Packet variants: size unknown; treat as 0 for accounting.
                },
                RTPMessage::TrackPacket(tp) => match &tp.packet {
                    Packet::Rtp(rtp) => rtp.marshal_size(),
                    Packet::Rtcp(rtcp_packets) => {
                        let mut rtcp_packet_size = 0;
                        for rtcp_packet in rtcp_packets {
                            rtcp_packet_size += rtcp_packet.marshal_size();
                        }
                        rtcp_packet_size
                    }
                    _ => 0,
                },
            },
        }
    }
}

pub(crate) struct TaggedRTCMessageInternal {
    pub(crate) now: Instant,
    pub(crate) transport: TransportContext,
    pub(crate) message: RTCMessageInternal,
}
