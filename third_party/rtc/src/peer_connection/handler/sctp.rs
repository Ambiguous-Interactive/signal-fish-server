use crate::peer_connection::event::{RTCEventInternal, TaggedRTCEventInternal};
use crate::peer_connection::message::internal::{
    DTLSMessage, RTCMessageInternal, SctpStreamEvent, TaggedRTCMessageInternal,
};
use crate::peer_connection::transport::sctp::SctpTransport;
use bytes::BytesMut;
use datachannel::data_channel::DataChannelMessage;
use datachannel::message::Message;
use datachannel::message::message_channel_threshold::DataChannelThreshold;
use log::{debug, warn};
use sctp::{
    Association, AssociationError, AssociationEvent, AssociationHandle, ClientConfig,
    DatagramEvent, EndpointEvent, Event, Payload, PayloadProtocolIdentifier, StreamEvent, StreamId,
};
use shared::error::{Error, Result};
use shared::marshal::Unmarshal;
use shared::{TransportContext, TransportMessage};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

pub(crate) struct SctpHandlerContext {
    pub(crate) sctp_transport: SctpTransport,

    pub(crate) read_outs: VecDeque<TaggedRTCMessageInternal>,
    pub(crate) write_outs: VecDeque<TaggedRTCMessageInternal>,
    pub(crate) event_outs: VecDeque<TaggedRTCEventInternal>,

    // Batch-drain state: handle_read/handle_write/handle_timeout only INGEST (set
    // `flush_dirty`); the single transmit flush runs in poll_write, once per driver
    // event-loop iteration. Paired with the driver's burst-read, N received DATA
    // chunks are processed before one flush, so the ack machine coalesces them into
    // ONE SACK (1st arms Delay, 2nd+ stay Immediate until flushed) instead of one
    // SACK per two packets — cutting sendto/recvfrom and amortizing per-iteration
    // cost. `now` carries the newest timestamp seen into that flush.
    flush_dirty: bool,

    /// Set by [`SctpHandler::request_abort`]: abort every association at the next `poll_write`.
    abort_requested: bool,

    /// Streams left with unread data because the pipeline backlog was over its bound.
    ///
    /// This set is what makes bounded draining safe. `StreamEvent::Readable` is
    /// **edge-triggered** — `association/mod.rs` emits it from `handle_data` when a *new* DATA
    /// chunk arrives, and from ForwardTSN; it is never re-emitted merely because unread data
    /// remains. So a handler that stops mid-stream gets no further notification, and the
    /// moment back-pressure works the peer stops sending, so no new chunk arrives, so no new
    /// `Readable` fires: **the stream would deadlock exactly when the feature engages.**
    /// Remembering the stream here, and retrying from `handle_read`/`handle_timeout`, is the
    /// level-triggered re-arm that closes that hole.
    pending_readable: HashSet<(AssociationHandle, StreamId)>,

    /// Transport context of the most recent inbound datagram, **per association**.
    ///
    /// A resumed drain has no triggering packet to copy this from. A single `last_transport`
    /// would be wrong the moment the endpoint carries more than one association — and it can:
    /// `sctp_associations` is a map, and `establish_two()` in the tests builds exactly that
    /// case. Messages resumed for association A would then be stamped with B's peer address.
    ///
    /// Keyed by association because the 5-tuple is a property of the association, not of the
    /// stream. An association with no entry has never received a datagram, so it cannot have
    /// parked data either.
    association_transport: HashMap<AssociationHandle, TransportContext>,

    /// The newest instant a caller has supplied, seeded at construction.
    ///
    /// This is the one place in the core that genuinely needs a retained instant: the flush is
    /// armed by a `handle_*` but executed by the next `poll_write`, and `flush_transmits` does
    /// real time-dependent work there — retransmit timers, congestion window, SACK scheduling.
    /// It was `Option<Instant>` with an `unwrap_or_else(Instant::now)` fallback only because
    /// nothing seeded it; that fallback was taken on **every client connection**, for the flush
    /// that emits the INIT chunk. Seeding at construction makes the `Option` unnecessary.
    now: Instant,
}

impl SctpHandlerContext {
    pub(crate) fn new(now: Instant, sctp_transport: SctpTransport) -> Self {
        Self {
            sctp_transport,
            read_outs: VecDeque::new(),
            write_outs: VecDeque::new(),
            event_outs: VecDeque::new(),
            flush_dirty: false,
            abort_requested: false,
            pending_readable: HashSet::new(),
            association_transport: HashMap::new(),
            now,
        }
    }

    /// Records the newest instant a caller has supplied.
    ///
    /// `max` rather than assignment, and without a monotonicity assert: an outbound message
    /// carries the instant of the input that *caused* it, so `handle_write` can legitimately be
    /// handed one older than the newest seen. See the design's §9.3.
    fn observe(&mut self, now: Instant) {
        self.now = now.max(self.now);
    }
}

/// How many undelivered inbound messages the pipeline may hold before this handler stops
/// draining SCTP's reassembly queues.
///
/// Draining unconditionally is what defeats SCTP's own flow control: bytes left in a
/// reassembly queue are what `get_my_receiver_window_credit()` subtracts from
/// `max_receive_buffer_size`, and that credit is advertised as `a_rwnd` in every SACK. Empty
/// the queue on arrival and `a_rwnd` never falls, so the peer is never told to slow down and
/// the backlog reappears — unbounded — further up the pipeline. Leaving messages in the
/// reassembly queue is not a leak; it is the mechanism.
///
/// The bound is a message count, so the bytes held depend on message size; SCTP's own
/// `max_receive_buffer_size` (1 MiB by default, `SettingEngineBuilder::with_sctp_max_receive_buffer_size`)
/// is the byte-denominated bound underneath it and is what the peer actually sees.
pub(crate) const SCTP_PIPELINE_READ_BACKLOG_LIMIT: usize = 256;

/// SctpHandler implements SCTP Protocol handling
pub(crate) struct SctpHandler<'a> {
    ctx: &'a mut SctpHandlerContext,
    /// Undelivered messages already sitting in `pipeline_context.read_outs`.
    ///
    /// Passed in rather than read from `ctx` because **this handler's own `read_outs` is not
    /// where the backlog forms**: the pipeline drains it into `intermediate_routs` inside the
    /// same `handle_read` call, so it is empty again by the time anything could observe it.
    /// Bounding against it would be a no-op. The queue that actually grows when the
    /// application stops consuming is the pipeline's, one hop downstream.
    downstream_backlog: usize,
}

impl<'a> SctpHandler<'a> {
    pub(crate) fn new(ctx: &'a mut SctpHandlerContext, downstream_backlog: usize) -> Self {
        SctpHandler {
            ctx,
            downstream_backlog,
        }
    }

    /// How many more messages may be pulled out of SCTP before the pipeline is over its bound.
    fn read_budget(&self) -> usize {
        SCTP_PIPELINE_READ_BACKLOG_LIMIT
            .saturating_sub(self.downstream_backlog)
            .saturating_sub(self.ctx.read_outs.len())
    }

    /// Drain one stream while budget allows, recording it as pending if data is left behind.
    ///
    /// Returns the messages read. The budget is checked **before** each `read_sctp()` because
    /// that call consumes a message: testing afterwards would mean having already taken the
    /// thing there was no room for.
    fn drain_stream(
        ctx_pending: &mut HashSet<(AssociationHandle, StreamId)>,
        conn: &mut Association,
        ch: AssociationHandle,
        id: StreamId,
        max_len: usize,
        budget: &mut usize,
        messages: &mut Vec<SctpMessage>,
    ) -> Result<()> {
        // An SCTP DATA chunk may arrive in the same flight as the tail of the
        // association handshake.  Do not expose that chunk to the DataChannel
        // handler while the association is still in COOKIE-WAIT/COOKIE-ECHOED:
        // doing so can announce `OnOpen` before the underlying data transport is
        // established, and an immediate application send cannot be transmitted
        // reliably.  Keep the chunk in SCTP's reassembly queue and let the
        // level-triggered pending path retry it after Event::Connected.
        if conn.is_handshaking() {
            ctx_pending.insert((ch, id));
            return Ok(());
        }

        let mut stream = match conn.stream(id) {
            Ok(stream) => stream,
            Err(Error::ErrStreamNotExisted) => {
                ctx_pending.remove(&(ch, id));
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        loop {
            if *budget == 0 {
                // Leave the rest in the reassembly queue: that is what shrinks `a_rwnd`.
                ctx_pending.insert((ch, id));
                return Ok(());
            }
            let chunks = match stream.read_sctp() {
                Ok(Some(chunks)) => chunks,
                Ok(None) | Err(Error::ErrStreamClosed) => {
                    ctx_pending.remove(&(ch, id));
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            // Reassemble straight into the delivered buffer: one copy instead of the
            // scratch-buffer round-trip (`max_len` preserves the max-message-size bound
            // `read()` enforced via `ErrShortBuffer`).
            let payload = chunks.to_payload(max_len)?;
            messages.push(SctpMessage::Inbound(DataChannelMessage {
                association_handle: ch.0,
                stream_id: id,
                ppi: chunks.ppi,
                payload,
                negotiated: false,
            }));
            *budget -= 1;
        }
    }

    fn drain_association_events(
        ctx_pending: &mut HashSet<(AssociationHandle, StreamId)>,
        event_outs: &mut VecDeque<TaggedRTCEventInternal>,
        conn: &mut Association,
        ch: AssociationHandle,
        now: Instant,
        limits: (usize, &mut usize),
        messages: &mut Vec<SctpMessage>,
    ) -> Result<()> {
        let (max_len, budget) = limits;
        while let Some(event) = conn.poll() {
            match event {
                Event::HandshakeFailed { reason } => {
                    debug!(
                        "association_handle {} handshake failed due to {}",
                        ch.0, reason
                    );
                    //TODO: put it into event_outs?
                }
                Event::Connected => {
                    debug!("association_handle {} is connected", ch.0);
                    event_outs.push_back(TaggedRTCEventInternal {
                        now,
                        event: RTCEventInternal::SCTPHandshakeComplete(ch.0),
                    });
                }
                Event::AssociationLost { reason, id } => {
                    debug!("association_handle {} is closed due to {}", ch.0, reason);
                    ctx_pending.remove(&(ch, id));
                    messages.push(SctpMessage::StreamEvent(SctpStreamEvent::Closed(ch.0, id)));
                }
                Event::Stream(StreamEvent::Readable { id }) => {
                    // Bounded: what is not read stays in the reassembly queue,
                    // which is what lowers `a_rwnd` and throttles the peer.
                    Self::drain_stream(ctx_pending, conn, ch, id, max_len, budget, messages)?;
                }
                Event::Stream(StreamEvent::BufferedAmountLow { id }) => {
                    debug!(
                        "association_handle {} stream id {} is buffered amount low",
                        ch.0, id
                    );
                    messages.push(SctpMessage::StreamEvent(
                        SctpStreamEvent::BufferedAmountLow(ch.0, id),
                    ));
                }
                Event::Stream(StreamEvent::BufferedAmountHigh { id }) => {
                    debug!(
                        "association_handle {} stream id {} is buffered amount high",
                        ch.0, id
                    );
                    messages.push(SctpMessage::StreamEvent(
                        SctpStreamEvent::BufferedAmountHigh(ch.0, id),
                    ));
                }
                Event::Stream(StreamEvent::BufferedAmountReleased {
                    id,
                    data_channel_bytes,
                    ..
                }) => {
                    /*
                        RTC charges application payload bytes at send admission.
                        DCEP and empty-message fillers release SCTP wire storage
                        without releasing pending gameplay (see DataChannelHandler).
                    */
                    messages.push(SctpMessage::StreamEvent(SctpStreamEvent::BufferReleased(
                        ch.0,
                        id,
                        data_channel_bytes,
                    )));
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Retry streams parked by a full pipeline. The level-triggered half of the drain.
    fn resume_pending_reads(&mut self, now: Instant) -> Result<()> {
        if self.ctx.pending_readable.is_empty() {
            return Ok(());
        }
        let mut budget = self.read_budget();
        if budget == 0 {
            return Ok(());
        }

        let max_len = self.ctx.sctp_transport.internal_buffer.len();
        let mut pending: Vec<(AssociationHandle, StreamId)> =
            self.ctx.pending_readable.iter().copied().collect();
        pending.sort_unstable();

        let mut drained_any = false;
        for (ch, id) in pending {
            if budget == 0 {
                break;
            }
            let Some(transport) = self.ctx.association_transport.get(&ch).copied() else {
                // Never received a datagram, so it cannot have parked data.
                self.ctx.pending_readable.remove(&(ch, id));
                continue;
            };
            let Some(conn) = self.ctx.sctp_transport.sctp_associations.get_mut(&ch) else {
                // The association went away while parked; nothing to resume. Unreachable in
                // practice — the two sites that remove an association clear both maps with it
                // — so this drops the transport entry too rather than leaving the one map to
                // outlive the other.
                self.ctx.pending_readable.remove(&(ch, id));
                self.ctx.association_transport.remove(&ch);
                continue;
            };

            // Drained per association, and stamped with *that* association's 5-tuple: a
            // shared batch would let one association's messages inherit another's transport.
            let mut messages = vec![];
            Self::drain_stream(
                &mut self.ctx.pending_readable,
                conn,
                ch,
                id,
                max_len,
                &mut budget,
                &mut messages,
            )?;

            Self::drain_association_events(
                &mut self.ctx.pending_readable,
                &mut self.ctx.event_outs,
                conn,
                ch,
                now,
                (max_len, &mut budget),
                &mut messages,
            )?;
            for message in messages {
                if let SctpMessage::StreamEvent(event) = &message {
                    self.ctx.read_outs.push_back(TaggedRTCMessageInternal {
                        now,
                        transport,
                        message: RTCMessageInternal::Dtls(DTLSMessage::SctpStreamEvent(
                            event.clone(),
                        )),
                    });
                }
                if let SctpMessage::Inbound(message) = message {
                    drained_any = true;
                    self.ctx.read_outs.push_back(TaggedRTCMessageInternal {
                        now,
                        transport,
                        message: RTCMessageInternal::Dtls(DTLSMessage::Sctp(message)),
                    });
                }
            }
        }

        if drained_any {
            // Reading drops bytes out of the reassembly queue, which raises the
            // receiver-window credit — the peer only learns that from a SACK, so flush.
            self.ctx.flush_dirty = true;
        }
        Ok(())
    }

    pub(crate) fn name(&self) -> &'static str {
        "SctpHandler"
    }

    /// Asks for every association to be aborted (W3C `close()`: "tear down the underlying SCTP
    /// association by sending an SCTP ABORT chunk").
    ///
    /// Done at this handler's next `poll_write`, not here, so the data still pending in the
    /// associations goes out first: [`Association::abort`] would discard it.
    pub(crate) fn request_abort(&mut self) {
        self.ctx.abort_requested = true;
    }

    /// Aborts every association and queues the ABORTs. The local data channels are closed
    /// abruptly, without their closing procedure (W3C `close()`), so the stream losses the aborts
    /// report are dropped rather than surfaced as `OnClose`.
    fn abort_associations(&mut self) {
        for association in self.ctx.sctp_transport.sctp_associations.values_mut() {
            if let Err(err) = association.abort() {
                warn!("failed to abort sctp association: {}", err);
            }
            while association.poll().is_some() {}
        }
        self.ctx.pending_readable.clear();
        self.flush_transmits(self.ctx.now);
    }

    /// Batch-drain flush: gather every association's pending outbound in one pass
    /// into `write_outs`. Called once per event-loop iteration from poll_write when
    /// `flush_dirty` is set, after a burst of inbound packets has been ingested, so
    /// their SACKs coalesce into a single datagram.
    fn flush_transmits(&mut self, now: Instant) {
        for conn in self.ctx.sctp_transport.sctp_associations.values_mut() {
            while let Some(x) = conn.poll_transmit(now) {
                for transmit in split_transmit(x) {
                    if let Payload::RawEncode(raw_data) = transmit.message {
                        for raw in raw_data {
                            self.ctx.write_outs.push_back(TaggedRTCMessageInternal {
                                now: transmit.now,
                                transport: transmit.transport,
                                message: RTCMessageInternal::Dtls(DTLSMessage::Raw(
                                    BytesMut::from(&raw[..]),
                                )),
                            });
                        }
                    }
                }
            }
        }
    }
}

enum SctpMessage {
    Inbound(DataChannelMessage),
    StreamEvent(SctpStreamEvent),
    Outbound(TransportMessage<Payload>),
}

impl<'a>
    sansio::Protocol<TaggedRTCMessageInternal, TaggedRTCMessageInternal, TaggedRTCEventInternal>
    for SctpHandler<'a>
{
    type Rout = TaggedRTCMessageInternal;
    type Wout = TaggedRTCMessageInternal;
    type Eout = TaggedRTCEventInternal;
    type Error = Error;
    type Time = Instant;

    fn handle_read(&mut self, msg: TaggedRTCMessageInternal) -> Result<()> {
        let now = msg.now;
        if let RTCMessageInternal::Dtls(DTLSMessage::Raw(dtls_message)) = msg.message {
            debug!("recv sctp RAW {:?}", msg.transport.peer_addr);

            if self.ctx.sctp_transport.sctp_endpoint.is_none() {
                warn!(
                    "drop sctp RAW {:?} due to sctp_endpoint is not ready yet",
                    msg.transport.peer_addr
                );
                return Ok(());
            }

            // Both are read before `sctp_transport` is split up below, which would otherwise
            // conflict with the borrow of `sctp_associations`.
            let max_len = self.ctx.sctp_transport.internal_buffer.len();
            let mut budget = self.read_budget();
            let mut inbound_transport = None;

            let (sctp_endpoint, sctp_associations) = (
                self.ctx
                    .sctp_transport
                    .sctp_endpoint
                    .as_mut()
                    .ok_or(Error::ErrSCTPNotEstablished)?,
                &mut self.ctx.sctp_transport.sctp_associations,
            );

            let mut sctp_events: HashMap<AssociationHandle, VecDeque<AssociationEvent>> =
                HashMap::new();
            if let Some((ch, event)) = sctp_endpoint.handle(
                msg.now,
                msg.transport.peer_addr,
                msg.transport.ecn,
                dtls_message.freeze(), //TODO: switch API Bytes to BytesMut
            ) {
                // `ch` is the association this datagram belongs to — the only safe key for
                // remembering its 5-tuple.
                inbound_transport = Some((ch, msg.transport));
                match event {
                    DatagramEvent::NewAssociation(conn) => {
                        sctp_associations.insert(ch, conn);
                    }
                    DatagramEvent::AssociationEvent(event) => {
                        sctp_events.entry(ch).or_default().push_back(event);
                    }
                    _ => {}
                }
            }

            let mut messages = vec![];
            {
                let mut endpoint_events: Vec<(AssociationHandle, EndpointEvent)> = vec![];

                for (ch, conn) in sctp_associations.iter_mut() {
                    for (event_ch, conn_events) in sctp_events.iter_mut() {
                        if ch == event_ch {
                            for event in conn_events.drain(..) {
                                debug!(
                                    "association_handle {} handle_event for Datagram from {}",
                                    ch.0, msg.transport.peer_addr
                                );
                                conn.handle_event(event);
                            }
                        }
                    }

                    Self::drain_association_events(
                        &mut self.ctx.pending_readable,
                        &mut self.ctx.event_outs,
                        conn,
                        *ch,
                        now,
                        (max_len, &mut budget),
                        &mut messages,
                    )?;

                    while let Some(event) = conn.poll_endpoint_event() {
                        endpoint_events.push((*ch, event));
                    }
                    // Transmit flush is deferred to poll_write (batch-drain) so a
                    // burst of inbound DATA coalesces into one SACK.
                }

                // Teardown safety: each association that emitted an endpoint event
                // is about to be removed below (drain/shutdown). Flush ITS pending
                // outbound now — the deferred poll_write flush runs after removal and
                // would drop its final packets. Only the draining associations are
                // touched, so non-draining ones keep coalescing and still flush
                // normally in poll_write.
                for (drained_ch, _event) in &endpoint_events {
                    if let Some(conn) = sctp_associations.get_mut(drained_ch) {
                        while let Some(x) = conn.poll_transmit(now) {
                            for transmit in split_transmit(x) {
                                messages.push(SctpMessage::Outbound(transmit));
                            }
                        }
                    }
                }

                for (ch, event) in endpoint_events {
                    sctp_endpoint.handle_event(ch, event); // handle drain event
                    sctp_associations.remove(&ch);
                    // Drop the per-association bookkeeping with it, or a long-lived endpoint
                    // accumulates entries for associations that no longer exist.
                    self.ctx.association_transport.remove(&ch);
                    self.ctx.pending_readable.retain(|(pch, _)| *pch != ch);
                }
            }

            for message in messages {
                match message {
                    SctpMessage::Inbound(message) => {
                        debug!(
                            "recv sctp data channel message {:?}",
                            msg.transport.peer_addr
                        );
                        self.ctx.read_outs.push_back(TaggedRTCMessageInternal {
                            now: msg.now,
                            transport: msg.transport,
                            message: RTCMessageInternal::Dtls(DTLSMessage::Sctp(message)),
                        })
                    }
                    SctpMessage::StreamEvent(event) => {
                        self.ctx.read_outs.push_back(TaggedRTCMessageInternal {
                            now,
                            transport: msg.transport,
                            message: RTCMessageInternal::Dtls(DTLSMessage::SctpStreamEvent(event)),
                        });
                    }
                    SctpMessage::Outbound(transmit) => {
                        if let Payload::RawEncode(raw_data) = transmit.message {
                            for raw in raw_data {
                                self.ctx.write_outs.push_back(TaggedRTCMessageInternal {
                                    now: transmit.now,
                                    transport: transmit.transport,
                                    message: RTCMessageInternal::Dtls(DTLSMessage::Raw(
                                        BytesMut::from(&raw[..]),
                                    )),
                                });
                            }
                        }
                    }
                }
            }

            if let Some((ch, transport)) = inbound_transport
                && self.ctx.sctp_transport.sctp_associations.contains_key(&ch)
            {
                self.ctx.association_transport.insert(ch, transport);
            }

            // The application may have drained while this packet was in flight, so retry
            // anything parked before giving up until the retry timer.
            self.resume_pending_reads(now)?;

            // Ingest done — mark dirty so poll_write runs the single flush.
            self.ctx.flush_dirty = true;
            self.ctx.observe(now);
        } else {
            // Bypass
            debug!("bypass sctp read {:?}", msg.transport.peer_addr);
            self.ctx.read_outs.push_back(msg);
        }
        Ok(())
    }

    fn poll_read(&mut self) -> Option<Self::Rout> {
        // Top up from streams parked by back-pressure. This is exactly when capacity has
        // appeared — somebody is asking for data, so the pipeline ahead has drained — and it
        // needs no timer, no invented interval and no influence on `poll_timeout`.
        //
        // It is also the level-triggered half of the bounded drain. `StreamEvent::Readable`
        // is edge-triggered: it fires on a *new* DATA chunk, never because unread data
        // remains. The moment back-pressure works the peer stops sending, so nothing arrives
        // to re-trigger the drain — without this the stream would deadlock precisely when the
        // feature engages.
        if self.ctx.read_outs.is_empty() && !self.ctx.pending_readable.is_empty() {
            let now = self.ctx.now;
            if let Err(err) = self.resume_pending_reads(now) {
                warn!("failed to resume parked SCTP streams: {}", err);
            }
        }
        self.ctx.read_outs.pop_front()
    }

    fn handle_write(&mut self, msg: TaggedRTCMessageInternal) -> Result<()> {
        let now = msg.now;
        if let RTCMessageInternal::Dtls(DTLSMessage::SctpStreamReject {
            association_handle,
            stream_id,
        }) = &msg.message
        {
            let conn = self
                .ctx
                .sctp_transport
                .sctp_associations
                .get_mut(&AssociationHandle(*association_handle))
                .ok_or(Error::ErrSCTPNotEstablished)?;
            conn.stream(*stream_id)?.close(now)?;
            // The reset completion event releases the rejection guard. Do not
            // report StreamClosed while late packets can still use this stream.
            self.ctx.flush_dirty = true;
            self.ctx.observe(now);
            return Ok(());
        }
        if let RTCMessageInternal::Dtls(DTLSMessage::SctpStreamConfig {
            association_handle,
            stream_id,
            unordered,
            reliability_type,
            reliability_parameter,
        }) = &msg.message
        {
            let conn = self
                .ctx
                .sctp_transport
                .sctp_associations
                .get_mut(&AssociationHandle(*association_handle))
                .ok_or(Error::ErrSCTPNotEstablished)?;
            conn.stream(*stream_id)?.set_reliability_params(
                *unordered,
                *reliability_type,
                *reliability_parameter,
            )?;
            self.ctx.observe(now);
            return Ok(());
        }
        if let RTCMessageInternal::Dtls(DTLSMessage::Sctp(mut message)) = msg.message {
            debug!(
                "send sctp data channel message to {:?}",
                msg.transport.peer_addr
            );

            if message.payload.len() > self.ctx.sctp_transport.internal_buffer.len() {
                return Err(Error::ErrOutboundPacketTooLarge);
            }

            if let Some(conn) = self
                .ctx
                .sctp_transport
                .sctp_associations
                .get_mut(&AssociationHandle(message.association_handle))
            {
                let mut is_dcep_internal_control_message = false;
                if message.ppi == PayloadProtocolIdentifier::Dcep {
                    let mut data_buf = &message.payload[..];
                    let dcep_message = Message::unmarshal(&mut data_buf)?;
                    match dcep_message {
                        Message::DataChannelOpen(data_channel_open) => {
                            debug!(
                                "sctp data channel open {:?} for stream id {}",
                                data_channel_open, message.stream_id
                            );
                            let (unordered, reliability_type) =
                                ::datachannel::data_channel::DataChannel::get_reliability_params(
                                    data_channel_open.channel_type,
                                );
                            let mut stream = conn.open_stream(message.stream_id, message.ppi)?;
                            stream.set_reliability_params(
                                unordered,
                                reliability_type,
                                data_channel_open.reliability_parameter,
                            )?;

                            // Out-of-band negotiated channels (W3C WebRTC
                            // `RTCDataChannelInit.negotiated`) only open the SCTP
                            // stream locally; the DCEP handshake must not be sent
                            // to the peer, which already created its own channel
                            // with the pre-agreed stream id.
                            if message.negotiated {
                                is_dcep_internal_control_message = true;
                            }
                        }
                        Message::DataChannelClose(_) => {
                            is_dcep_internal_control_message = true;
                            debug!(
                                "sctp data channel close for stream id {}",
                                message.stream_id
                            );
                            let mut stream = conn.stream(message.stream_id)?;
                            stream.close(now)?;

                            // Closing requests a reset; the association reports
                            // completion only when both stream directions finish.
                            // Keep the channel's SID reserved until that event.
                        }
                        Message::DataChannelThreshold(data_channel_threshold) => {
                            is_dcep_internal_control_message = true;
                            debug!(
                                "sctp data channel set threshold {:?} for stream id {}",
                                data_channel_threshold, message.stream_id
                            );
                            let mut stream = conn.stream(message.stream_id)?;
                            match data_channel_threshold {
                                DataChannelThreshold::Low(threshold) => {
                                    stream.set_buffered_amount_low_threshold(threshold as usize)?;
                                }
                                DataChannelThreshold::High(threshold) => {
                                    stream
                                        .set_buffered_amount_high_threshold(threshold as usize)?;
                                }
                            }
                        }
                        _ => {}
                    }
                }

                let mut stream = conn.stream(message.stream_id)?;
                if !is_dcep_internal_control_message {
                    if !stream.is_writable() {
                        return Err(Error::ErrStreamClosed);
                    }
                    // The payload is owned end-to-end (the DataChannel `send` API
                    // takes the buffer by value), so hand it to SCTP zero-copy:
                    // `freeze()` is O(1) and the enqueued chunks are refcounted
                    // slices, eliminating a per-message alloc + full-payload memcpy.
                    let payload = std::mem::take(&mut message.payload).freeze();
                    stream.write_chunk_with_ppi(now, &payload, message.ppi)?;
                }

                // Transmit flush is deferred to poll_write (batch-drain).
            } else {
                return Err(Error::ErrAssociationNotExisted);
            }

            // Ingest done — mark dirty so poll_write runs the single flush.
            self.ctx.flush_dirty = true;
            self.ctx.observe(now);
        } else {
            // Bypass
            debug!("Bypass sctp write {:?}", msg.transport.peer_addr);
            self.ctx.write_outs.push_back(msg);
        }
        Ok(())
    }

    fn poll_write(&mut self) -> Option<Self::Wout> {
        // Batch-drain: run the single deferred transmit flush for this event-loop
        // iteration before serving queued outbound.
        if self.ctx.flush_dirty || self.ctx.abort_requested {
            self.ctx.flush_dirty = false;
            self.flush_transmits(self.ctx.now);
        }
        // After the flush above, so data the associations still held is sent before the ABORT.
        if std::mem::take(&mut self.ctx.abort_requested) {
            self.abort_associations();
        }
        self.ctx.write_outs.pop_front()
    }

    fn handle_event(&mut self, evt: TaggedRTCEventInternal) -> Result<()> {
        // The event's instant arms the deferred flush below, so `poll_write` can stamp the
        // client INIT with the time the handshake completed rather than the wall clock.
        let now = evt.now;
        self.ctx.observe(now);

        // DTLSHandshakeComplete is not terminated here since SRTP handler needs it
        let dtls_complete = matches!(&evt.event, RTCEventInternal::DTLSHandshakeComplete(_, _));
        if dtls_complete {
            debug!("sctp recv dtls handshake complete");

            if let Some(sctp_transport_config) =
                self.ctx.sctp_transport.sctp_transport_config.clone()
            {
                let (sctp_endpoint, sctp_associations) = (
                    self.ctx
                        .sctp_transport
                        .sctp_endpoint
                        .as_mut()
                        .ok_or(Error::ErrSCTPNotEstablished)?,
                    &mut self.ctx.sctp_transport.sctp_associations,
                );

                debug!("sctp endpoint initiates connection for dtls client role");
                let (ch, conn) = sctp_endpoint
                    .connect(
                        now,
                        ClientConfig::new(sctp_transport_config),
                        TransportContext::default().peer_addr,
                    )
                    .map_err(|e| Error::Other(e.to_string()))?;

                sctp_associations.insert(ch, conn);

                // `connect` queued the client INIT. Mark dirty so poll_write emits
                // it at the next flush instead of waiting for a later handle_* (the
                // deferred flush is now the only transmit path).
                self.ctx.flush_dirty = true;
            }
        }

        // The peer closed the DTLS transport the associations run over, so they are over too.
        // Nothing is sent: there is no transport left to send it on. The data channel handler,
        // next in line for this event, closes the channels themselves.
        if matches!(&evt.event, RTCEventInternal::DTLSClosed) {
            debug!("sctp closing associations: dtls closed by peer");
            for association in self.ctx.sctp_transport.sctp_associations.values_mut() {
                if let Err(err) = association.close(AssociationError::ApplicationClosed) {
                    warn!("failed to close sctp association: {}", err);
                }
            }
            self.ctx.pending_readable.clear();
        }

        self.ctx.event_outs.push_back(evt);

        Ok(())
    }

    fn poll_event(&mut self) -> Option<Self::Eout> {
        self.ctx.event_outs.pop_front()
    }

    fn handle_timeout(&mut self, now: Instant) -> Result<()> {
        if self.ctx.sctp_transport.sctp_endpoint.is_none() {
            return Ok(());
        }

        let mut transmits = vec![];

        let (sctp_endpoint, sctp_associations) = (
            self.ctx
                .sctp_transport
                .sctp_endpoint
                .as_mut()
                .ok_or(Error::ErrSCTPNotEstablished)?,
            &mut self.ctx.sctp_transport.sctp_associations,
        );

        let mut endpoint_events: Vec<(AssociationHandle, EndpointEvent)> = vec![];
        for (ch, conn) in sctp_associations.iter_mut() {
            conn.handle_timeout(now);

            while let Some(event) = conn.poll_endpoint_event() {
                endpoint_events.push((*ch, event));
            }
            // Transmit flush is deferred to poll_write (batch-drain).
        }

        // Teardown safety (see handle_read): flush each draining association's final
        // packets before it is removed below, so the deferred flush doesn't drop
        // them. Non-draining associations flush normally in poll_write.
        for (drained_ch, _event) in &endpoint_events {
            if let Some(conn) = sctp_associations.get_mut(drained_ch) {
                while let Some(x) = conn.poll_transmit(now) {
                    transmits.extend(split_transmit(x));
                }
            }
        }

        for (ch, event) in endpoint_events {
            sctp_endpoint.handle_event(ch, event); // handle drain event
            sctp_associations.remove(&ch);
            self.ctx.association_transport.remove(&ch);
            self.ctx.pending_readable.retain(|(pch, _)| *pch != ch);
        }

        for transmit in transmits {
            if let Payload::RawEncode(raw_data) = transmit.message {
                for raw in raw_data {
                    self.ctx.write_outs.push_back(TaggedRTCMessageInternal {
                        now: transmit.now,
                        transport: transmit.transport,
                        message: RTCMessageInternal::Dtls(DTLSMessage::Raw(BytesMut::from(
                            &raw[..],
                        ))),
                    });
                }
            }
        }

        // Timer processed — mark dirty so poll_write runs the single flush.
        self.ctx.flush_dirty = true;
        self.ctx.observe(now);

        Ok(())
    }

    fn poll_timeout(&mut self) -> Option<Instant> {
        let mut eto = None;

        for conn in self.ctx.sctp_transport.sctp_associations.values() {
            if let Some(timeout) = conn.poll_timeout() {
                eto = Some(eto.map_or(timeout, |e: Instant| e.min(timeout)));
            }
        }

        eto
    }

    fn close(&mut self) -> Result<()> {
        Ok(())
    }
}

fn split_transmit(transmit: TransportMessage<Payload>) -> Vec<TransportMessage<Payload>> {
    let mut transmits: Vec<TransportMessage<Payload>> = Vec::new();
    if let Payload::RawEncode(contents) = transmit.message {
        for content in contents {
            transmits.push(TransportMessage {
                now: transmit.now,
                transport: transmit.transport,
                message: Payload::RawEncode(vec![content]),
            })
        }
    }

    transmits
}

#[cfg(test)]
pub(crate) use tests::established_test_association;

#[cfg(test)]
mod tests {
    //! Coverage for the batch-drain teardown-safety flush.
    //!
    //! `handle_read` / `handle_timeout` normally only INGEST and defer the transmit
    //! flush to `poll_write`. But when an association drains (graceful shutdown) it is
    //! removed in that same call — so its final packet (SHUTDOWN / SHUTDOWN_COMPLETE)
    //! would be dropped by a flush that runs *after* removal, and the peer would hang
    //! waiting for it. The teardown-safety block flushes pending outbound *before*
    //! removal. These tests drive a real client<->server SCTP handshake through the
    //! public `sctp` API (no DTLS/ICE), then verify the draining association's final
    //! packet reaches `write_outs` instead of being lost.

    use super::*;
    use crate::data_channel::registry::DataChannelRegistry;
    use crate::peer_connection::configuration::setting_engine::SctpMaxMessageSize;
    use crate::peer_connection::handler::datachannel::{
        DataChannelHandler, DataChannelHandlerContext,
    };
    use crate::peer_connection::message::internal::DataChannelEvent;
    use crate::peer_connection::transport::dtls::role::RTCDtlsRole;
    use crate::peer_connection::transport::{RTCTransportId, TransportKind};
    use crate::statistics::accumulator::RTCStatsAccumulator;
    use bytes::Bytes;
    use datachannel::message::message_channel_open::{
        CHANNEL_PRIORITY_NORMAL, ChannelType, DataChannelOpen,
    };
    use sansio::Protocol;
    use sctp::{Association, Endpoint, EndpointConfig, ServerConfig, TransportConfig};
    use shared::{TransportProtocol, marshal::Marshal};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    /// A fixed nonce keeps test ids deterministic while still distinguishing the kinds.
    fn test_transport_id(kind: TransportKind) -> RTCTransportId {
        RTCTransportId::new(0xabcd_ef01_2345_6789, kind)
    }

    fn client_addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444)
    }

    fn server_addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5555)
    }

    /// An established client<->server SCTP association pair, driven purely through the
    /// public `sctp` API so a handler test can operate on a real, connected
    /// association without standing up the full peer-connection stack.
    struct Established {
        client_ep: Endpoint,
        client_ch: AssociationHandle,
        client_conn: Association,
        server_ep: Endpoint,
        server_ch: AssociationHandle,
        server_conn: Association,
    }

    /// Drain every datagram an association currently wants to transmit.
    fn drain_transmits(conn: &mut Association, now: Instant) -> Vec<Bytes> {
        let mut out = vec![];
        while let Some(t) = conn.poll_transmit(now) {
            if let Payload::RawEncode(contents) = t.message {
                out.extend(contents);
            }
        }
        out
    }

    /// Run the SCTP handshake to completion and return the established pair. The
    /// shuttle plays the role the ICE handler plays in production: it rewrites each
    /// datagram's source address for the receiving endpoint.
    pub(crate) fn established_test_association(role: RTCDtlsRole) -> Association {
        let pair = establish();
        if role == RTCDtlsRole::Client {
            pair.client_conn
        } else {
            pair.server_conn
        }
    }

    fn establish() -> Established {
        establish_with_stream_count(u16::MAX)
    }

    fn establish_with_stream_count(max_streams: u16) -> Established {
        let now = Instant::now();
        let transport_config = TransportConfig::default()
            .with_max_num_inbound_streams(max_streams)
            .with_max_num_outbound_streams(max_streams);

        let mut client_ep = Endpoint::new(
            client_addr(),
            TransportProtocol::UDP,
            EndpointConfig::default().into(),
            None,
        );
        let mut server_ep = Endpoint::new(
            server_addr(),
            TransportProtocol::UDP,
            EndpointConfig::default().into(),
            Some(ServerConfig::new(transport_config.clone()).into()),
        );

        let (client_ch, mut client_conn) = client_ep
            .connect(now, ClientConfig::new(transport_config), server_addr())
            .expect("client connect");

        let mut server_conns: HashMap<AssociationHandle, Association> = HashMap::new();
        let mut connected = false;

        for _ in 0..50 {
            // client -> server
            let c_out = drain_transmits(&mut client_conn, now);
            let mut moved = !c_out.is_empty();
            for dgram in c_out {
                if let Some((ch, event)) = server_ep.handle(now, client_addr(), None, dgram) {
                    match event {
                        DatagramEvent::NewAssociation(conn) => {
                            server_conns.insert(ch, conn);
                        }
                        DatagramEvent::AssociationEvent(e) => {
                            if let Some(sc) = server_conns.get_mut(&ch) {
                                sc.handle_event(e);
                            }
                        }
                        _ => {}
                    }
                }
            }

            // server -> client
            let mut s_out = vec![];
            for sc in server_conns.values_mut() {
                while sc.poll().is_some() {}
                s_out.extend(drain_transmits(sc, now));
            }
            moved |= !s_out.is_empty();
            for dgram in s_out {
                if let Some((_ch, DatagramEvent::AssociationEvent(e))) =
                    client_ep.handle(now, server_addr(), None, dgram)
                {
                    client_conn.handle_event(e);
                }
            }

            while let Some(event) = client_conn.poll() {
                if let Event::Connected = event {
                    connected = true;
                }
            }

            if connected && !moved {
                break;
            }
        }

        assert!(connected, "SCTP handshake did not complete");
        assert_eq!(server_conns.len(), 1, "exactly one server association");
        let (server_ch, server_conn) = server_conns.into_iter().next().unwrap();

        Established {
            client_ep,
            client_ch,
            client_conn,
            server_ep,
            server_ch,
            server_conn,
        }
    }

    #[test]
    fn deferred_channel_failures_reserved_stream_id_is_rejected_before_connect() {
        use crate::data_channel::RTCDataChannelInit;
        use crate::peer_connection::RTCPeerConnectionBuilder;
        let mut pc = RTCPeerConnectionBuilder::new()
            .build(Instant::now())
            .unwrap();
        for _ in 0..3 {
            assert!(matches!(
                pc.create_data_channel(
                    "reserved",
                    Some(RTCDataChannelInit {
                        negotiated: Some(u16::MAX),
                        ..Default::default()
                    })
                ),
                Err(Error::ErrMaxDataChannelID)
            ));
            assert!(pc.data_channels.is_empty());
            assert!(!pc.data_channels.stream_id_in_use(&u16::MAX));
        }
        assert!(pc.create_data_channel("ordinary", None).is_ok());
    }

    fn connected_creation_fixed_id_history(role: RTCDtlsRole) {
        use crate::data_channel::RTCDataChannelInit;
        use crate::peer_connection::RTCPeerConnectionBuilder;
        let now = Instant::now();
        let pair = establish_with_stream_count(2);
        let (endpoint, handle, association) = if role == RTCDtlsRole::Client {
            (pair.client_ep, pair.client_ch, pair.client_conn)
        } else {
            (pair.server_ep, pair.server_ch, pair.server_conn)
        };
        let mut pc = RTCPeerConnectionBuilder::new().build(now).unwrap();
        pc.dtls_transport_mut().dtls_role = role;
        pc.pipeline_context.sctp_handler_context = client_ctx(now, endpoint, handle, association);
        assert_eq!(pc.sctp_transport().max_channels(), Some(2));
        for sid in [2, u16::MAX, 2] {
            assert!(
                matches!(
                    pc.create_data_channel(
                        "invalid",
                        Some(RTCDataChannelInit {
                            negotiated: Some(sid),
                            ..Default::default()
                        })
                    ),
                    Err(Error::ErrMaxDataChannelID)
                ),
                "role={role:?}, sid={sid}"
            );
            assert!(pc.data_channels.is_empty());
            assert!(!pc.data_channels.stream_id_in_use(&sid));
        }
        let mut valid = Vec::new();
        for sid in [0, 1] {
            let id = pc
                .create_data_channel(
                    "valid-fixed",
                    Some(RTCDataChannelInit {
                        negotiated: Some(sid),
                        ..Default::default()
                    }),
                )
                .unwrap()
                .id();
            valid.push(id);
            assert_eq!(pc.data_channels.handle_of_stream(&sid), Some(id));
            assert!(pc.data_channels.get(&id).unwrap().data_channel.is_some());
            let before = pc.data_channels.len();
            assert!(matches!(
                pc.create_data_channel(
                    "still-invalid",
                    Some(RTCDataChannelInit {
                        negotiated: Some(2),
                        ..Default::default()
                    })
                ),
                Err(Error::ErrMaxDataChannelID)
            ));
            assert_eq!(pc.data_channels.len(), before);
            assert_eq!(pc.data_channels.handle_of_stream(&sid), Some(id));
        }
        assert_eq!(pc.data_channels.len(), 2);
        assert_eq!(pc.data_channels.handle_of_stream(&0), Some(valid[0]));
        assert_eq!(pc.data_channels.handle_of_stream(&1), Some(valid[1]));
        assert!(pc.data_channels.pending_stream_id_handles().is_empty());
    }

    #[test]
    fn deferred_channel_failures_connected_creation_rejects_fixed_ids_client() {
        connected_creation_fixed_id_history(RTCDtlsRole::Client);
    }

    #[test]
    fn deferred_channel_failures_connected_creation_rejects_fixed_ids_server() {
        connected_creation_fixed_id_history(RTCDtlsRole::Server);
    }

    // Regression #1081: a failed deferred channel cannot stall another channel.
    fn deferred_channel_failure_history(role: RTCDtlsRole, ordered: bool) {
        use crate::data_channel::{RTCDataChannelInit, RTCDataChannelState};
        use crate::peer_connection::message::RTCMessage;
        use crate::peer_connection::{RTCPeerConnection, RTCPeerConnectionBuilder};

        fn forward(
            sender: &mut RTCPeerConnection,
            receiver: &mut RTCPeerConnection,
            source: SocketAddr,
            now: Instant,
        ) -> Vec<RTCMessage> {
            sender.pipeline_context.observe(now);
            receiver.pipeline_context.observe(now);
            sender.get_sctp_handler().handle_timeout(now).unwrap();
            while let Some(message) = sender.get_endpoint_handler().poll_write() {
                sender
                    .get_datachannel_handler()
                    .handle_write(message)
                    .unwrap();
            }
            while let Some(message) = sender.get_datachannel_handler().poll_write() {
                sender.get_sctp_handler().handle_write(message).unwrap();
            }
            while let Some(message) = sender.get_sctp_handler().poll_write() {
                if let RTCMessageInternal::Dtls(DTLSMessage::Raw(raw)) = message.message {
                    receiver
                        .get_sctp_handler()
                        .handle_read(TaggedRTCMessageInternal {
                            now,
                            transport: TransportContext {
                                peer_addr: source,
                                ..Default::default()
                            },
                            message: RTCMessageInternal::Dtls(DTLSMessage::Raw(raw)),
                        })
                        .unwrap();
                }
            }
            let mut output = Vec::new();
            while let Some(message) = receiver.poll_read() {
                output.push(message.message);
            }
            output
        }

        let now = Instant::now();
        let pair = establish_with_stream_count(2);
        let (sender_ep, sender_ch, sender, receiver_ep, receiver_ch, receiver, source, remote) =
            if role == RTCDtlsRole::Client {
                (
                    pair.client_ep,
                    pair.client_ch,
                    pair.client_conn,
                    pair.server_ep,
                    pair.server_ch,
                    pair.server_conn,
                    client_addr(),
                    server_addr(),
                )
            } else {
                (
                    pair.server_ep,
                    pair.server_ch,
                    pair.server_conn,
                    pair.client_ep,
                    pair.client_ch,
                    pair.client_conn,
                    server_addr(),
                    client_addr(),
                )
            };
        let sid = if role == RTCDtlsRole::Client { 0 } else { 1 };
        let mut pc = RTCPeerConnectionBuilder::new().build(now).unwrap();
        let closed = pc
            .create_data_channel("closed-before-connect", None)
            .unwrap()
            .id();
        pc.data_channel(closed).unwrap().close().unwrap();
        let closed_fixed = pc
            .create_data_channel(
                "closed-fixed",
                Some(RTCDataChannelInit {
                    negotiated: Some(sid),
                    ..Default::default()
                }),
            )
            .unwrap()
            .id();
        pc.data_channel(closed_fixed).unwrap().close().unwrap();
        let invalid = pc
            .create_data_channel(
                "out-of-range",
                Some(RTCDataChannelInit {
                    negotiated: Some(2),
                    ..Default::default()
                }),
            )
            .unwrap()
            .id();
        // Exercise defensive connect-time validation even though the public API
        // now rejects the reserved value before inserting a pending channel.
        let invalid_max =
            pc.data_channels
                .insert(crate::data_channel::internal::RTCDataChannelInternal::new(
                    crate::data_channel::parameters::DataChannelParameters {
                        label: "invalid-max".to_owned(),
                        negotiated: Some(u16::MAX),
                        ..Default::default()
                    },
                ));
        let options = RTCDataChannelInit {
            ordered,
            max_retransmits: (!ordered).then_some(0),
            ..Default::default()
        };
        let valid = pc
            .create_data_channel("valid", Some(options.clone()))
            .unwrap()
            .id();
        let excess = pc.create_data_channel("excess", None).unwrap().id();
        pc.dtls_transport_mut().dtls_role = role;
        pc.pipeline_context.sctp_handler_context = client_ctx(now, sender_ep, sender_ch, sender);
        assert_eq!(pc.sctp_transport().max_channels(), Some(2));
        for observed in [now, now + Duration::from_secs(1)] {
            pc.get_datachannel_handler()
                .handle_event(TaggedRTCEventInternal {
                    now: observed,
                    event: RTCEventInternal::SCTPHandshakeComplete(sender_ch.0),
                })
                .unwrap();
            let dc = pc.data_channels.get(&valid).unwrap();
            assert_eq!(dc.stream_id, Some(sid));
            assert!(
                dc.data_channel.is_some(),
                "role={role:?}, ordered={ordered}"
            );
            assert_eq!(dc.handshake_deadline, Some(now + Duration::from_secs(30)));
            assert_eq!(
                pc.pipeline_context
                    .stats
                    .peer_connection
                    .data_channels_closed,
                5
            );
            for id in [closed, closed_fixed, invalid, invalid_max, excess] {
                assert!(!pc.data_channels.contains(&id));
                assert_eq!(
                    pc.pipeline_context
                        .stats
                        .data_channels
                        .get(&id)
                        .unwrap()
                        .state,
                    RTCDataChannelState::Closed
                );
            }
            for (id, stream_id) in [(invalid, 2), (invalid_max, u16::MAX)] {
                assert_eq!(
                    pc.pipeline_context
                        .stats
                        .data_channels
                        .get(&id)
                        .unwrap()
                        .data_channel_identifier,
                    stream_id
                );
            }
        }
        let mut closed_ids = Vec::new();
        while let Some(message) = pc.poll_read() {
            if let RTCMessage::DataChannelClosed(id) = message.message {
                closed_ids.push(id);
            }
        }
        assert_eq!(
            closed_ids,
            vec![closed, closed_fixed, invalid, invalid_max, excess]
        );
        assert_eq!(
            pc.pipeline_context
                .stats
                .peer_connection
                .data_channels_opened,
            0
        );
        let mut other = RTCPeerConnectionBuilder::new().build(now).unwrap();
        other.dtls_transport_mut().dtls_role = if role == RTCDtlsRole::Client {
            RTCDtlsRole::Server
        } else {
            RTCDtlsRole::Client
        };
        other.pipeline_context.sctp_handler_context =
            client_ctx(now, receiver_ep, receiver_ch, receiver);
        let mut sender_output = Vec::new();
        let mut receiver_output = Vec::new();
        for tick in 0..10 {
            let observed = now + Duration::from_secs(2) + Duration::from_millis(tick * 100);
            receiver_output.extend(forward(&mut pc, &mut other, source, observed));
            sender_output.extend(forward(&mut other, &mut pc, remote, observed));
        }
        assert_eq!(
            sender_output
                .iter()
                .filter(|m| matches!(m,
                    RTCMessage::DataChannelOpened(id) if *id == valid))
                .count(),
            1
        );
        assert_eq!(
            pc.data_channel(valid).unwrap().ready_state(),
            RTCDataChannelState::Open
        );
        assert_eq!(
            pc.data_channels.get(&valid).unwrap().handshake_deadline,
            None
        );
        pc.data_channel(valid)
            .unwrap()
            .send_text(now + Duration::from_secs(3), "survives-deferred-overflow")
            .unwrap();
        for tick in 10..20 {
            let observed = now + Duration::from_secs(2) + Duration::from_millis(tick * 100);
            receiver_output.extend(forward(&mut pc, &mut other, source, observed));
            sender_output.extend(forward(&mut other, &mut pc, remote, observed));
        }
        assert_eq!(
            receiver_output
                .iter()
                .filter(|m| matches!(m,
                    RTCMessage::DataChannelMessage(_, message)
                    if message.data == b"survives-deferred-overflow"[..]))
                .count(),
            1
        );
        pc.data_channel(valid).unwrap().close().unwrap();
        for tick in 20..40 {
            let observed = now + Duration::from_secs(2) + Duration::from_millis(tick * 100);
            receiver_output.extend(forward(&mut pc, &mut other, source, observed));
            sender_output.extend(forward(&mut other, &mut pc, remote, observed));
        }
        assert!(!pc.data_channels.stream_id_in_use(&sid));
        assert_eq!(
            sender_output
                .iter()
                .filter(|m| matches!(m,
                    RTCMessage::DataChannelClosed(id) if *id == valid))
                .count(),
            1
        );
        let fresh = pc.create_data_channel("fresh", Some(options)).unwrap().id();
        assert_eq!(
            pc.data_channels.get(&fresh).unwrap().handshake_deadline,
            Some(pc.pipeline_context.now + Duration::from_secs(30)),
            "a channel created after connect must keep the DCEP timeout"
        );
        assert!(fresh > excess);
        assert_eq!(pc.data_channels.get(&fresh).unwrap().stream_id, Some(sid));
        for tick in 40..50 {
            let observed = now + Duration::from_secs(2) + Duration::from_millis(tick * 100);
            receiver_output.extend(forward(&mut pc, &mut other, source, observed));
            sender_output.extend(forward(&mut other, &mut pc, remote, observed));
        }
        assert_eq!(
            pc.data_channel(fresh).unwrap().ready_state(),
            RTCDataChannelState::Open
        );
        assert_eq!(
            pc.data_channels.get(&fresh).unwrap().handshake_deadline,
            None,
            "the paired DCEP ACK must clear the post-connected deadline"
        );
        pc.data_channel(fresh)
            .unwrap()
            .send_text(now + Duration::from_secs(7), "fresh-stream")
            .unwrap();
        for tick in 50..60 {
            let observed = now + Duration::from_secs(2) + Duration::from_millis(tick * 100);
            receiver_output.extend(forward(&mut pc, &mut other, source, observed));
            sender_output.extend(forward(&mut other, &mut pc, remote, observed));
        }
        assert_eq!(receiver_output.iter().filter(|m| matches!(m,
                    RTCMessage::DataChannelMessage(_, message) if message.data == b"fresh-stream"[..])).count(), 1);
        assert_eq!(
            pc.pipeline_context
                .stats
                .peer_connection
                .data_channels_closed,
            6
        );
        pc.get_datachannel_handler()
            .handle_event(TaggedRTCEventInternal {
                now: now + Duration::from_secs(8),
                event: RTCEventInternal::DTLSClosed,
            })
            .unwrap();
        assert_eq!(
            pc.pipeline_context
                .stats
                .peer_connection
                .data_channels_closed,
            7
        );
    }

    #[test]
    fn deferred_channel_failures_client_preserves_valid_channel_and_paired_wire_reuse() {
        for ordered in [true, false] {
            deferred_channel_failure_history(RTCDtlsRole::Client, ordered);
        }
    }

    #[test]
    fn deferred_channel_failures_server_preserves_valid_channel_and_paired_wire_reuse() {
        for ordered in [true, false] {
            deferred_channel_failure_history(RTCDtlsRole::Server, ordered);
        }
    }

    // Regression #1079: use the final SID from a real negotiated stream count.
    #[test]
    fn stream_count_boundary_channel_sends_payload_on_paired_wire() {
        use crate::data_channel::RTCDataChannelInit;
        use crate::peer_connection::RTCPeerConnectionBuilder;
        use crate::peer_connection::message::RTCMessage;
        for count in 1..=4 {
            let role = if count % 2 == 1 {
                RTCDtlsRole::Client
            } else {
                RTCDtlsRole::Server
            };
            for ordered in [true, false] {
                let now = Instant::now();
                let pair = establish_with_stream_count(count);
                let (sender_ep, sender_ch, sender, mut receiver_ep, mut receiver, source) =
                    if role == RTCDtlsRole::Client {
                        (
                            pair.client_ep,
                            pair.client_ch,
                            pair.client_conn,
                            pair.server_ep,
                            pair.server_conn,
                            client_addr(),
                        )
                    } else {
                        (
                            pair.server_ep,
                            pair.server_ch,
                            pair.server_conn,
                            pair.client_ep,
                            pair.client_conn,
                            server_addr(),
                        )
                    };
                let mut pc = RTCPeerConnectionBuilder::new().build(now).unwrap();
                pc.pipeline_context.sctp_handler_context =
                    client_ctx(now, sender_ep, sender_ch, sender);
                let negotiated = pc.sctp_transport().max_channels();
                assert_eq!(negotiated, Some(count));
                pc.dtls_transport_mut().dtls_role = role;
                let parity = if role == RTCDtlsRole::Client { 0 } else { 1 };
                let mut id = 0;
                let mut expected_opened = Vec::new();
                for sid in (parity..count).step_by(2) {
                    id = pc
                        .create_data_channel(
                            "post-connected-boundary",
                            Some(RTCDataChannelInit {
                                ordered,
                                max_retransmits: (!ordered).then_some(0),
                                negotiated: Some(sid),
                                ..Default::default()
                            }),
                        )
                        .unwrap()
                        .id();
                    expected_opened.push(id);
                }
                let sid = pc.data_channels.get(&id).unwrap().stream_id.unwrap();
                assert_eq!(sid, count - 1);
                assert!(matches!(
                    pc.create_data_channel(
                        "duplicate-boundary",
                        Some(RTCDataChannelInit {
                            negotiated: Some(sid),
                            ..Default::default()
                        })
                    ),
                    Err(Error::ErrStreamAlreadyExist)
                ));
                assert_eq!(
                    pc.data_channels.handle_of_stream(&sid),
                    Some(id),
                    "rejected duplicate must preserve the original wire route"
                );
                let mut opened = Vec::new();
                while let Some(message) = pc.poll_read() {
                    if let RTCMessage::DataChannelOpened(id) = message.message {
                        opened.push(id);
                    }
                }
                assert_eq!(
                    opened, expected_opened,
                    "post-connected negotiated channels publish open before payload"
                );
                pc.data_channel(id)
                    .unwrap()
                    .send_text(now, "boundary-gameplay")
                    .unwrap();
                while let Some(message) = pc.get_endpoint_handler().poll_write() {
                    pc.get_datachannel_handler().handle_write(message).unwrap();
                }
                while let Some(message) = pc.get_datachannel_handler().poll_write() {
                    pc.get_sctp_handler().handle_write(message).unwrap();
                }
                let mut received = Vec::new();
                while let Some(message) = pc.get_sctp_handler().poll_write() {
                    if let RTCMessageInternal::Dtls(DTLSMessage::Raw(raw)) = message.message {
                        if let Some((_, DatagramEvent::AssociationEvent(event))) =
                            receiver_ep.handle(now, source, None, raw.freeze())
                        {
                            receiver.handle_event(event);
                        }
                        if let Ok(mut stream) = receiver.stream(sid) {
                            while let Some(message) = stream.read_sctp().unwrap() {
                                received.push(message.to_payload(64).unwrap());
                            }
                        }
                    }
                }
                assert_eq!(
                    received,
                    vec![Bytes::from_static(b"boundary-gameplay")],
                    "count={count}, role={role:?}, ordered={ordered}"
                );
            }
        }
    }

    // Inspect actual reply DATA and retransmission abandonment on a remote-opened
    // stream. Public property checks alone cannot prove SCTP uses the mode.
    #[test]
    fn remote_dcep_reply_uses_wire_order_and_reliability() {
        let cases = [
            (ChannelType::Reliable, false, false),
            (ChannelType::ReliableUnordered, true, false),
            (ChannelType::PartialReliableRexmit, false, true),
            (ChannelType::PartialReliableRexmitUnordered, true, true),
            (ChannelType::PartialReliableTimed, false, true),
            (ChannelType::PartialReliableTimedUnordered, true, true),
        ];
        for (channel_type, unordered, partial) in cases {
            let now = Instant::now();
            let established = establish();
            let ch = established.client_ch;
            let mut ctx = client_ctx(now, established.client_ep, ch, established.client_conn);
            // Inbound SCTP creates the stream before DataChannelHandler sees OPEN.
            ctx.sctp_transport
                .sctp_associations
                .get_mut(&ch)
                .unwrap()
                .open_stream(1, PayloadProtocolIdentifier::Dcep)
                .unwrap();
            let wire = Message::DataChannelOpen(DataChannelOpen {
                channel_type,
                priority: CHANNEL_PRIORITY_NORMAL,
                reliability_parameter: 1,
                label: b"reply".to_vec(),
                protocol: Vec::new(),
            })
            .marshal()
            .unwrap();
            let mut dc_ctx = DataChannelHandlerContext::new(now);
            let mut channels = DataChannelRegistry::new();
            let mut stats = RTCStatsAccumulator::new();
            {
                let mut handler = DataChannelHandler::new(
                    &mut dc_ctx,
                    &mut channels,
                    &mut stats,
                    None,
                    RTCDtlsRole::Server,
                    None,
                );
                handler
                    .handle_read(TaggedRTCMessageInternal {
                        now,
                        transport: TransportContext::default(),
                        message: RTCMessageInternal::Dtls(DTLSMessage::Sctp(DataChannelMessage {
                            association_handle: ch.0,
                            stream_id: 1,
                            ppi: PayloadProtocolIdentifier::Dcep,
                            payload: BytesMut::from(&wire[..]),
                            negotiated: false,
                        })),
                    })
                    .unwrap();
                // A duplicate OPEN must only ACK the existing channel. It must
                // not replace its original delivery mode or reopen the stream.
                let duplicate = Message::DataChannelOpen(DataChannelOpen {
                    channel_type: ChannelType::Reliable,
                    priority: CHANNEL_PRIORITY_NORMAL,
                    reliability_parameter: 0,
                    label: b"replacement".to_vec(),
                    protocol: Vec::new(),
                })
                .marshal()
                .unwrap();
                handler
                    .handle_read(TaggedRTCMessageInternal {
                        now,
                        transport: TransportContext::default(),
                        message: RTCMessageInternal::Dtls(DTLSMessage::Sctp(DataChannelMessage {
                            association_handle: ch.0,
                            stream_id: 1,
                            ppi: PayloadProtocolIdentifier::Dcep,
                            payload: BytesMut::from(&duplicate[..]),
                            negotiated: false,
                        })),
                    })
                    .unwrap();
                let mut sctp = SctpHandler::new(&mut ctx, SCTP_PIPELINE_READ_BACKLOG_LIMIT);
                while let Some(msg) = handler.poll_write() {
                    // The ACK is independently reliable. Exclude it from the
                    // loss probe so its outstanding TSN cannot block FORWARD-TSN.
                    if matches!(&msg.message, RTCMessageInternal::Dtls(DTLSMessage::Sctp(message))
                        if matches!(Message::unmarshal(&mut &message.payload[..]).unwrap(), Message::DataChannelAck(_)))
                    {
                        continue;
                    }
                    sctp.handle_write(msg).unwrap();
                }
            }
            let conn = ctx.sctp_transport.sctp_associations.get_mut(&ch).unwrap();
            conn.stream(1)
                .unwrap()
                .write_sctp(
                    now,
                    &Bytes::from_static(b"reply"),
                    PayloadProtocolIdentifier::String,
                )
                .unwrap();
            let initial = drain_transmits(conn, now);
            let flags = initial
                .iter()
                .find_map(|raw| {
                    let mut offset = 12;
                    while offset + 4 <= raw.len() {
                        let length =
                            usize::from(u16::from_be_bytes([raw[offset + 2], raw[offset + 3]]));
                        if raw[offset] == 0 {
                            return Some(raw[offset + 1]);
                        }
                        offset += (length + 3) & !3;
                    }
                    None
                })
                .expect("reply DATA chunk");
            assert_eq!(flags & 4 != 0, unordered, "{channel_type:?} reply ordering");
            let mut forwarded = false;
            // Drop all reply packets. Advance only the protocol's virtual clock.
            // Partial modes must abandon; reliable modes must keep retransmitting.
            let mut tick = now;
            for _ in 0..6 {
                tick = conn.poll_timeout().expect("pending retransmission timer");
                conn.handle_timeout(tick);
                for raw in drain_transmits(conn, tick) {
                    let mut offset = 12;
                    while offset + 4 <= raw.len() {
                        let length =
                            usize::from(u16::from_be_bytes([raw[offset + 2], raw[offset + 3]]));
                        forwarded |= raw[offset] == 192;
                        offset += (length + 3) & !3;
                    }
                }
                if forwarded {
                    break;
                }
            }
            assert_eq!(
                forwarded, partial,
                "{channel_type:?} reply abandonment at {tick:?}"
            );
        }
    }

    #[test]
    fn stream_reset_pipeline_closes_old_handle_before_reopened_dcep() {
        let now = Instant::now();
        let mut pc = crate::peer_connection::RTCPeerConnectionBuilder::new()
            .build(now)
            .unwrap();
        let mut old = crate::data_channel::internal::RTCDataChannelInternal::new(
            crate::data_channel::parameters::DataChannelParameters {
                negotiated: Some(0),
                ..Default::default()
            },
        );
        old.dial(0).unwrap();
        old.outstanding_bytes = 7;
        let old_id = pc.data_channels.insert(old);
        let wire = Message::DataChannelOpen(DataChannelOpen {
            channel_type: ChannelType::Reliable,
            priority: CHANNEL_PRIORITY_NORMAL,
            reliability_parameter: 0,
            label: b"new-generation".to_vec(),
            protocol: Vec::new(),
        })
        .marshal()
        .unwrap();
        // SCTP emits these together when final reset ACK releases parked DATA.
        let ctx = &mut pc.pipeline_context.sctp_handler_context;
        ctx.read_outs.push_back(TaggedRTCMessageInternal {
            now,
            transport: TransportContext::default(),
            message: RTCMessageInternal::Dtls(DTLSMessage::Sctp(DataChannelMessage {
                association_handle: 0,
                stream_id: 0,
                ppi: PayloadProtocolIdentifier::String,
                payload: BytesMut::from(&b"final-old-payload"[..]),
                negotiated: false,
            })),
        });
        for event in [
            SctpStreamEvent::BufferReleased(0, 0, 7),
            SctpStreamEvent::BufferedAmountLow(0, 0),
            SctpStreamEvent::BufferedAmountHigh(0, 0),
        ] {
            ctx.read_outs.push_back(TaggedRTCMessageInternal {
                now,
                transport: TransportContext::default(),
                message: RTCMessageInternal::Dtls(DTLSMessage::SctpStreamEvent(event)),
            });
        }
        ctx.read_outs.push_back(TaggedRTCMessageInternal {
            now,
            transport: TransportContext::default(),
            message: RTCMessageInternal::Dtls(DTLSMessage::SctpStreamEvent(
                SctpStreamEvent::Closed(0, 0),
            )),
        });
        ctx.read_outs.push_back(TaggedRTCMessageInternal {
            now,
            transport: TransportContext::default(),
            message: RTCMessageInternal::Dtls(DTLSMessage::Sctp(DataChannelMessage {
                association_handle: 0,
                stream_id: 0,
                ppi: PayloadProtocolIdentifier::Dcep,
                payload: BytesMut::from(&wire[..]),
                negotiated: false,
            })),
        });
        ctx.read_outs.push_back(TaggedRTCMessageInternal {
            now,
            transport: TransportContext::default(),
            message: RTCMessageInternal::Dtls(DTLSMessage::Sctp(DataChannelMessage {
                association_handle: 0,
                stream_id: 0,
                ppi: PayloadProtocolIdentifier::String,
                payload: BytesMut::from(&b"new-game-payload"[..]),
                negotiated: false,
            })),
        });
        pc.handle_read(shared::TaggedBytesMut {
            now,
            transport: TransportContext::default(),
            message: BytesMut::new(),
        })
        .unwrap();
        use crate::peer_connection::event::{RTCDataChannelEvent, RTCPeerConnectionEvent};
        let new_id_before_events = pc.data_channels.handle_of_stream(&0).unwrap();
        pc.data_channels
            .get_mut(&new_id_before_events)
            .unwrap()
            .outstanding_bytes = 50;
        assert!(
            matches!(pc.poll_event(), Some(RTCPeerConnectionEvent::OnDataChannel(
            RTCDataChannelEvent::OnBufferedAmountLow(id))) if id == old_id)
        );
        assert!(
            matches!(pc.poll_event(), Some(RTCPeerConnectionEvent::OnDataChannel(
            RTCDataChannelEvent::OnBufferedAmountHigh(id))) if id == old_id)
        );
        let new_id = pc
            .data_channels
            .handle_of_stream(&0)
            .expect("new DCEP survives the old handle's completion event");
        assert_ne!(new_id, old_id);
        assert!(
            pc.poll_event().is_none(),
            "lifetime markers stay with input"
        );
        assert_eq!(pc.data_channel(new_id).unwrap().label(), "new-generation");
        assert_eq!(
            pc.data_channels.get(&new_id).unwrap().outstanding_bytes,
            50,
            "old release accounting must not decrement the new generation"
        );

        assert!(matches!(pc.poll_read().unwrap().message,
            crate::peer_connection::message::RTCMessage::DataChannelMessage(id, payload)
                if id == old_id && &payload.data[..] == b"final-old-payload"));
        assert!(matches!(pc.poll_read().unwrap().message,
            crate::peer_connection::message::RTCMessage::DataChannelClosed(id) if id == old_id));
        assert!(matches!(pc.poll_read().unwrap().message,
            crate::peer_connection::message::RTCMessage::DataChannelOpened(id) if id == new_id));
        assert!(matches!(pc.poll_read().unwrap().message,
            crate::peer_connection::message::RTCMessage::DataChannelMessage(id, payload)
                if id == new_id && &payload.data[..] == b"new-game-payload"));
        assert!(pc.poll_read().is_none(), "new payload is delivered once");
    }

    #[test]
    fn stream_reset_parked_closed_stream_clears_pending_read_without_new_input() {
        let now = Instant::now();
        let established = establish();
        let ch = established.client_ch;
        let mut ctx = client_ctx(now, established.client_ep, ch, established.client_conn);
        ctx.sctp_transport
            .sctp_associations
            .get_mut(&ch)
            .unwrap()
            .open_stream(0, PayloadProtocolIdentifier::Dcep)
            .unwrap()
            .close(now)
            .unwrap();
        ctx.pending_readable.insert((ch, 0));
        ctx.association_transport
            .insert(ch, TransportContext::default());
        let mut handler = SctpHandler::new(&mut ctx, 0);
        assert!(handler.poll_read().is_none());
        assert!(
            handler.ctx.pending_readable.is_empty(),
            "a stopped reader must not block reset event drain or retry forever"
        );
    }

    // Regression #1095: transport acknowledgments must credit only gameplay bytes.
    #[test]
    fn transport_control_ack_preserves_pending_gameplay_accounting() {
        use crate::data_channel::{
            internal::RTCDataChannelInternal, parameters::DataChannelParameters,
        };
        use crate::peer_connection::RTCPeerConnectionBuilder;
        let open = Message::DataChannelOpen(DataChannelOpen {
            channel_type: ChannelType::Reliable,
            priority: CHANNEL_PRIORITY_NORMAL,
            reliability_parameter: 0,
            label: b"gameplay".to_vec(),
            protocol: Vec::new(),
        })
        .marshal()
        .unwrap();
        for (ppi, first) in [
            (PayloadProtocolIdentifier::Dcep, open),
            (PayloadProtocolIdentifier::Dcep, BytesMut::from(&[2][..])), // DCEP ACK
            (
                PayloadProtocolIdentifier::StringEmpty,
                BytesMut::from(&[0][..]),
            ),
            (
                PayloadProtocolIdentifier::BinaryEmpty,
                BytesMut::from(&[0][..]),
            ),
            (PayloadProtocolIdentifier::Binary, BytesMut::from(&[7][..])),
        ] {
            let now = Instant::now();
            let mut pair = establish();
            let ch = pair.client_ch;
            let mut pc = RTCPeerConnectionBuilder::new().build(now).unwrap();
            pc.pipeline_context.sctp_handler_context =
                client_ctx(now, pair.client_ep, ch, pair.client_conn);
            let mut dc = RTCDataChannelInternal::new(DataChannelParameters {
                negotiated: Some(0),
                ordered: true,
                ..Default::default()
            });
            dc.dial(ch.0).unwrap();
            // The fixture installs an already-created SCTP stream below.
            while dc.data_channel.as_mut().unwrap().poll_write().is_some() {}
            let id = pc.data_channels.insert(dc);
            // DCEP and empty-message fillers do not charge RTC's gameplay
            // counter. Earlier binary gameplay provides a positive control.
            let first = first.freeze();
            pc.pipeline_context
                .sctp_handler_context
                .sctp_transport
                .sctp_associations
                .get_mut(&ch)
                .unwrap()
                .open_stream(0, ppi)
                .unwrap()
                .write_sctp(now, &first, ppi)
                .unwrap();
            if ppi == PayloadProtocolIdentifier::Binary {
                pc.data_channels.get_mut(&id).unwrap().outstanding_bytes = first.len();
            }
            let outbound = drain_transmits(
                pc.pipeline_context
                    .sctp_handler_context
                    .sctp_transport
                    .sctp_associations
                    .get_mut(&ch)
                    .unwrap(),
                now,
            );
            assert!(!outbound.is_empty());
            for raw in outbound {
                if let Some((_, DatagramEvent::AssociationEvent(event))) =
                    pair.server_ep.handle(now, client_addr(), None, raw)
                {
                    pair.server_conn.handle_event(event);
                }
            }
            // Queue gameplay before the earlier transport SACK reaches RTC.
            pc.data_channel(id)
                .unwrap()
                .send_text(now, "still-pending-gameplay")
                .unwrap();
            let later = now + Duration::from_millis(500);
            pair.server_conn.handle_timeout(later);
            let sacks = drain_transmits(&mut pair.server_conn, later);
            assert!(
                sacks.iter().any(|raw| raw[12] == CT_SACK),
                "{ppi:?}: actual transport acknowledgment"
            );
            for raw in sacks {
                pc.get_sctp_handler()
                    .handle_read(raw_read(later, raw))
                    .unwrap();
            }
            while let Some(event) = pc.get_sctp_handler().poll_read() {
                pc.get_datachannel_handler().handle_read(event).unwrap();
            }
            assert_eq!(
                pc.data_channel(id).unwrap().outstanding_bytes(),
                "still-pending-gameplay".len(),
                "{ppi:?}: only acknowledged gameplay releases its charge"
            );
            while let Some(message) = pc.get_endpoint_handler().poll_write() {
                pc.get_datachannel_handler().handle_write(message).unwrap();
            }
            while let Some(message) = pc.get_datachannel_handler().poll_write() {
                pc.get_sctp_handler().handle_write(message).unwrap();
            }
            while let Some(message) = pc.get_sctp_handler().poll_write() {
                if let RTCMessageInternal::Dtls(DTLSMessage::Raw(raw)) = message.message
                    && let Some((_, DatagramEvent::AssociationEvent(event))) = pair
                        .server_ep
                        .handle(later, client_addr(), None, raw.freeze())
                {
                    pair.server_conn.handle_event(event);
                }
            }
            let acknowledged = later + Duration::from_millis(500);
            pair.server_conn.handle_timeout(acknowledged);
            for raw in drain_transmits(&mut pair.server_conn, acknowledged) {
                pc.get_sctp_handler()
                    .handle_read(raw_read(acknowledged, raw))
                    .unwrap();
            }
            while let Some(event) = pc.get_sctp_handler().poll_read() {
                pc.get_datachannel_handler().handle_read(event).unwrap();
            }
            assert_eq!(
                pc.data_channel(id).unwrap().outstanding_bytes(),
                0,
                "{ppi:?}: gameplay acknowledgment releases the complete charge"
            );
        }
    }

    // Regression #1076: inspect the complete accepted-write -> SCTP wire boundary.
    #[test]
    fn stream_reset_accepted_send_then_close_transmits_data_before_reset() {
        use crate::data_channel::{
            internal::RTCDataChannelInternal, parameters::DataChannelParameters,
        };
        use crate::peer_connection::RTCPeerConnectionBuilder;
        for ordered in [true, false] {
            let now = Instant::now();
            let mut established = establish();
            let ch = established.client_ch;
            let mut pc = RTCPeerConnectionBuilder::new().build(now).unwrap();
            pc.pipeline_context.sctp_handler_context =
                client_ctx(now, established.client_ep, ch, established.client_conn);
            pc.pipeline_context
                .sctp_handler_context
                .sctp_transport
                .sctp_associations
                .get_mut(&ch)
                .unwrap()
                .open_stream(0, PayloadProtocolIdentifier::String)
                .unwrap()
                .set_reliability_params(
                    !ordered,
                    if ordered {
                        sctp::ReliabilityType::Reliable
                    } else {
                        sctp::ReliabilityType::Rexmit
                    },
                    0,
                )
                .unwrap();
            established
                .server_conn
                .open_stream(0, PayloadProtocolIdentifier::String)
                .unwrap();
            let mut dc = RTCDataChannelInternal::new(DataChannelParameters {
                ordered,
                max_retransmits: (!ordered).then_some(0),
                negotiated: Some(0),
                ..Default::default()
            });
            dc.dial(ch.0).unwrap();
            while dc.data_channel.as_mut().unwrap().poll_write().is_some() {}
            let id = pc.data_channels.insert(dc);
            pc.data_channel(id)
                .unwrap()
                .send_text(now, "final-before-reset")
                .unwrap();
            pc.data_channel(id).unwrap().close().unwrap();
            // Use the production handlers, retaining raw SCTP before DTLS encrypts it.
            while let Some(message) = pc.get_endpoint_handler().poll_write() {
                pc.get_datachannel_handler().handle_write(message).unwrap();
            }
            while let Some(message) = pc.get_datachannel_handler().poll_write() {
                pc.get_sctp_handler().handle_write(message).unwrap();
            }
            let mut wire = Vec::new();
            while let Some(message) = pc.get_sctp_handler().poll_write() {
                if let RTCMessageInternal::Dtls(DTLSMessage::Raw(raw)) = message.message {
                    wire.push(raw.freeze());
                }
            }
            let kinds: Vec<_> = wire.iter().map(|raw| raw[12]).collect();
            eprintln!(
                "ordered={ordered}: outstanding={}, wire_chunk_types={kinds:?}",
                pc.data_channel(id).unwrap().outstanding_bytes()
            );
            assert_eq!(kinds, vec![0, 130], "DATA must precede RECONFIG");
            assert_eq!(
                &wire[1][16..18],
                &13u16.to_be_bytes(),
                "outgoing SSN reset request"
            );
            assert_eq!(
                &wire[0][16..20],
                &wire[1][28..32],
                "reset sender_last_tsn includes accepted DATA"
            );
            let mut received = Vec::new();
            for raw in wire {
                if let Some((_, DatagramEvent::AssociationEvent(event))) =
                    established.server_ep.handle(now, client_addr(), None, raw)
                {
                    established.server_conn.handle_event(event);
                }
                if let Ok(mut stream) = established.server_conn.stream(0) {
                    while let Some(message) = stream.read_sctp().unwrap() {
                        received.push(message.to_payload(64).unwrap());
                    }
                }
            }
            assert_eq!(received, vec![Bytes::from_static(b"final-before-reset")]);
        }
    }

    // Regression #1121: a remote reset cannot overtake accepted application writes.
    #[test]
    fn incoming_stream_reset_preserves_accepted_outgoing_data() {
        use crate::data_channel::{
            internal::RTCDataChannelInternal, parameters::DataChannelParameters,
        };
        use crate::peer_connection::RTCPeerConnectionBuilder;
        use crate::peer_connection::message::internal::RTPMessage;
        for ordered in [true, false] {
            for binary in [false, true] {
                let context = format!("ordered={ordered}, binary={binary}");
                let now = Instant::now();
                let mut established = establish();
                let ch = established.client_ch;
                let mut pc = RTCPeerConnectionBuilder::new().build(now).unwrap();
                pc.pipeline_context.sctp_handler_context =
                    client_ctx(now, established.client_ep, ch, established.client_conn);
                pc.pipeline_context
                    .sctp_handler_context
                    .sctp_transport
                    .sctp_associations
                    .get_mut(&ch)
                    .unwrap()
                    .open_stream(0, PayloadProtocolIdentifier::String)
                    .unwrap()
                    .set_reliability_params(
                        !ordered,
                        if ordered {
                            sctp::ReliabilityType::Reliable
                        } else {
                            sctp::ReliabilityType::Rexmit
                        },
                        0,
                    )
                    .unwrap();
                established
                    .server_conn
                    .open_stream(0, PayloadProtocolIdentifier::String)
                    .unwrap();
                let mut dc = RTCDataChannelInternal::new(DataChannelParameters {
                    ordered,
                    max_retransmits: (!ordered).then_some(0),
                    negotiated: Some(0),
                    ..Default::default()
                });
                dc.dial(ch.0).unwrap();
                while dc.data_channel.as_mut().unwrap().poll_write().is_some() {}
                let id = pc.data_channels.insert(dc);
                /*
                   Media can await an inbound DTLS handshake. Early data-channel handoff
                   must keep both media entries and their relative order unchanged.
                */
                let media = |bytes: &'static [u8]| TaggedRTCMessageInternal {
                    now,
                    transport: TransportContext::default(),
                    message: RTCMessageInternal::Rtp(RTPMessage::Raw(BytesMut::from(bytes))),
                };
                pc.get_endpoint_handler()
                    .handle_write(media(b"media-before"))
                    .unwrap();
                /*
                   A stale unrelated driver control cannot reject a new payload
                   or leave that payload queued after a reported send failure.
                */
                pc.pipeline_context
                    .datachannel_handler_context
                    .write_outs
                    .push_back(TaggedRTCMessageInternal {
                        now,
                        transport: TransportContext::default(),
                        message: RTCMessageInternal::Dtls(DTLSMessage::SctpStreamReject {
                            association_handle: ch.0,
                            stream_id: 999,
                        }),
                    });
                let bodies = ["accepted-first", "accepted-second", "accepted-last"];
                for body in bodies {
                    if binary {
                        pc.data_channel(id)
                            .unwrap()
                            .send(now, BytesMut::from(body))
                            .unwrap();
                    } else {
                        pc.data_channel(id).unwrap().send_text(now, body).unwrap();
                    }
                }
                pc.get_endpoint_handler()
                    .handle_write(media(b"media-after"))
                    .unwrap();
                let accepted_bytes = bodies.iter().map(|body| body.len()).sum::<usize>();
                assert_eq!(
                    pc.data_channel(id).unwrap().outstanding_bytes(),
                    accepted_bytes
                );
                established
                    .server_conn
                    .stream(0)
                    .unwrap()
                    .close(now)
                    .unwrap();
                for raw in drain_transmits(&mut established.server_conn, now) {
                    // Decoded SCTP at the DTLS output seam uses the production read pump.
                    pc.pipeline_context
                        .dtls_handler_context
                        .read_outs
                        .push_back(raw_read(now, raw));
                    pc.pump_reads(None);
                }
                let stats = &pc.pipeline_context.stats.data_channels[&id];
                let sent_stats = (stats.messages_sent, stats.bytes_sent);
                assert_eq!(
                    sent_stats,
                    (3, u64::try_from(accepted_bytes).unwrap()),
                    "{context}: successful admission owns exact sent credit"
                );
                let rejected = if binary {
                    pc.data_channel(id)
                        .unwrap()
                        .send(now, BytesMut::from("rejected-after-reset"))
                } else {
                    pc.data_channel(id)
                        .unwrap()
                        .send_text(now, "rejected-after-reset")
                };
                assert_eq!(
                    rejected,
                    Err(Error::ErrDataChannelClosed),
                    "{context}: reject before enqueue"
                );
                assert_eq!(
                    pc.data_channel(id).unwrap().outstanding_bytes(),
                    accepted_bytes,
                    "{context}: rejection adds no byte charge"
                );
                let stats = &pc.pipeline_context.stats.data_channels[&id];
                assert_eq!(
                    (stats.messages_sent, stats.bytes_sent),
                    sent_stats,
                    "{context}: rejected writes add no sent statistics"
                );
                assert!(
                    pc.pipeline_context
                        .datachannel_handler_context
                        .write_outs
                        .is_empty(),
                    "{context}: no rejected payload remains for a later flush"
                );
                let media = &pc.pipeline_context.endpoint_handler_context.write_outs;
                assert_eq!(media.len(), 2, "{context}: retain media awaiting keys");
                for (message, expected) in media
                    .iter()
                    .zip([b"media-before".as_slice(), b"media-after"])
                {
                    assert!(
                        matches!(&message.message, RTCMessageInternal::Rtp(RTPMessage::Raw(raw)) if raw.as_ref() == expected),
                        "{context}: media FIFO"
                    );
                }
                let mut sent = Vec::new();
                let mut last_data_tsn = None;
                let mut reset_boundary = None;
                let before_datas = established.server_conn.stats().get_num_datas();
                /*
                   No packet loss is injected. Drive both real SCTP directions through
                   reciprocal completion, without a wall-clock or application ACK shortcut.
                */
                for _ in 0..8 {
                    while let Some(message) = pc.get_sctp_handler().poll_write() {
                        if let RTCMessageInternal::Dtls(DTLSMessage::Raw(raw)) = message.message {
                            let mut offset = 12;
                            while offset + 4 <= raw.len() {
                                let length = usize::from(u16::from_be_bytes([
                                    raw[offset + 2],
                                    raw[offset + 3],
                                ]));
                                if raw[offset] == 0 {
                                    assert!(
                                        reset_boundary.is_none(),
                                        "{context}: DATA precedes reset"
                                    );
                                    last_data_tsn = Some(u32::from_be_bytes(
                                        raw[offset + 4..offset + 8].try_into().unwrap(),
                                    ));
                                    assert_eq!(
                                        u32::from_be_bytes(
                                            raw[offset + 12..offset + 16].try_into().unwrap()
                                        ),
                                        if binary { 53 } else { 51 },
                                        "{context}: preserve wire type"
                                    );
                                    sent.push(Bytes::copy_from_slice(
                                        &raw[offset + 16..offset + length],
                                    ));
                                } else if raw[offset] == 130
                                    && raw[offset + 4..offset + 6] == 13u16.to_be_bytes()
                                {
                                    reset_boundary = Some(u32::from_be_bytes(
                                        raw[offset + 16..offset + 20].try_into().unwrap(),
                                    ));
                                }
                                offset += length.next_multiple_of(4);
                            }
                            if let Some((_, DatagramEvent::AssociationEvent(event))) = established
                                .server_ep
                                .handle(now, client_addr(), None, raw.freeze())
                            {
                                established.server_conn.handle_event(event);
                            }
                        }
                    }
                    for raw in drain_transmits(&mut established.server_conn, now) {
                        pc.pipeline_context
                            .dtls_handler_context
                            .read_outs
                            .push_back(raw_read(now, raw));
                        pc.pump_reads(None);
                    }
                }
                assert_eq!(
                    sent,
                    bodies.map(|body| Bytes::copy_from_slice(body.as_bytes())),
                    "{context}: full bodies exactly once in accepted order"
                );
                assert_eq!(
                    reset_boundary, last_data_tsn,
                    "{context}: reset accounts for all accepted TSNs"
                );
                assert_eq!(
                    established.server_conn.stats().get_num_datas() - before_datas,
                    3,
                    "{context}: peer ingests all DATA before reset completes"
                );
                assert!(
                    pc.data_channel(id).is_none(),
                    "{context}: local reset completes"
                );
                assert!(
                    established.server_conn.stream(0).is_err(),
                    "{context}: peer reset completes"
                );
            }
        }
    }

    // Regression #872: a local close must not release its SID while reset
    // packets are withheld. Drive the real SCTP association without clocks or IO.
    #[test]
    fn stream_reset_local_close_keeps_stream_reserved_until_reset_completes() {
        let now = Instant::now();
        let mut established = establish();
        let ch = established.client_ch;
        let mut ctx = client_ctx(now, established.client_ep, ch, established.client_conn);
        ctx.sctp_transport
            .sctp_associations
            .get_mut(&ch)
            .unwrap()
            .open_stream(0, PayloadProtocolIdentifier::Dcep)
            .unwrap();
        established
            .server_conn
            .open_stream(0, PayloadProtocolIdentifier::Dcep)
            .unwrap();
        let mut channels = DataChannelRegistry::new();
        let id = channels.insert(crate::data_channel::internal::RTCDataChannelInternal::new(
            crate::data_channel::parameters::DataChannelParameters {
                label: "retiring".into(),
                negotiated: Some(0),
                ..Default::default()
            },
        ));
        let wire = Message::DataChannelClose(
            datachannel::message::message_channel_close::DataChannelClose,
        )
        .marshal()
        .unwrap();
        let mut sctp = SctpHandler::new(&mut ctx, 0);
        sctp.handle_write(TaggedRTCMessageInternal {
            now,
            transport: TransportContext::default(),
            message: RTCMessageInternal::Dtls(DTLSMessage::Sctp(DataChannelMessage {
                association_handle: ch.0,
                stream_id: 0,
                ppi: PayloadProtocolIdentifier::Dcep,
                payload: BytesMut::from(&wire[..]),
                negotiated: false,
            })),
        })
        .unwrap();
        assert!(
            sctp.poll_event().is_none(),
            "close is only a reset request, not completion"
        );
        assert!(
            sctp.poll_read().is_none(),
            "close request must not emit a completion marker"
        );
        assert!(channels.stream_id_in_use(&0));
        let pending = channels.insert(crate::data_channel::internal::RTCDataChannelInternal::new(
            crate::data_channel::parameters::DataChannelParameters::default(),
        ));
        channels
            .assign_stream_ids(RTCDtlsRole::Client, None)
            .unwrap();
        assert_eq!(channels.get(&pending).unwrap().stream_id, Some(2));
        let mut dc_ctx = DataChannelHandlerContext::new(now);
        let mut stats = RTCStatsAccumulator::new();
        let mut closed = 0;
        for _ in 0..16 {
            let mut sctp = SctpHandler::new(&mut ctx, 0);
            while let Some(message) = sctp.poll_write() {
                if let RTCMessageInternal::Dtls(DTLSMessage::Raw(raw)) = message.message
                    && let Some((_, DatagramEvent::AssociationEvent(event))) = established
                        .server_ep
                        .handle(now, client_addr(), None, raw.freeze())
                {
                    established.server_conn.handle_event(event);
                }
            }
            for raw in drain_transmits(&mut established.server_conn, now) {
                sctp.handle_read(raw_read(now, raw)).unwrap();
            }
            while let Some(message) = sctp.poll_read() {
                if matches!(
                    message.message,
                    RTCMessageInternal::Dtls(DTLSMessage::SctpStreamEvent(
                        SctpStreamEvent::Closed(_, 0)
                    ))
                ) {
                    closed += 1;
                }
                DataChannelHandler::new(
                    &mut dc_ctx,
                    &mut channels,
                    &mut stats,
                    None,
                    RTCDtlsRole::Client,
                    None,
                )
                .handle_read(message)
                .unwrap();
            }
            while let Some(event) = sctp.poll_event() {
                if matches!(event.event, RTCEventInternal::SCTPStreamClosed(_, 0)) {
                    closed += 1;
                }
                DataChannelHandler::new(
                    &mut dc_ctx,
                    &mut channels,
                    &mut stats,
                    None,
                    RTCDtlsRole::Client,
                    None,
                )
                .handle_event(event)
                .unwrap();
            }
        }
        assert_eq!(closed, 1, "completed reset closes the old handle once");
        assert!(!channels.contains(&id));
        let replacement =
            channels.insert(crate::data_channel::internal::RTCDataChannelInternal::new(
                crate::data_channel::parameters::DataChannelParameters::default(),
            ));
        channels
            .assign_stream_ids(RTCDtlsRole::Client, None)
            .unwrap();
        assert_eq!(
            channels.get(&replacement).unwrap().stream_id,
            Some(0),
            "completed SID is reusable"
        );
        let conn = ctx.sctp_transport.sctp_associations.get_mut(&ch).unwrap();
        conn.open_stream(0, PayloadProtocolIdentifier::String)
            .unwrap()
            .write_sctp(
                now,
                &Bytes::from_static(b"reopened"),
                PayloadProtocolIdentifier::String,
            )
            .unwrap();
        for raw in drain_transmits(conn, now) {
            if let Some((_, DatagramEvent::AssociationEvent(event))) =
                established.server_ep.handle(now, client_addr(), None, raw)
            {
                established.server_conn.handle_event(event);
            }
        }
        let chunks = established
            .server_conn
            .stream(0)
            .unwrap()
            .read_sctp()
            .unwrap()
            .unwrap();
        assert_eq!(&chunks.to_payload(64).unwrap()[..], b"reopened");
    }

    #[test]
    fn remote_dcep_rejected_stream_resets_without_ack_and_can_be_reused() {
        for channel_type in [
            ChannelType::PartialReliableRexmit,
            ChannelType::PartialReliableRexmitUnordered,
            ChannelType::PartialReliableTimed,
            ChannelType::PartialReliableTimedUnordered,
        ] {
            for parameter in [u32::from(u16::MAX) + 1, u32::MAX] {
                let now = Instant::now();
                let mut established = establish();
                let ch = established.client_ch;
                let mut ctx = client_ctx(now, established.client_ep, ch, established.client_conn);
                ctx.sctp_transport
                    .sctp_associations
                    .get_mut(&ch)
                    .unwrap()
                    .open_stream(1, PayloadProtocolIdentifier::Dcep)
                    .unwrap();
                established
                    .server_conn
                    .open_stream(1, PayloadProtocolIdentifier::Dcep)
                    .unwrap();
                let mut pc = crate::peer_connection::RTCPeerConnectionBuilder::new()
                    .build(now)
                    .unwrap();
                let default_sctp =
                    std::mem::replace(&mut pc.pipeline_context.sctp_handler_context, ctx);
                // Stage decrypted SCTP output at its normal pipeline seam. This
                // isolates the refusal from ICE/DTLS while using the public read
                // entrypoint and the actual PeerConnection handler pump.
                for (stream_id, label, mode) in [
                    (2, "reliable", ChannelType::Reliable),
                    (4, "unreliable", ChannelType::PartialReliableRexmitUnordered),
                ] {
                    pc.pipeline_context
                        .sctp_handler_context
                        .sctp_transport
                        .sctp_associations
                        .get_mut(&ch)
                        .unwrap()
                        .open_stream(stream_id, PayloadProtocolIdentifier::Dcep)
                        .unwrap();
                    established
                        .server_conn
                        .open_stream(stream_id, PayloadProtocolIdentifier::Dcep)
                        .unwrap();
                    let wire = Message::DataChannelOpen(DataChannelOpen {
                        channel_type: mode,
                        priority: CHANNEL_PRIORITY_NORMAL,
                        reliability_parameter: 0,
                        label: label.as_bytes().to_vec(),
                        protocol: Vec::new(),
                    })
                    .marshal()
                    .unwrap();
                    pc.pipeline_context
                        .sctp_handler_context
                        .read_outs
                        .push_back(TaggedRTCMessageInternal {
                            now,
                            transport: TransportContext::default(),
                            message: RTCMessageInternal::Dtls(DTLSMessage::Sctp(
                                DataChannelMessage {
                                    association_handle: ch.0,
                                    stream_id,
                                    ppi: PayloadProtocolIdentifier::Dcep,
                                    payload: BytesMut::from(&wire[..]),
                                    negotiated: false,
                                },
                            )),
                        });
                }
                let wire = Message::DataChannelOpen(DataChannelOpen {
                    channel_type,
                    priority: CHANNEL_PRIORITY_NORMAL,
                    reliability_parameter: parameter,
                    label: b"invalid".to_vec(),
                    protocol: Vec::new(),
                })
                .marshal()
                .unwrap();
                for _ in 0..2 {
                    pc.pipeline_context
                        .sctp_handler_context
                        .read_outs
                        .push_back(TaggedRTCMessageInternal {
                            now,
                            transport: TransportContext::default(),
                            message: RTCMessageInternal::Dtls(DTLSMessage::Sctp(
                                DataChannelMessage {
                                    association_handle: ch.0,
                                    stream_id: 1,
                                    ppi: PayloadProtocolIdentifier::Dcep,
                                    payload: BytesMut::from(&wire[..]),
                                    negotiated: false,
                                },
                            )),
                        });
                }
                // The handler-level refusal is logged and contained by pump_reads.
                // It must never propagate to the public PeerConnection caller.
                pc.handle_read(shared::TaggedBytesMut {
                    now,
                    transport: TransportContext::default(),
                    message: BytesMut::new(),
                })
                .expect("stream refusal must not fail the PeerConnection caller");
                for stream_id in [2, 4] {
                    let id = pc.data_channels.handle_of_stream(&stream_id).unwrap();
                    assert_eq!(
                        pc.data_channel(id).unwrap().ready_state(),
                        crate::data_channel::RTCDataChannelState::Open
                    );
                }
                assert!(pc.data_channels.handle_of_stream(&1).is_none());
                let mut ctx =
                    std::mem::replace(&mut pc.pipeline_context.sctp_handler_context, default_sctp);
                let mut dc_ctx = std::mem::replace(
                    &mut pc.pipeline_context.datachannel_handler_context,
                    DataChannelHandlerContext::new(now),
                );
                let mut channels = std::mem::take(&mut pc.data_channels);
                let mut stats = RTCStatsAccumulator::new();
                {
                    let mut handler = DataChannelHandler::new(
                        &mut dc_ctx,
                        &mut channels,
                        &mut stats,
                        None,
                        RTCDtlsRole::Server,
                        None,
                    );
                    let mut sctp = SctpHandler::new(&mut ctx, 0);
                    let mut resets = 0;
                    while let Some(message) = handler.poll_write() {
                        assert!(
                            !matches!(&message.message,
                            RTCMessageInternal::Dtls(DTLSMessage::Sctp(channel)) if channel.stream_id == 1),
                            "refused stream must send no DCEP ACK or payload"
                        );
                        if matches!(
                            &message.message,
                            RTCMessageInternal::Dtls(DTLSMessage::SctpStreamReject { .. })
                        ) {
                            resets += 1;
                        }
                        sctp.handle_write(message).unwrap();
                    }
                    assert_eq!(resets, 1, "one reset and no invalid-channel ACK");
                }
                assert!(channels.handle_of_stream(&1).is_none());
                assert!(
                    !ctx.sctp_transport
                        .sctp_associations
                        .get_mut(&ch)
                        .unwrap()
                        .stream(1)
                        .unwrap()
                        .is_writable(),
                    "rejected stream stops sending"
                );
                let mut completed = false;
                for _ in 0..8 {
                    let mut sctp = SctpHandler::new(&mut ctx, 0);
                    while let Some(msg) = sctp.poll_write() {
                        if let RTCMessageInternal::Dtls(DTLSMessage::Raw(raw)) = msg.message
                            && let Some((peer_ch, DatagramEvent::AssociationEvent(event))) =
                                established
                                    .server_ep
                                    .handle(now, client_addr(), None, raw.freeze())
                        {
                            assert_eq!(peer_ch, established.server_ch);
                            established.server_conn.handle_event(event);
                        }
                    }
                    for raw in drain_transmits(&mut established.server_conn, now) {
                        sctp.handle_read(raw_read(now, raw)).unwrap();
                    }
                    while let Some(message) = sctp.poll_read() {
                        if matches!(
                            message.message,
                            RTCMessageInternal::Dtls(DTLSMessage::SctpStreamEvent(
                                SctpStreamEvent::Closed(_, 1)
                            ))
                        ) {
                            completed = true;
                        }
                        DataChannelHandler::new(
                            &mut dc_ctx,
                            &mut channels,
                            &mut stats,
                            None,
                            RTCDtlsRole::Server,
                            None,
                        )
                        .handle_read(message)
                        .unwrap();
                    }
                    while let Some(event) = sctp.poll_event() {
                        if matches!(&event.event, RTCEventInternal::SCTPStreamClosed(_, 1)) {
                            completed = true;
                        }
                        let mut handler = DataChannelHandler::new(
                            &mut dc_ctx,
                            &mut channels,
                            &mut stats,
                            None,
                            RTCDtlsRole::Server,
                            None,
                        );
                        handler.handle_event(event).unwrap();
                    }
                    if completed {
                        break;
                    }
                }
                assert!(
                    completed,
                    "reset completes for {channel_type:?} parameter {parameter}"
                );
                // Both existing required streams must still carry actual SCTP
                // messages in both directions after the unrelated stream reset.
                for stream_id in [2, 4] {
                    let conn = ctx.sctp_transport.sctp_associations.get_mut(&ch).unwrap();
                    conn.stream(stream_id)
                        .unwrap()
                        .write_sctp(
                            now,
                            &Bytes::from_static(b"healthy-out"),
                            PayloadProtocolIdentifier::String,
                        )
                        .unwrap();
                    for raw in drain_transmits(conn, now) {
                        if let Some((_, DatagramEvent::AssociationEvent(event))) =
                            established.server_ep.handle(now, client_addr(), None, raw)
                        {
                            established.server_conn.handle_event(event);
                        }
                    }
                    // Consume the reliable DCEP ACK, then the game payload.
                    let peer = &mut established.server_conn;
                    loop {
                        let chunks = peer
                            .stream(stream_id)
                            .unwrap()
                            .read_sctp()
                            .unwrap()
                            .unwrap();
                        if chunks.ppi == PayloadProtocolIdentifier::String {
                            assert_eq!(&chunks.to_payload(64).unwrap()[..], b"healthy-out");
                            break;
                        }
                    }
                    peer.stream(stream_id)
                        .unwrap()
                        .write_sctp(
                            now,
                            &Bytes::from_static(b"healthy-in"),
                            PayloadProtocolIdentifier::String,
                        )
                        .unwrap();
                    let mut sctp = SctpHandler::new(&mut ctx, 0);
                    let packets = drain_transmits(peer, now);
                    for raw in packets {
                        sctp.handle_read(raw_read(now, raw)).unwrap();
                    }
                    let inbound = loop {
                        let inbound = sctp.poll_read().expect("healthy inbound message");
                        if matches!(
                            inbound.message,
                            RTCMessageInternal::Dtls(DTLSMessage::SctpStreamEvent(_))
                        ) {
                            DataChannelHandler::new(
                                &mut dc_ctx,
                                &mut channels,
                                &mut stats,
                                None,
                                RTCDtlsRole::Server,
                                None,
                            )
                            .handle_read(inbound)
                            .unwrap();
                        } else {
                            break inbound;
                        }
                    };
                    assert!(
                        matches!(&inbound.message, RTCMessageInternal::Dtls(DTLSMessage::Sctp(message))
                        if message.stream_id == stream_id && &message.payload[..] == b"healthy-in")
                    );
                    let expected_id = channels.handle_of_stream(&stream_id).unwrap();
                    let mut handler = DataChannelHandler::new(
                        &mut dc_ctx,
                        &mut channels,
                        &mut stats,
                        None,
                        RTCDtlsRole::Server,
                        None,
                    );
                    handler.handle_read(inbound).unwrap();
                    loop {
                        let inbound = handler.poll_read().expect("healthy application message");
                        if let RTCMessageInternal::Dtls(DTLSMessage::DataChannel(
                            crate::peer_connection::message::internal::ApplicationMessage {
                                data_channel_id,
                                data_channel_event: DataChannelEvent::Message(payload),
                            },
                        )) = inbound.message
                        {
                            assert_eq!(data_channel_id, expected_id);
                            assert!(payload.is_string);
                            assert_eq!(&payload.data[..], b"healthy-in");
                            break;
                        }
                    }
                }
                ctx.sctp_transport
                    .sctp_associations
                    .get_mut(&ch)
                    .unwrap()
                    .open_stream(1, PayloadProtocolIdentifier::Dcep)
                    .expect("reset stream id can be reused");
                let valid = Message::DataChannelOpen(DataChannelOpen {
                    channel_type: ChannelType::Reliable,
                    priority: CHANNEL_PRIORITY_NORMAL,
                    reliability_parameter: 0,
                    label: b"valid".to_vec(),
                    protocol: Vec::new(),
                })
                .marshal()
                .unwrap();
                let mut handler = DataChannelHandler::new(
                    &mut dc_ctx,
                    &mut channels,
                    &mut stats,
                    None,
                    RTCDtlsRole::Server,
                    None,
                );
                handler
                    .handle_read(TaggedRTCMessageInternal {
                        now,
                        transport: TransportContext::default(),
                        message: RTCMessageInternal::Dtls(DTLSMessage::Sctp(DataChannelMessage {
                            association_handle: ch.0,
                            stream_id: 1,
                            ppi: PayloadProtocolIdentifier::Dcep,
                            payload: BytesMut::from(&valid[..]),
                            negotiated: false,
                        })),
                    })
                    .expect("reused stream admits a valid new channel");
                assert!(handler.poll_read().is_some());
            }
        }
    }

    /// Build a client association that has sent COOKIE-ECHO but has not yet received COOKIE-ACK,
    /// then create one DATA datagram from the already-established server.  This is the ordering
    /// that can occur when a peer opens a DataChannel in the same flight as the final SCTP
    /// handshake packet.
    fn client_with_data_before_cookie_ack() -> (SctpHandlerContext, Vec<Bytes>, Vec<Bytes>) {
        let now = Instant::now();

        let mut client_ep = Endpoint::new(
            client_addr(),
            TransportProtocol::UDP,
            EndpointConfig::default().into(),
            None,
        );
        let mut server_ep = Endpoint::new(
            server_addr(),
            TransportProtocol::UDP,
            EndpointConfig::default().into(),
            Some(ServerConfig::new(TransportConfig::default()).into()),
        );

        let (client_ch, mut client_conn) = client_ep
            .connect(
                now,
                ClientConfig::new(TransportConfig::default()),
                server_addr(),
            )
            .expect("client connect");

        // Client INIT -> server association.
        let init = drain_transmits(&mut client_conn, now);
        let (server_ch, mut server_conn) = init
            .into_iter()
            .find_map(|dgram| {
                server_ep.handle(now, client_addr(), None, dgram).and_then(
                    |(ch, event)| match event {
                        DatagramEvent::NewAssociation(conn) => Some((ch, conn)),
                        _ => None,
                    },
                )
            })
            .expect("server association");

        // Server INIT-ACK -> client COOKIE-ECHO.
        let init_ack = drain_transmits(&mut server_conn, now);
        for dgram in init_ack {
            if let Some((ch, DatagramEvent::AssociationEvent(event))) =
                client_ep.handle(now, server_addr(), None, dgram)
            {
                assert_eq!(ch, client_ch);
                client_conn.handle_event(event);
            }
        }
        let cookie_echo = drain_transmits(&mut client_conn, now);

        // Server processes COOKIE-ECHO and becomes established, producing COOKIE-ACK.
        for dgram in cookie_echo {
            if let Some((ch, DatagramEvent::AssociationEvent(event))) =
                server_ep.handle(now, client_addr(), None, dgram)
            {
                assert_eq!(ch, server_ch);
                server_conn.handle_event(event);
            }
        }
        while server_conn.poll().is_some() {}
        let cookie_ack = drain_transmits(&mut server_conn, now);
        assert!(!cookie_ack.is_empty(), "server must produce COOKIE-ACK");

        // Send a DCEP OPEN before the client receives COOKIE-ACK.
        let dcep_open = Message::DataChannelOpen(DataChannelOpen {
            channel_type: ChannelType::Reliable,
            priority: CHANNEL_PRIORITY_NORMAL,
            reliability_parameter: 0,
            label: b"early-channel".to_vec(),
            protocol: Vec::new(),
        })
        .marshal()
        .expect("DCEP OPEN");
        {
            let mut stream = server_conn
                .open_stream(1, PayloadProtocolIdentifier::Dcep)
                .expect("open server stream");
            stream
                .write_sctp(
                    now,
                    &Bytes::from(dcep_open),
                    PayloadProtocolIdentifier::Dcep,
                )
                .expect("server data");
        }
        let early_data = drain_transmits(&mut server_conn, now);
        assert!(!early_data.is_empty(), "server must produce DCEP DATA");

        (
            client_ctx(now, client_ep, client_ch, client_conn),
            cookie_ack,
            early_data,
        )
    }

    /// Establish TWO independent associations on a single client endpoint (each to
    /// its own server), so a handler test can drain one while the other stays live.
    fn establish_two() -> (
        Endpoint,
        AssociationHandle,
        Association,
        AssociationHandle,
        Association,
    ) {
        let now = Instant::now();
        let server_a_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5555);
        let server_b_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6666);

        let mut client_ep = Endpoint::new(
            client_addr(),
            TransportProtocol::UDP,
            EndpointConfig::default().into(),
            None,
        );
        let mut server_a = Endpoint::new(
            server_a_addr,
            TransportProtocol::UDP,
            EndpointConfig::default().into(),
            Some(ServerConfig::new(TransportConfig::default()).into()),
        );
        let mut server_b = Endpoint::new(
            server_b_addr,
            TransportProtocol::UDP,
            EndpointConfig::default().into(),
            Some(ServerConfig::new(TransportConfig::default()).into()),
        );

        let (ch_a, mut conn_a) = client_ep
            .connect(
                now,
                ClientConfig::new(TransportConfig::default()),
                server_a_addr,
            )
            .expect("connect A");
        let (ch_b, mut conn_b) = client_ep
            .connect(
                now,
                ClientConfig::new(TransportConfig::default()),
                server_b_addr,
            )
            .expect("connect B");

        let mut sa: HashMap<AssociationHandle, Association> = HashMap::new();
        let mut sb: HashMap<AssociationHandle, Association> = HashMap::new();
        let mut a_up = false;
        let mut b_up = false;

        for _ in 0..100 {
            let mut moved = false;

            // client A -> server A, client B -> server B
            for d in drain_transmits(&mut conn_a, now) {
                moved = true;
                if let Some((ch, ev)) = server_a.handle(now, client_addr(), None, d) {
                    match ev {
                        DatagramEvent::NewAssociation(c) => {
                            sa.insert(ch, c);
                        }
                        DatagramEvent::AssociationEvent(e) => {
                            if let Some(c) = sa.get_mut(&ch) {
                                c.handle_event(e);
                            }
                        }
                        _ => {}
                    }
                }
            }
            for d in drain_transmits(&mut conn_b, now) {
                moved = true;
                if let Some((ch, ev)) = server_b.handle(now, client_addr(), None, d) {
                    match ev {
                        DatagramEvent::NewAssociation(c) => {
                            sb.insert(ch, c);
                        }
                        DatagramEvent::AssociationEvent(e) => {
                            if let Some(c) = sb.get_mut(&ch) {
                                c.handle_event(e);
                            }
                        }
                        _ => {}
                    }
                }
            }

            // servers -> client (client_ep routes back by verification tag)
            let mut back = vec![];
            for c in sa.values_mut() {
                while c.poll().is_some() {}
                for d in drain_transmits(c, now) {
                    back.push((server_a_addr, d));
                }
            }
            for c in sb.values_mut() {
                while c.poll().is_some() {}
                for d in drain_transmits(c, now) {
                    back.push((server_b_addr, d));
                }
            }
            for (from, d) in back {
                moved = true;
                if let Some((ch, DatagramEvent::AssociationEvent(e))) =
                    client_ep.handle(now, from, None, d)
                {
                    if ch == ch_a {
                        conn_a.handle_event(e);
                    } else if ch == ch_b {
                        conn_b.handle_event(e);
                    }
                }
            }

            while let Some(ev) = conn_a.poll() {
                if let Event::Connected = ev {
                    a_up = true;
                }
            }
            while let Some(ev) = conn_b.poll() {
                if let Event::Connected = ev {
                    b_up = true;
                }
            }

            if a_up && b_up && !moved {
                break;
            }
        }

        assert!(a_up && b_up, "both associations must connect");
        (client_ep, ch_a, conn_a, ch_b, conn_b)
    }

    /// Wrap an established client endpoint + association in a handler context.
    fn client_ctx(
        now: Instant,
        client_ep: Endpoint,
        ch: AssociationHandle,
        conn: Association,
    ) -> SctpHandlerContext {
        let mut transport = SctpTransport::new(
            SctpMaxMessageSize::default(),
            None,
            None,
            test_transport_id(TransportKind::Sctp),
            test_transport_id(TransportKind::Dtls),
        );
        transport
            .internal_buffer
            .resize(SctpMaxMessageSize::DEFAULT_MESSAGE_SIZE as usize, 0);
        transport.sctp_endpoint = Some(client_ep);
        transport.sctp_associations.insert(ch, conn);
        SctpHandlerContext::new(now, transport)
    }

    // First-chunk types we assert on (RFC 9260 §3.2). The `sctp` crate keeps its
    // `CT_*` constants crate-private, so we decode the wire format directly.
    const CT_PAYLOAD_DATA: u8 = 0;
    const CT_SHUTDOWN: u8 = 7;
    const CT_SHUTDOWN_COMPLETE: u8 = 14;

    /// First SCTP chunk type of every datagram flushed into `write_outs`. The SCTP
    /// common header is a fixed 12 bytes (RFC 9260 §3.1), so the first chunk's type
    /// field is byte 12.
    fn flushed_chunk_types(ctx: &SctpHandlerContext) -> Vec<u8> {
        ctx.write_outs
            .iter()
            .filter_map(|m| match &m.message {
                RTCMessageInternal::Dtls(DTLSMessage::Raw(raw)) if raw.len() > 12 => Some(raw[12]),
                _ => None,
            })
            .collect()
    }

    const CT_SACK: u8 = 3;

    /// The advertised receiver window (`a_rwnd`) from the first flushed SACK.
    ///
    /// Asserting on the wire rather than on the association's internal credit: `a_rwnd` is
    /// what the peer actually reads, and `get_my_receiver_window_credit()` is `pub(crate)` to
    /// the `sctp` crate anyway. Layout is RFC 9260 §3.3.4 — 12-byte common header, then the
    /// 4-byte chunk header, then a 4-byte Cumulative TSN Ack, then `a_rwnd`.
    fn flushed_sack_a_rwnd(flushed: &[TaggedRTCMessageInternal]) -> Option<u32> {
        flushed.iter().find_map(|m| match &m.message {
            RTCMessageInternal::Dtls(DTLSMessage::Raw(raw))
                if raw.len() >= 24 && raw[12] == CT_SACK =>
            {
                Some(u32::from_be_bytes([raw[20], raw[21], raw[22], raw[23]]))
            }
            _ => None,
        })
    }

    /// Feed `dgrams` to a client handler with the given downstream backlog, flush, and return
    /// the resulting context.
    fn run_client(
        now: Instant,
        e: Established,
        dgrams: Vec<Bytes>,
        backlog: usize,
    ) -> (SctpHandlerContext, Vec<TaggedRTCMessageInternal>) {
        let mut ctx = client_ctx(now, e.client_ep, e.client_ch, e.client_conn);
        let mut flushed = vec![];
        let mut handler = SctpHandler::new(&mut ctx, backlog);
        for dgram in dgrams {
            handler.handle_read(raw_read(now, dgram)).expect("read");
        }
        // The first DATA arms a *delayed* ack, so without letting that timer expire there is
        // no SACK to read `a_rwnd` from. Harmless in the parked case: the backlog is still
        // over the bound there, so the resume finds no budget and changes nothing.
        handler
            .handle_timeout(now + Duration::from_millis(500))
            .expect("delayed-ack timer");
        // `poll_write` *pops*: collect, or the evidence is thrown away.
        while let Some(m) = handler.poll_write() {
            flushed.push(m);
        }
        (ctx, flushed)
    }

    /// Push `count` messages from the server onto `stream_id` and return the datagrams the
    /// client must receive. Returns raw wire bytes so the test drives the real
    /// `handle_read` path rather than poking handler internals.
    fn server_datagrams_carrying(
        e: &mut Established,
        stream_id: StreamId,
        count: usize,
        now: Instant,
    ) -> Vec<Bytes> {
        {
            let mut stream = e
                .server_conn
                .open_stream(stream_id, PayloadProtocolIdentifier::Binary)
                .expect("open server stream");
            for i in 0..count {
                stream
                    .write_sctp(
                        now,
                        &Bytes::from(vec![i as u8; 64]),
                        PayloadProtocolIdentifier::Binary,
                    )
                    .expect("server write");
            }
        }
        while e.server_conn.poll().is_some() {}
        drain_transmits(&mut e.server_conn, now)
    }

    fn raw_read(now: Instant, dgram: Bytes) -> TaggedRTCMessageInternal {
        TaggedRTCMessageInternal {
            now,
            transport: TransportContext {
                local_addr: client_addr(),
                peer_addr: server_addr(),
                ecn: None,
                transport_protocol: TransportProtocol::UDP,
            },
            message: RTCMessageInternal::Dtls(DTLSMessage::Raw(BytesMut::from(&dgram[..]))),
        }
    }

    /// An SCTP DATA message received before COOKIE-ACK must stay in the association's receive
    /// queue.  Once COOKIE-ACK establishes the association, the same message is delivered exactly
    /// once, allowing the DataChannel handler to announce `OnOpen` only after SCTP is ready.
    #[test]
    fn inbound_data_waits_for_sctp_handshake() {
        let now = Instant::now();
        let (mut ctx, cookie_ack, early_data) = client_with_data_before_cookie_ack();

        {
            let mut handler = SctpHandler::new(&mut ctx, 0);
            for dgram in early_data {
                handler
                    .handle_read(raw_read(now, dgram))
                    .expect("early DATA");
            }
        }

        assert!(
            ctx.read_outs.is_empty(),
            "handshaking SCTP data must not reach the DataChannel handler"
        );
        assert_eq!(
            ctx.pending_readable.len(),
            1,
            "the readable stream must be retried after SCTP connects"
        );

        {
            let mut handler = SctpHandler::new(&mut ctx, 0);
            for dgram in cookie_ack {
                handler
                    .handle_read(raw_read(now, dgram))
                    .expect("COOKIE-ACK");
            }
        }

        assert!(
            ctx.pending_readable.is_empty(),
            "the stream must be drained after SCTP becomes established"
        );
        assert_eq!(ctx.read_outs.len(), 1, "early DCEP must be delivered once");
        let inbound = ctx.read_outs.pop_front().expect("delivered DCEP");
        assert!(matches!(
            &inbound.message,
            RTCMessageInternal::Dtls(DTLSMessage::Sctp(data))
                if data.ppi == PayloadProtocolIdentifier::Dcep
        ));

        // The DataChannel handler now sees the DCEP message only after SCTP has become
        // established. It can therefore accept the channel, emit Open, and queue its ACK
        // without attempting a write in CookieEchoed.
        let mut data_channel_ctx = DataChannelHandlerContext::new(now);
        let mut data_channels = DataChannelRegistry::new();
        let mut stats = RTCStatsAccumulator::new();
        {
            let mut handler = DataChannelHandler::new(
                &mut data_channel_ctx,
                &mut data_channels,
                &mut stats,
                None,
                RTCDtlsRole::Client,
                None,
            );
            handler
                .handle_read(inbound)
                .expect("accept DCEP after SCTP handshake");
        }
        assert!(
            data_channel_ctx.read_outs.iter().any(|message| matches!(
                &message.message,
                RTCMessageInternal::Dtls(DTLSMessage::DataChannel(app))
                    if matches!(app.data_channel_event, DataChannelEvent::Open)
            )),
            "the DataChannel Open event must follow SCTP establishment"
        );
    }

    /// With the pipeline already at its bound, inbound DATA must be left in the reassembly
    /// queue rather than pulled into the pipeline — and the peer must be *told*, by the only
    /// channel SCTP has for saying so: a smaller `a_rwnd` in the SACK.
    ///
    /// Draining unconditionally is exactly what defeats this. The bytes sitting in the
    /// reassembly queue are what `max_receive_buffer_size` is measured against, so emptying
    /// it on arrival keeps `a_rwnd` pinned at maximum however far behind the application is.
    #[test]
    fn a_full_pipeline_parks_the_stream_and_shrinks_the_advertised_window() {
        let now = Instant::now();

        // Same traffic, two identical associations: one allowed to drain, one at the bound.
        let mut e_free = establish();
        let free_dgrams = server_datagrams_carrying(&mut e_free, 1, 8, now);
        let mut e_full = establish();
        let full_dgrams = server_datagrams_carrying(&mut e_full, 1, 8, now);
        assert!(!full_dgrams.is_empty(), "server produced no DATA");

        let (ctx_free, flushed_free) = run_client(now, e_free, free_dgrams, 0);
        let (ctx_full, flushed_full) =
            run_client(now, e_full, full_dgrams, SCTP_PIPELINE_READ_BACKLOG_LIMIT);

        assert!(
            !ctx_free.read_outs.is_empty(),
            "control: an empty pipeline must receive the messages"
        );
        assert!(
            ctx_free.pending_readable.is_empty(),
            "control: nothing should be parked when there is room"
        );

        assert!(
            ctx_full.read_outs.is_empty(),
            "a full pipeline must not be handed more messages, got {}",
            ctx_full.read_outs.len()
        );
        assert!(
            !ctx_full.pending_readable.is_empty(),
            "the stream must be remembered, or the edge-triggered Readable is lost forever"
        );

        let free = flushed_sack_a_rwnd(&flushed_free).expect("control flushed no SACK");
        let full = flushed_sack_a_rwnd(&flushed_full).expect("parked run flushed no SACK");
        assert!(
            full < free,
            "undrained bytes must shrink the window advertised to the peer: \
             parked a_rwnd {full} should be below drained a_rwnd {free}"
        );
    }

    /// **The deadlock this feature would otherwise create.**
    ///
    /// `StreamEvent::Readable` is edge-triggered: `association/mod.rs` emits it when a *new*
    /// DATA chunk arrives, never because unread data remains. So the moment back-pressure
    /// works — the peer throttled, no new chunk coming — a stream parked mid-drain would
    /// never be revisited, and the connection would stall exactly when the feature engaged.
    ///
    /// This drives `handle_timeout` with **no new inbound datagram** and requires the parked
    /// data to come through anyway.
    #[test]
    fn a_parked_stream_resumes_without_a_new_inbound_chunk() {
        let now = Instant::now();
        let mut e = establish();
        let dgrams = server_datagrams_carrying(&mut e, 1, 8, now);

        let mut ctx = client_ctx(now, e.client_ep, e.client_ch, e.client_conn);
        {
            let mut handler = SctpHandler::new(&mut ctx, SCTP_PIPELINE_READ_BACKLOG_LIMIT);
            for dgram in dgrams {
                handler.handle_read(raw_read(now, dgram)).expect("read");
            }
        }
        assert!(ctx.read_outs.is_empty(), "precondition: nothing drained");
        assert!(!ctx.pending_readable.is_empty(), "precondition: parked");

        // The application drains, so the pipeline has room again. Nothing else happens: no
        // packet arrives, and the peer — correctly throttled — sends nothing. The only call
        // is the one the pipeline makes when it wants data.
        let resumed = {
            let mut handler = SctpHandler::new(&mut ctx, 0);
            handler.poll_read()
        };

        assert!(
            resumed.is_some(),
            "parked stream never resumed — this is the edge-triggered Readable deadlock: \
             the peer is throttled so no new chunk will arrive to re-trigger the drain"
        );
        assert!(
            ctx.pending_readable.is_empty(),
            "a fully drained stream must stop being remembered"
        );
    }

    /// A resumed drain must be stamped with **its own** association's 5-tuple.
    ///
    /// The first version of this kept a single `last_transport` — the most recent inbound
    /// datagram's context — and reused it for every resumed message. With more than one
    /// association on the endpoint (which `sctp_associations` being a map allows, and
    /// `establish_two` exercises) that hands association A's messages association B's peer
    /// address, because whichever datagram arrived last wins.
    #[test]
    fn a_resumed_drain_uses_its_own_associations_transport() {
        let now = Instant::now();

        // Two associations to two different peers, each carrying parked data.
        let mut e_a = establish();
        let a_dgrams = server_datagrams_carrying(&mut e_a, 1, 4, now);
        let mut e_b = establish();
        let b_dgrams = server_datagrams_carrying(&mut e_b, 1, 4, now);

        let peer_a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7001);
        let peer_b = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7002);

        let mut ctx = client_ctx(now, e_a.client_ep, e_a.client_ch, e_a.client_conn);
        {
            let mut handler = SctpHandler::new(&mut ctx, SCTP_PIPELINE_READ_BACKLOG_LIMIT);
            for dgram in a_dgrams {
                let mut msg = raw_read(now, dgram);
                msg.transport.peer_addr = peer_a;
                handler.handle_read(msg).expect("read A");
            }
        }
        assert!(!ctx.pending_readable.is_empty(), "A must be parked");

        // A datagram from a *different* peer arrives afterwards. Under the old single
        // `last_transport` this is the value A's resumed messages would inherit.
        {
            let mut handler = SctpHandler::new(&mut ctx, SCTP_PIPELINE_READ_BACKLOG_LIMIT);
            for dgram in b_dgrams {
                let mut msg = raw_read(now, dgram);
                msg.transport.peer_addr = peer_b;
                // Belongs to no association on this endpoint; under the old single
                // `last_transport` it would still have overwritten it.
                let _ = handler.handle_read(msg);
            }
        }

        // Now let A resume, through the path that actually resumes it.
        let mut resumed = vec![];
        {
            let mut handler = SctpHandler::new(&mut ctx, 0);
            while let Some(msg) = handler.poll_read() {
                resumed.push(msg);
            }
        }

        assert!(!resumed.is_empty(), "A never resumed");
        for msg in &resumed {
            assert_eq!(
                msg.transport.peer_addr, peer_a,
                "a resumed message must carry its own association's peer address, not \
                 whichever datagram happened to arrive most recently"
            );
        }
    }

    /// `poll_timeout` must stay silent about parked streams.
    ///
    /// Reporting a deadline for them was tried and reverted. The only instant available here
    /// is the *retained* one, which is always in the past relative to the caller's clock, and
    /// a caller that computes `deadline - now` gets zero. `tests/ice_tcp_active_passive.rs`
    /// treats a zero delay as "handle the timeout and `continue`", skipping its socket reads
    /// — so a past deadline starved the connection of I/O for the test's full 30 s budget and
    /// no data-channel message was ever delivered.
    #[test]
    fn poll_timeout_reports_nothing_for_a_parked_stream() {
        let now = Instant::now();
        let mut e = establish();
        let dgrams = server_datagrams_carrying(&mut e, 1, 8, now);

        let mut ctx = client_ctx(now, e.client_ep, e.client_ch, e.client_conn);
        {
            let mut handler = SctpHandler::new(&mut ctx, SCTP_PIPELINE_READ_BACKLOG_LIMIT);
            for dgram in dgrams {
                handler.handle_read(raw_read(now, dgram)).expect("read");
            }
        }
        assert!(!ctx.pending_readable.is_empty(), "precondition: parked");

        // With room, and with none: neither may produce a deadline at or before `now`.
        for backlog in [0, SCTP_PIPELINE_READ_BACKLOG_LIMIT] {
            let mut handler = SctpHandler::new(&mut ctx, backlog);
            if let Some(eto) = handler.poll_timeout() {
                assert!(
                    eto > now,
                    "a parked stream must not put a past deadline in front of the caller \
                     (backlog {backlog}): callers read that as zero delay and skip their I/O"
                );
            }
        }
    }

    // handle_timeout teardown-safety block: a graceful shutdown queues both the
    // `Drained` endpoint event AND the final SHUTDOWN datagram. handle_timeout
    // collects the drain (removing the association) and must flush the SHUTDOWN
    // *before* removal, or the peer would never learn the association closed.
    #[test]
    fn handle_timeout_flushes_final_packet_before_drain() {
        let now = Instant::now();
        let e = establish();
        let mut client_conn = e.client_conn;
        client_conn
            .shutdown()
            .expect("shutdown from Established queues Drained + SHUTDOWN");

        let mut ctx = client_ctx(now, e.client_ep, e.client_ch, client_conn);
        assert_eq!(ctx.sctp_transport.sctp_associations.len(), 1);

        {
            let mut handler = SctpHandler::new(&mut ctx, 0);
            handler
                .handle_timeout(Instant::now())
                .expect("handle_timeout");
        }

        assert!(
            ctx.sctp_transport.sctp_associations.is_empty(),
            "draining association must be removed"
        );
        let flushed = flushed_chunk_types(&ctx);
        assert!(
            flushed.contains(&CT_SHUTDOWN),
            "the final SHUTDOWN must be flushed before removal, not dropped \
             (flushed chunk types: {flushed:?})"
        );
    }

    const CT_ABORT: u8 = 6;

    // `close()` asks for an abort (W3C close(): "tear down the underlying SCTP association by
    // sending an SCTP ABORT chunk"). The data still queued must go out before the ABORT, which
    // would otherwise discard it, and the stream losses the abort reports must not stay queued
    // for a later read to surface as OnClose: the local channels close without their closing
    // procedure.
    #[test]
    fn abort_sends_pending_data_then_abort_and_drops_local_events() {
        let now = Instant::now();
        let e = establish();
        let mut client_conn = e.client_conn;
        client_conn
            .open_stream(1, PayloadProtocolIdentifier::Binary)
            .and_then(|mut s| {
                s.write_chunk_with_ppi(
                    now,
                    &Bytes::from_static(b"last words"),
                    PayloadProtocolIdentifier::Binary,
                )
            })
            .expect("queue DATA");

        let mut ctx = client_ctx(now, e.client_ep, e.client_ch, client_conn);
        let mut sent = vec![];
        {
            let mut handler = SctpHandler::new(&mut ctx, 0);
            handler.request_abort();
            while let Some(msg) = handler.poll_write() {
                sent.push(msg);
            }
        }
        let types: Vec<u8> = sent
            .iter()
            .filter_map(|m| match &m.message {
                RTCMessageInternal::Dtls(DTLSMessage::Raw(raw)) if raw.len() > 12 => Some(raw[12]),
                _ => None,
            })
            .collect();
        let data = types.iter().position(|t| *t == CT_PAYLOAD_DATA);
        let abort = types.iter().position(|t| *t == CT_ABORT);
        assert!(
            matches!((data, abort), (Some(d), Some(a)) if d < a),
            "the pending DATA must be sent, then the ABORT (chunk types: {types:?})"
        );

        for association in ctx.sctp_transport.sctp_associations.values_mut() {
            assert!(association.is_closed(), "the association is closed");
            assert!(
                association.poll().is_none(),
                "the abort's stream losses must not stay queued"
            );
        }
    }

    // handle_read teardown-safety block (+ the SctpMessage::Outbound arm): when the
    // client, mid graceful-shutdown, receives the peer's SHUTDOWN_ACK it must emit a
    // SHUTDOWN_COMPLETE. That inbound arrives via handle_read, which collects the
    // Drained and removes the association — so the SHUTDOWN_COMPLETE has to be flushed
    // in the same call.
    #[test]
    fn handle_read_flushes_final_packet_before_drain() {
        let e = establish();
        let mut client_conn = e.client_conn;
        let mut server_ep = e.server_ep;
        let mut server_conn = e.server_conn;
        let server_ch = e.server_ch;
        let now = Instant::now();

        // Client initiates graceful shutdown -> emits SHUTDOWN.
        client_conn.shutdown().expect("shutdown");
        let shutdown_dgrams = drain_transmits(&mut client_conn, now);
        assert!(!shutdown_dgrams.is_empty(), "shutdown emits SHUTDOWN");

        // Server processes SHUTDOWN -> replies SHUTDOWN_ACK.
        for d in shutdown_dgrams {
            if let Some((ch, DatagramEvent::AssociationEvent(ev))) =
                server_ep.handle(now, client_addr(), None, d)
            {
                assert_eq!(ch, server_ch);
                server_conn.handle_event(ev);
            }
        }
        while server_conn.poll().is_some() {}
        let ack_dgrams = drain_transmits(&mut server_conn, now);
        assert!(!ack_dgrams.is_empty(), "server replies SHUTDOWN_ACK");

        // Feed SHUTDOWN_ACK to the client HANDLER via handle_read.
        let mut ctx = client_ctx(now, e.client_ep, e.client_ch, client_conn);
        assert_eq!(ctx.sctp_transport.sctp_associations.len(), 1);
        {
            let mut handler = SctpHandler::new(&mut ctx, 0);
            for d in ack_dgrams {
                let msg = TaggedRTCMessageInternal {
                    now,
                    transport: TransportContext {
                        local_addr: client_addr(),
                        peer_addr: server_addr(),
                        transport_protocol: TransportProtocol::UDP,
                        ecn: None,
                    },
                    message: RTCMessageInternal::Dtls(DTLSMessage::Raw(BytesMut::from(&d[..]))),
                };
                handler.handle_read(msg).expect("handle_read");
            }
        }

        assert!(
            ctx.sctp_transport.sctp_associations.is_empty(),
            "draining association must be removed"
        );
        let flushed = flushed_chunk_types(&ctx);
        assert!(
            flushed.contains(&CT_SHUTDOWN_COMPLETE),
            "the final SHUTDOWN_COMPLETE must be flushed before removal, not dropped \
             (flushed chunk types: {flushed:?})"
        );
    }

    // The teardown-safety flush must touch ONLY the draining association. With two
    // live associations, draining A must flush A's final SHUTDOWN and remove A while
    // leaving B — and B's queued DATA — untouched (B keeps coalescing and is flushed
    // later in poll_write). The over-broad "flush every association" form would drain
    // B's DATA here too; this test rejects that.
    #[test]
    fn teardown_flush_targets_only_the_draining_association() {
        let now = Instant::now();
        let (client_ep, ch_a, mut conn_a, ch_b, mut conn_b) = establish_two();

        // A: graceful shutdown -> Drained + a pending SHUTDOWN.
        conn_a.shutdown().expect("shutdown A");
        // B: queue DATA that is NOT yet flushed (stays pending in the association).
        conn_b
            .open_stream(1, PayloadProtocolIdentifier::Binary)
            .and_then(|mut s| {
                s.write_chunk_with_ppi(
                    now,
                    &Bytes::from_static(b"pending payload on B"),
                    PayloadProtocolIdentifier::Binary,
                )
            })
            .expect("queue DATA on B");

        let mut transport = SctpTransport::new(
            SctpMaxMessageSize::default(),
            None,
            None,
            test_transport_id(TransportKind::Sctp),
            test_transport_id(TransportKind::Dtls),
        );
        transport
            .internal_buffer
            .resize(SctpMaxMessageSize::DEFAULT_MESSAGE_SIZE as usize, 0);
        transport.sctp_endpoint = Some(client_ep);
        transport.sctp_associations.insert(ch_a, conn_a);
        transport.sctp_associations.insert(ch_b, conn_b);
        let mut ctx = SctpHandlerContext::new(now, transport);

        {
            let mut handler = SctpHandler::new(&mut ctx, 0);
            handler
                .handle_timeout(Instant::now())
                .expect("handle_timeout");
        }

        // A (draining) removed; B (live) retained.
        assert!(
            !ctx.sctp_transport.sctp_associations.contains_key(&ch_a),
            "draining association A must be removed"
        );
        assert!(
            ctx.sctp_transport.sctp_associations.contains_key(&ch_b),
            "live association B must be retained"
        );

        // Only A's SHUTDOWN was flushed; B's DATA was NOT.
        let flushed = flushed_chunk_types(&ctx);
        assert!(
            flushed.contains(&CT_SHUTDOWN),
            "A's final SHUTDOWN must be flushed (flushed chunk types: {flushed:?})"
        );
        assert!(
            !flushed.contains(&CT_PAYLOAD_DATA),
            "B's DATA must NOT be flushed by A's teardown (flushed chunk types: {flushed:?})"
        );

        // ...and B's DATA is still pending in the association, ready for poll_write.
        let b = ctx
            .sctp_transport
            .sctp_associations
            .get_mut(&ch_b)
            .expect("B present");
        assert!(
            b.poll_transmit(Instant::now()).is_some(),
            "B's queued DATA must remain pending, not consumed by A's teardown"
        );
    }

    // Design §3.7's worked example. The client INIT is queued by `connect` inside
    // `handle_event` and emitted by the *next* `poll_write` — so before C3-01 gave the event
    // channel a timestamp, the retained instant was still unset at that point and the flush
    // fell back to `Instant::now()`. That was not a defensive branch: it was the path taken on
    // every client connection. Now the INIT is stamped from the event that armed it.
    #[test]
    fn client_init_flush_is_stamped_from_the_event_that_armed_it() {
        let base = Instant::now();
        let t = |secs| base + Duration::from_secs(secs);

        let mut transport = SctpTransport::new(
            SctpMaxMessageSize::default(),
            None,
            None,
            test_transport_id(TransportKind::Sctp),
            test_transport_id(TransportKind::Dtls),
        );
        transport
            .internal_buffer
            .resize(SctpMaxMessageSize::DEFAULT_MESSAGE_SIZE as usize, 0);
        transport.sctp_endpoint = Some(Endpoint::new(
            client_addr(),
            TransportProtocol::UDP,
            EndpointConfig::default().into(),
            None,
        ));
        transport.sctp_transport_config = Some(TransportConfig::default());
        let mut ctx = SctpHandlerContext::new(t(0), transport);

        // No inbound SCTP has been seen, so nothing but the event has moved the retained
        // instant off its construction seed.
        assert_eq!(ctx.now, t(0));

        {
            let mut handler = SctpHandler::new(&mut ctx, 0);
            handler
                .handle_event(TaggedRTCEventInternal {
                    now: t(7),
                    event: RTCEventInternal::DTLSHandshakeComplete(None, None),
                })
                .expect("handle_event");
        }
        assert_eq!(
            ctx.now,
            t(7),
            "the event's instant must arm the deferred flush"
        );

        let flushed = {
            let mut handler = SctpHandler::new(&mut ctx, 0);
            let mut out = vec![];
            while let Some(msg) = handler.poll_write() {
                out.push(msg);
            }
            out
        };

        assert!(!flushed.is_empty(), "poll_write must emit the client INIT");
        for msg in &flushed {
            assert_eq!(
                msg.now,
                t(7),
                "the INIT carries the instant of the event that caused it, not the wall clock"
            );
        }
    }

    // C4-04 replaced `last_now: Option<Instant>` + `unwrap_or_else(Instant::now)` with a
    // construction-seeded `now: Instant`. The *point* of the deferred flush is batching — N
    // inbound DATA chunks ingested before one flush, so the ack machine coalesces them into a
    // single SACK instead of one per two packets. Nothing in the suite pinned that invariant,
    // and the workspace has no SCTP throughput benchmark to measure it with, so this asserts it
    // structurally: a burst of reads must arm the flush once and produce one flush's worth of
    // output, not one per packet.
    #[test]
    fn a_burst_of_reads_produces_one_flush_not_one_per_packet() {
        let base = Instant::now();
        let t = |millis| base + Duration::from_millis(millis);

        let e = establish();
        let mut server_conn = e.server_conn;
        let server_ch = e.server_ch;
        let mut ctx = client_ctx(t(0), e.client_ep, e.client_ch, e.client_conn);

        // Server sends several DATA chunks; each becomes its own inbound datagram.
        let mut stream = server_conn
            .open_stream(1, PayloadProtocolIdentifier::Binary)
            .expect("open stream");
        for i in 0..4u8 {
            stream
                .write_chunk_with_ppi(
                    base,
                    &Bytes::from(vec![i; 1024]),
                    PayloadProtocolIdentifier::Binary,
                )
                .expect("queue DATA");
        }
        let dgrams = drain_transmits(&mut server_conn, t(0));
        assert!(
            dgrams.len() >= 2,
            "the burst must be more than one datagram to be worth batching, got {}",
            dgrams.len()
        );
        let _ = server_ch;

        // Ingest the whole burst. Every read arms the flush; none performs it.
        {
            let mut handler = SctpHandler::new(&mut ctx, 0);
            for (i, d) in dgrams.iter().enumerate() {
                handler
                    .handle_read(TaggedRTCMessageInternal {
                        now: t(i as u64),
                        transport: TransportContext {
                            local_addr: client_addr(),
                            peer_addr: server_addr(),
                            transport_protocol: TransportProtocol::UDP,
                            ecn: None,
                        },
                        message: RTCMessageInternal::Dtls(DTLSMessage::Raw(BytesMut::from(&d[..]))),
                    })
                    .expect("handle_read");
            }
        }
        assert!(
            ctx.write_outs.is_empty(),
            "ingest must not flush: that is what makes the SACKs coalesce"
        );
        assert_eq!(
            ctx.now,
            t(dgrams.len() as u64 - 1),
            "the retained instant is the newest of the burst, which is what stamps the flush"
        );

        // One poll_write runs the single flush for the whole burst.
        let flushed = {
            let mut handler = SctpHandler::new(&mut ctx, 0);
            let mut out = vec![];
            while let Some(msg) = handler.poll_write() {
                out.push(msg);
            }
            out
        };
        assert!(
            !flushed.is_empty(),
            "the flush must emit the coalesced SACK"
        );
        assert!(
            flushed.len() < dgrams.len(),
            "batching must emit fewer datagrams than it ingested ({} in, {} out)",
            dgrams.len(),
            flushed.len()
        );
    }
}
