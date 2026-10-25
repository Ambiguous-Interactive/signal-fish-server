use crate::association::{Association, AssociationError};
use crate::chunk::chunk_payload_data::{
    ChunkPayloadData, MessageReliability, PayloadProtocolIdentifier,
};
use crate::queue::reassembly_queue::{Chunks, ReassemblyQueue};
use crate::{ErrorCauseCode, Event, Side};
use shared::error::{Error, Result};

use crate::util::{ByteSlice, BytesChunk, BytesSource};
use bytes::Bytes;
use log::{debug, error, trace};
use std::fmt;
use std::time::{Duration, Instant};

/// Identifier for a stream within a particular association
pub type StreamId = u16;

/// Application events about streams
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamEvent {
    /// One or more new streams has been opened
    Opened {
        /// Which stream was opened.
        id: StreamId,
    },
    /// A currently open stream has data or errors waiting to be read
    Readable {
        /// Which stream is now readable
        id: StreamId,
    },
    /// A formerly write-blocked stream might be ready for a write or have been stopped
    ///
    /// Only generated for streams that are currently open.
    Writable {
        /// Which stream is now writable
        id: StreamId,
    },
    /// A finished stream has been fully acknowledged or stopped
    Finished {
        /// Which stream has been finished
        id: StreamId,
    },
    /// The peer asked us to stop sending on an outgoing stream
    Stopped {
        /// Which stream has been stopped
        id: StreamId,
        /// Error code supplied by the peer
        error_code: ErrorCauseCode,
    },
    /// At least one new stream of a certain directionality may be opened
    Available,
    /// The number of bytes of outgoing data buffered is lower than the threshold.
    BufferedAmountLow {
        /// Which stream is now readable
        id: StreamId,
    },
    /// The number of bytes of outgoing data buffered is higher than the threshold.
    BufferedAmountHigh {
        /// Which stream is now readable
        id: StreamId,
    },
    /// Outgoing buffered data was released (acknowledged OR abandoned), carrying
    /// the exact number of SCTP payload bytes freed for this stream. Unlike the
    /// edge-triggered [`StreamEvent::BufferedAmountLow`], this fires on every
    /// release with the byte delta, so upper layers can keep their own
    /// send-buffer accounting (e.g. a synchronous back-pressure counter) exact.
    BufferedAmountReleased {
        /// Which stream released buffered bytes
        id: StreamId,
        /// SCTP payload bytes released, including DCEP and empty-message fillers.
        n_bytes: usize,
        /// Nonempty WebRTC application bytes released. DCEP and empty-message
        /// filler bytes do not count toward the data channel send counter.
        data_channel_bytes: usize,
    },
}

/// Reliability type for stream
#[derive(Default, Debug, Copy, Clone, PartialEq)]
#[non_exhaustive]
pub enum ReliabilityType {
    /// ReliabilityTypeReliable is used for reliable transmission
    #[default]
    Reliable = 0,
    /// ReliabilityTypeRexmit is used for partial reliability by retransmission count
    Rexmit = 1,
    /// ReliabilityTypeTimed is used for partial reliability by retransmission duration
    Timed = 2,
}

impl fmt::Display for ReliabilityType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match *self {
            ReliabilityType::Reliable => "Reliable",
            ReliabilityType::Rexmit => "Rexmit",
            ReliabilityType::Timed => "Timed",
        };
        write!(f, "{}", s)
    }
}

impl From<u8> for ReliabilityType {
    fn from(v: u8) -> ReliabilityType {
        match v {
            1 => ReliabilityType::Rexmit,
            2 => ReliabilityType::Timed,
            _ => ReliabilityType::Reliable,
        }
    }
}

/// Stream represents an SCTP stream
pub struct Stream<'a> {
    pub(crate) stream_identifier: StreamId,
    pub(crate) generation: u64,
    pub(crate) association: &'a mut Association,
}

impl Stream<'_> {
    /// read reads a packet of len(p) bytes, dropping the Payload Protocol Identifier.
    /// Returns EOF when the stream is reset or an error if the stream is closed
    /// otherwise.
    pub fn read(&mut self) -> Result<Option<Chunks>> {
        self.read_sctp()
    }

    /// read_sctp reads a packet of len(p) bytes and returns the associated Payload
    /// Protocol Identifier.
    /// Returns EOF when the stream is reset or an error if the stream is closed
    /// otherwise. Complete messages remain readable after graceful association
    /// shutdown until the application reads them or stops the stream.
    pub fn read_sctp(&mut self) -> Result<Option<Chunks>> {
        // The final old-generation read can complete a deferred reset and
        // admit saved DATA into the same SID. This handle must report EOF
        // instead of consuming or changing that new stream.
        if self
            .association
            .streams
            .get(&self.stream_identifier)
            .is_none_or(|s| s.generation != self.generation || s.incoming_reset_complete)
        {
            return Ok(None);
        }
        let was_full = !self.association.retired
            && self
                .association
                .streams
                .get(&self.stream_identifier)
                .is_some_and(StreamState::has_readable_data)
            && self.association.get_my_receiver_window_credit() == 0;
        if let Some(s) = self
            .association
            .streams
            .get_mut(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
            && (s.state == RecvSendState::ReadWritable || s.state == RecvSendState::Readable)
        {
            let chunks = s.reassembly_queue.read();
            if !s.reassembly_queue.is_readable() {
                if self.association.retired {
                    self.association.unregister_stream(
                        self.stream_identifier,
                        AssociationError::AssociationClosed,
                    );
                    self.association.release_reset_data();
                } else {
                    self.association
                        .retry_deferred_resets(self.stream_identifier);
                }
            }
            if chunks.is_some()
                && was_full
                && !self.association.retired
                && self.association.get_my_receiver_window_credit() > 0
            {
                /* Reopen a zero window now; reads with remaining credit keep normal ACK policy. */
                self.association.ack_state = super::AckState::Immediate;
                self.association.timers.stop(super::Timer::Ack);
                self.association.awake_write_loop();
            }
            Ok(chunks)
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// write_sctp writes len(p) bytes from p to the DTLS connection
    ///
    /// An empty write is a no-op on a writable stream.
    ///
    /// `now` is the instant the message is handed to the stack. See the note on
    /// [`Stream`] for why it is a parameter.
    pub fn write_sctp(
        &mut self,
        now: Instant,
        p: &Bytes,
        ppi: PayloadProtocolIdentifier,
    ) -> Result<usize> {
        self.write_source(now, &mut BytesChunk::new(p), ppi)
    }

    /// Send data on the given stream.
    ///
    /// Uses the deafult payload protocol (PPI).
    ///
    /// Returns the number of bytes successfully written.
    pub fn write(&mut self, now: Instant, data: &[u8]) -> Result<usize> {
        self.write_with_ppi(now, data, self.get_default_payload_type()?)
    }

    /// Send data on the given stream, with a specific payload protocol.
    ///
    /// Returns the number of bytes successfully written.
    pub fn write_with_ppi(
        &mut self,
        now: Instant,
        data: &[u8],
        ppi: PayloadProtocolIdentifier,
    ) -> Result<usize> {
        self.write_source(now, &mut ByteSlice::from_slice(data), ppi)
    }

    /// write writes len(p) bytes from p with the default Payload Protocol Identifier
    pub fn write_chunk(&mut self, now: Instant, p: &Bytes) -> Result<usize> {
        self.write_source(
            now,
            &mut BytesChunk::new(p),
            self.get_default_payload_type()?,
        )
    }

    /// Send an owned [`Bytes`] on the stream with a specific payload protocol.
    ///
    /// Unlike [`write_with_ppi`](Self::write_with_ppi), which takes a `&[u8]` and
    /// must copy it into a freshly allocated buffer, this enqueues the payload
    /// zero-copy: each fragment is a refcounted slice of `data`. Prefer this on the
    /// hot send path when the caller already owns the payload as `Bytes`.
    ///
    /// Returns the number of bytes successfully written.
    pub fn write_chunk_with_ppi(
        &mut self,
        now: Instant,
        data: &Bytes,
        ppi: PayloadProtocolIdentifier,
    ) -> Result<usize> {
        self.write_source(now, &mut BytesChunk::new(data), ppi)
    }

    /// Send data on the given stream
    ///
    /// Writes the next nonempty buffer as one message and returns its byte count.
    /// Later buffers remain unchanged. The message limit applies to that buffer,
    /// not the total array. A failed write leaves every buffer unchanged.
    pub fn write_chunks(&mut self, now: Instant, data: &mut [Bytes]) -> Result<usize> {
        let ppi = self.get_default_payload_type()?;
        let Some(next) = data.iter_mut().find(|chunk| !chunk.is_empty()) else {
            return self.write_source(now, &mut BytesChunk::new(&Bytes::new()), ppi);
        };
        let written = self.write_source(now, &mut BytesChunk::new(next), ppi)?;
        // Commit caller ownership only after admission succeeds.
        *next = Bytes::new();
        Ok(written)
    }

    /// write_source writes BytesSource to the DTLS connection
    fn write_source<B: BytesSource>(
        &mut self,
        now: Instant,
        source: &mut B,
        ppi: PayloadProtocolIdentifier,
    ) -> Result<usize> {
        if !self.is_writable() {
            return Err(Error::ErrStreamClosed);
        }

        if source.remaining() == 0 {
            return Ok(0);
        }

        if self
            .association
            .streams
            .get(&self.stream_identifier)
            .is_some_and(|stream| stream.max_payload_size < 4)
        {
            return Err(Error::ErrOutboundPacketTooLarge);
        }

        if source.remaining() > self.association.max_message_size() as usize {
            return Err(Error::ErrOutboundPacketTooLarge);
        }

        let id = self.association.allocate_message_id()?;
        let (p, _) = source.pop_chunk(self.association.max_message_size() as usize);

        if let Some(s) = self
            .association
            .streams
            .get_mut(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            let (is_buffered_amount_high, chunks) = s.packetize(now, &p, ppi);

            if is_buffered_amount_high {
                trace!("StreamEvent::BufferedAmountHigh");
                self.association
                    .events
                    .push_back(Event::Stream(StreamEvent::BufferedAmountHigh {
                        id: self.stream_identifier,
                    }))
            }

            self.association.send_payload_data(id, chunks);

            Ok(p.len())
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// Whether this stream has data or an error waiting to be read.
    pub fn is_readable(&self) -> bool {
        if let Some(s) = self
            .association
            .streams
            .get(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            s.state == RecvSendState::Readable || s.state == RecvSendState::ReadWritable
        } else {
            false
        }
    }

    /// Whether this stream can currently accept more data.
    /// Returns false once graceful association shutdown starts.
    pub fn is_writable(&self) -> bool {
        if self.association.is_drained() {
            return false;
        }
        if let Some(s) = self
            .association
            .streams
            .get(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            s.state == RecvSendState::Writable || s.state == RecvSendState::ReadWritable
        } else {
            false
        }
    }

    /// stop closes the read-direction of the stream.
    /// Future calls to read are not permitted after calling stop.
    /// During graceful association shutdown, discard this generation's unread
    /// DATA so receive pressure cannot prevent the peer's admitted tail draining.
    /// Saved reset generations retain their separate readers.
    /// Returns `ErrOutboundPacketTooLarge` without changing either half when
    /// the DATA packet budget cannot hold a stream-reset request.
    ///
    /// `now` is the instant the caller is acting at: resetting the stream queues an outgoing
    /// chunk, so it is on the same write path as [`write_sctp`](Self::write_sctp).
    pub fn stop(&mut self, now: Instant) -> Result<()> {
        if self.association.retired {
            if self
                .association
                .streams
                .get(&self.stream_identifier)
                .is_some_and(|s| s.generation == self.generation)
            {
                self.association
                    .unregister_stream(self.stream_identifier, AssociationError::AssociationClosed);
                self.association.release_reset_data();
            }
            return Ok(());
        }
        let reset = self
            .association
            .streams
            .get(&self.stream_identifier)
            .is_some_and(|s| {
                s.generation == self.generation
                    && matches!(
                        s.state,
                        RecvSendState::Readable | RecvSendState::ReadWritable
                    )
            });
        if reset && !self.association.is_drained() {
            self.association
                .send_reset_request(now, self.stream_identifier)?;
        }
        if let Some(s) = self
            .association
            .streams
            .get_mut(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            s.state = ((s.state as u8) & 0x2).into();
        }

        if reset {
            // A locally stopped reader cannot drain an already deferred
            // incoming reset. Re-evaluate it after the read direction closes.
            self.association
                .retry_deferred_resets(self.stream_identifier);
        }

        self.association.discard_stopped_read_data_during_shutdown();
        Ok(())
    }

    /// finish closes the write-direction of the stream.
    /// Future calls to write are not permitted after calling Close.
    pub fn finish(&mut self) -> Result<()> {
        if let Some(s) = self
            .association
            .streams
            .get_mut(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            s.state = ((s.state as u8) & 0x1).into();
        }
        Ok(())
    }

    /// Shuts down the read, write, or both halves of this stream.
    ///
    /// This function will cause all pending and future I/O on the specified portions to return
    /// immediately with an appropriate value (see the documentation of `Shutdown`).
    ///
    /// Resets the stream when both halves of this stream are shutdown.
    pub fn close(&mut self, now: Instant) -> Result<()> {
        self.stop(now)?;
        self.finish()
    }

    /// stream_identifier returns the Stream identifier associated to the stream.
    pub fn stream_identifier(&self) -> StreamId {
        self.stream_identifier
    }

    /// set_default_payload_type sets the default payload type used by write.
    pub fn set_default_payload_type(
        &mut self,
        default_payload_type: PayloadProtocolIdentifier,
    ) -> Result<()> {
        if let Some(s) = self
            .association
            .streams
            .get_mut(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            s.default_payload_type = default_payload_type;
            Ok(())
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// get_default_payload_type returns the payload type associated to the stream.
    pub fn get_default_payload_type(&self) -> Result<PayloadProtocolIdentifier> {
        if let Some(s) = self
            .association
            .streams
            .get(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            Ok(s.default_payload_type)
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// set_reliability_params sets reliability parameters for this stream.
    pub fn set_reliability_params(
        &mut self,
        unordered: bool,
        rel_type: ReliabilityType,
        rel_val: u32,
    ) -> Result<()> {
        if let Some(s) = self
            .association
            .streams
            .get_mut(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            debug!(
                "[{}] reliability params: ordered={} type={} value={}",
                s.side, !unordered, rel_type, rel_val
            );
            s.unordered = unordered;
            s.reliability_type = rel_type;
            s.reliability_value = rel_val;
            Ok(())
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// buffered_amount returns the number of bytes of data currently queued to be sent over this stream.
    pub fn buffered_amount(&self) -> Result<usize> {
        if let Some(s) = self
            .association
            .streams
            .get(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            Ok(s.buffered_amount)
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// buffered_amount_low_threshold returns the number of bytes of buffered outgoing data that is
    /// considered "low" Defaults to 0.
    pub fn buffered_amount_low_threshold(&self) -> Result<usize> {
        if let Some(s) = self
            .association
            .streams
            .get(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            Ok(s.buffered_amount_low)
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// set_buffered_amount_low_threshold is used to update the threshold.
    /// See buffered_amount_low_threshold().
    pub fn set_buffered_amount_low_threshold(&mut self, th: usize) -> Result<()> {
        if let Some(s) = self
            .association
            .streams
            .get_mut(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            s.buffered_amount_low = th;
            Ok(())
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// buffered_amount_high_threshold returns the number of bytes of buffered outgoing data that is
    /// considered "high" Defaults to u32::MAX.
    pub fn buffered_amount_high_threshold(&self) -> Result<usize> {
        if let Some(s) = self
            .association
            .streams
            .get(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            Ok(s.buffered_amount_high)
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// set_buffered_amount_high_threshold is used to update the threshold.
    /// See buffered_amount_high_threshold().
    pub fn set_buffered_amount_high_threshold(&mut self, th: usize) -> Result<()> {
        if let Some(s) = self
            .association
            .streams
            .get_mut(&self.stream_identifier)
            .filter(|s| s.generation == self.generation)
        {
            s.buffered_amount_high = th;
            Ok(())
        } else {
            Err(Error::ErrStreamClosed)
        }
    }
}

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum RecvSendState {
    #[default]
    Closed = 0,
    Readable = 1,
    Writable = 2,
    ReadWritable = 3,
}

impl From<u8> for RecvSendState {
    fn from(v: u8) -> Self {
        match v {
            1 => RecvSendState::Readable,
            2 => RecvSendState::Writable,
            3 => RecvSendState::ReadWritable,
            _ => RecvSendState::Closed,
        }
    }
}

/// StreamState represents the state of an SCTP stream
#[derive(Default, Debug)]
pub struct StreamState {
    pub(crate) side: Side,
    pub(crate) max_payload_size: u32,
    pub(crate) stream_identifier: StreamId,
    pub(crate) generation: u64,
    pub(crate) default_payload_type: PayloadProtocolIdentifier,
    pub(crate) reassembly_queue: ReassemblyQueue,
    pub(crate) sequence_number: u16,
    pub(crate) reset_requested: bool,
    pub(crate) incoming_reset_complete: bool,
    pub(crate) outgoing_reset_complete: bool,
    pub(crate) deferred_reset_next_ssn: Option<u16>,
    pub(crate) state: RecvSendState,
    pub(crate) unordered: bool,
    pub(crate) reliability_type: ReliabilityType,
    pub(crate) reliability_value: u32,
    pub(crate) buffered_amount: usize,
    pub(crate) buffered_amount_low: usize,
    pub(crate) buffered_amount_high: usize,
}
impl StreamState {
    pub(crate) fn new(
        side: Side,
        stream_identifier: StreamId,
        max_payload_size: u32,
        default_payload_type: PayloadProtocolIdentifier,
    ) -> Self {
        StreamState {
            side,
            stream_identifier,
            generation: 0,
            max_payload_size,
            default_payload_type,
            reassembly_queue: ReassemblyQueue::new(stream_identifier),
            sequence_number: 0,
            reset_requested: false,
            incoming_reset_complete: false,
            outgoing_reset_complete: false,
            deferred_reset_next_ssn: None,
            state: RecvSendState::ReadWritable,
            unordered: false,
            reliability_type: ReliabilityType::Reliable,
            reliability_value: 0,
            buffered_amount: 0,
            buffered_amount_low: 0,
            buffered_amount_high: u32::MAX as usize,
        }
    }

    /// Complete data that the application can still consume in this generation.
    pub(crate) fn has_readable_data(&self) -> bool {
        matches!(
            self.state,
            RecvSendState::Readable | RecvSendState::ReadWritable
        ) && !self.incoming_reset_complete
            && self.reassembly_queue.is_readable()
    }

    pub(crate) fn handle_data(&mut self, pd: &ChunkPayloadData) -> bool {
        self.reassembly_queue.push(pd.clone())
    }

    pub(crate) fn handle_forward_tsn_for_ordered(&mut self, ssn: u16) {
        // Remove all chunks older than or equal to the new TSN from
        // the reassembly_queue.
        self.reassembly_queue.forward_tsn_for_ordered(ssn);
    }

    pub(crate) fn handle_forward_tsn_for_unordered(&mut self, new_cumulative_tsn: u32) {
        // Remove all chunks older than or equal to the new TSN from
        // the reassembly_queue.
        self.reassembly_queue
            .forward_tsn_for_unordered(new_cumulative_tsn);
    }

    fn packetize(
        &mut self,
        now: Instant,
        raw: &Bytes,
        ppi: PayloadProtocolIdentifier,
    ) -> (bool, Vec<ChunkPayloadData>) {
        let mut i = 0;
        let mut remaining = raw.len();

        // From draft-ietf-rtcweb-data-protocol-09, section 6:
        //   All Data Channel Establishment Protocol messages MUST be sent using
        //   ordered delivery and reliable transmission.
        let unordered = ppi != PayloadProtocolIdentifier::Dcep && self.unordered;
        let reliability = if ppi == PayloadProtocolIdentifier::Dcep {
            MessageReliability::Reliable
        } else {
            match self.reliability_type {
                ReliabilityType::Reliable => MessageReliability::Reliable,
                ReliabilityType::Rexmit => MessageReliability::Rexmit {
                    max_retransmits: self.reliability_value,
                },
                ReliabilityType::Timed => MessageReliability::Timed {
                    deadline: now + Duration::from_millis(self.reliability_value as u64),
                },
            }
        };

        // DATA padding must fit the configured packet budget. The
        // 16-bit chunk length includes the complete DATA header.
        let fragment_budget = (self.max_payload_size as usize & !3)
            .min(usize::from(u16::MAX) - crate::config::DATA_CHUNK_HEADER_SIZE as usize);
        // Admission has checked a nonzero budget. Reserve the known shape once,
        // including one slot rather than Vec's minimum growth for whole messages.
        let mut chunks = Vec::with_capacity(raw.len().div_ceil(fragment_budget));
        while remaining != 0 {
            let fragment_size = fragment_budget.min(remaining);

            // Copy the userdata since we'll have to store it until acked
            // and the caller may re-use the buffer in the mean time
            let user_data = raw.slice(i..i + fragment_size);

            let chunk = ChunkPayloadData {
                // Timed reliability measures a message's age from here, so that
                // a long wait for cwnd or rwnd counts against maxPacketLifeTime.
                created_at: Some(now),
                reliability,
                stream_generation: self.generation,
                stream_identifier: self.stream_identifier,
                user_data,
                unordered,
                beginning_fragment: i == 0,
                ending_fragment: remaining - fragment_size == 0,
                immediate_sack: false,
                payload_type: ppi,
                stream_sequence_number: self.sequence_number,
                ..Default::default()
            };

            chunks.push(chunk);

            remaining -= fragment_size;
            i += fragment_size;
        }

        // RFC 4960 Sec 6.6
        // Note: When transmitting ordered and unordered data, an endpoint does
        // not increment its Stream Sequence Number when transmitting a DATA
        // chunk with U flag set to 1.
        if !unordered {
            self.sequence_number = self.sequence_number.wrapping_add(1);
        }

        let old_amount = self.buffered_amount;
        let n_bytes_added = raw.len();
        self.buffered_amount += raw.len();
        let new_amount = self.buffered_amount;

        trace!(
            "[{}] new_amount = {}, old_amount = {}, buffered_amount_high = {}, n_bytes_added = {}",
            self.side, new_amount, old_amount, self.buffered_amount_high, n_bytes_added,
        );

        let is_buffered_amount_high =
            old_amount < self.buffered_amount_high && new_amount >= self.buffered_amount_high;

        (is_buffered_amount_high, chunks)
    }

    /// This method is called by association to notify this stream
    /// of the specified amount of outgoing data has been delivered to the peer.
    pub(crate) fn on_buffer_released(&mut self, n_bytes_released: i64) -> bool {
        if n_bytes_released <= 0 {
            return false;
        }

        let old_amount = self.buffered_amount;
        let new_amount = if old_amount < n_bytes_released as usize {
            self.buffered_amount = 0;
            error!(
                "[{}] released buffer size {} should be <= {}",
                self.side, n_bytes_released, 0,
            );
            0
        } else {
            self.buffered_amount -= n_bytes_released as usize;

            old_amount - n_bytes_released as usize
        };

        trace!(
            "[{}] new_amount = {}, old_amount = {}, buffered_amount_low = {}, n_bytes_released = {}",
            self.side, new_amount, old_amount, self.buffered_amount_low, n_bytes_released,
        );

        old_amount > self.buffered_amount_low && new_amount <= self.buffered_amount_low
    }

    pub(crate) fn get_num_bytes_in_reassembly_queue(&self) -> usize {
        // No lock is required as it reads the size with atomic load function.
        self.reassembly_queue.get_num_bytes()
    }
}

#[cfg(test)]
#[path = "stream_write_test.rs"]
mod write_tests;
