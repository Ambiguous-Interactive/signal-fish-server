use crate::StreamId;
use crate::chunk::chunk_payload_data::{ChunkPayloadData, PayloadProtocolIdentifier};
use crate::util::*;
use shared::error::{Error, Result};

use bytes::{Bytes, BytesMut};
use rustc_hash::FxHashMap;
use std::collections::VecDeque;

/// A chunk of data from the stream
#[derive(Debug, PartialEq)]
pub struct Chunk {
    /// The contents of the chunk
    pub bytes: Bytes,
}

/// Chunks is a set of chunks that share the same SSN
#[derive(Debug, Clone)]
pub struct Chunks {
    /// used only with the ordered chunks
    pub ssn: u16,
    /// The payload protocol identifier shared by every fragment of this message.
    pub ppi: PayloadProtocolIdentifier,
    /// The fragments, in order, that make up one complete message.
    pub chunks: Vec<ChunkPayloadData>,
    offset: usize,
    index: usize,
    /// Arrival order, used only to decide which of two ready messages came first.
    /// A counter rather than an instant: this is sequencing, not time.
    arrival: u64,
    discontinuities: usize,
    #[cfg(test)]
    pub(crate) adjacency_checks: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(crate) ordering_comparisons: std::cell::Cell<usize>,
}

impl Chunks {
    /// Whether the reassembled message carries no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The total length in bytes of the reassembled message.
    pub fn len(&self) -> usize {
        let mut l = 0;
        for c in &self.chunks {
            l += c.user_data.len();
        }
        l
    }

    /// Reassemble all fragments into a single freshly-allocated, exactly-sized
    /// buffer with one copy.
    ///
    /// Unlike [`read`](Self::read), this does not round-trip through a
    /// caller-provided scratch buffer, eliminating one full-payload copy on the
    /// receive path (the reassembled message would otherwise be copied into the
    /// scratch buffer and then again into the delivered `BytesMut`). Returns
    /// [`Error::ErrShortBuffer`] when the message exceeds `max_len`, mirroring
    /// `read`'s bound so oversized inbound messages are still rejected.
    pub fn to_payload(&self, max_len: usize) -> Result<BytesMut> {
        let total = self.len();
        if total > max_len {
            return Err(Error::ErrShortBuffer);
        }
        let mut buf = BytesMut::with_capacity(total);
        for c in &self.chunks {
            buf.extend_from_slice(&c.user_data);
        }
        Ok(buf)
    }

    /// Consume a complete message, reusing a uniquely owned single fragment.
    ///
    /// Shared and fragmented messages retain the same bounded copy behavior as
    /// [`to_payload`](Self::to_payload). The returned buffer owns its bytes and
    /// remains valid after the stream or association closes.
    pub fn into_payload(mut self, max_len: usize) -> Result<BytesMut> {
        if self.chunks.len() == 1
            && let Some(fragment) = self.chunks.pop()
        {
            if fragment.user_data.len() > max_len {
                return Err(Error::ErrShortBuffer);
            }
            return Ok(fragment
                .user_data
                .try_into_mut()
                .unwrap_or_else(|shared| BytesMut::from(shared.as_ref())));
        }
        self.to_payload(max_len)
    }

    /// Concatenates every fragment into `buf`, returning the number of bytes written.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ErrShortBuffer`](shared::error::Error::ErrShortBuffer) if `buf` cannot
    /// hold the whole message; the partial copy is left in place.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let mut n_written = 0;
        for c in &self.chunks {
            let to_copy = c.user_data.len();
            let n = std::cmp::min(to_copy, buf.len() - n_written);
            buf[n_written..n_written + n].copy_from_slice(&c.user_data[..n]);
            n_written += n;
            if n < to_copy {
                return Err(Error::ErrShortBuffer);
            }
        }
        Ok(n_written)
    }

    /// Yields the next slice of the reassembled message, up to `max_length` bytes.
    ///
    /// Advances an internal cursor, so repeated calls walk the message; returns `None` once it is
    /// exhausted.
    pub fn next(&mut self, max_length: usize) -> Option<Chunk> {
        if self.index >= self.chunks.len() {
            return None;
        }

        let fragment = &self.chunks[self.index].user_data;
        let available = fragment.len() - self.offset;
        if available > 0 && (max_length <= available || self.index + 1 == self.chunks.len()) {
            let length = available.min(max_length);
            let bytes = fragment.slice(self.offset..self.offset + length);
            self.offset += length;
            if self.offset == fragment.len() {
                self.index += 1;
                self.offset = 0;
                while self.index < self.chunks.len() && self.chunks[self.index].user_data.is_empty()
                {
                    self.index += 1;
                }
            }
            return Some(Chunk { bytes });
        }

        let mut capacity = 0;
        for (index, chunk) in self.chunks[self.index..].iter().enumerate() {
            let offset = if index == 0 { self.offset } else { 0 };
            capacity += (chunk.user_data.len() - offset).min(max_length - capacity);
            if capacity == max_length {
                break;
            }
        }
        let mut buf = BytesMut::with_capacity(capacity);

        let mut n_written = 0;
        while self.index < self.chunks.len() {
            let to_copy = self.chunks[self.index].user_data[self.offset..].len();
            let n = std::cmp::min(to_copy, max_length - n_written);
            buf.extend_from_slice(&self.chunks[self.index].user_data[self.offset..self.offset + n]);
            n_written += n;
            if n < to_copy {
                self.offset += n;
                return Some(Chunk {
                    bytes: buf.freeze(),
                });
            }
            self.index += 1;
            self.offset = 0;
        }

        Some(Chunk {
            bytes: buf.freeze(),
        })
    }

    pub(crate) fn new(
        arrival: u64,
        ssn: u16,
        ppi: PayloadProtocolIdentifier,
        chunks: Vec<ChunkPayloadData>,
    ) -> Self {
        let discontinuities = chunks
            .windows(2)
            .filter(|pair| pair[1].tsn != pair[0].tsn.wrapping_add(1))
            .count();
        Chunks {
            ssn,
            ppi,
            chunks,
            offset: 0,
            index: 0,
            arrival,
            discontinuities,
            #[cfg(test)]
            adjacency_checks: std::cell::Cell::new(0),
            #[cfg(test)]
            ordering_comparisons: std::cell::Cell::new(0),
        }
    }

    pub(crate) fn push(&mut self, chunk: ChunkPayloadData) -> bool {
        let precedes = |c: &ChunkPayloadData| {
            #[cfg(test)]
            self.ordering_comparisons
                .set(self.ordering_comparisons.get() + 1);
            sna32lt(c.tsn, chunk.tsn)
        };
        let idx = if self.chunks.last().is_none_or(&precedes) {
            self.chunks.len()
        } else {
            self.chunks.partition_point(precedes)
        };
        if idx < self.chunks.len() && self.chunks[idx].tsn == chunk.tsn {
            return false;
        }
        let previous = idx.checked_sub(1).and_then(|i| self.chunks.get(i));
        let next = self.chunks.get(idx);
        if let (Some(previous), Some(next)) = (previous, next) {
            self.discontinuities -= usize::from(!self.adjacent(previous.tsn, next.tsn));
        }
        if let Some(previous) = previous {
            self.discontinuities += usize::from(!self.adjacent(previous.tsn, chunk.tsn));
        }
        if let Some(next) = next {
            self.discontinuities += usize::from(!self.adjacent(chunk.tsn, next.tsn));
        }
        self.chunks.insert(idx, chunk);

        // Check if we now have a complete set
        self.is_complete()
    }

    fn adjacent(&self, previous: u32, next: u32) -> bool {
        #[cfg(test)]
        self.adjacency_checks.set(self.adjacency_checks.get() + 1);
        next == previous.wrapping_add(1)
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.chunks.first().is_some_and(|c| c.beginning_fragment)
            && self.chunks.last().is_some_and(|c| c.ending_fragment)
            && self.discontinuities == 0
    }
}

/// Endpoints of a contiguous incomplete unordered fragment run. Only endpoints
/// are indexed, so each arrival joins at most two runs without scanning them.
#[derive(Clone, Copy, Debug)]
struct UnorderedRun {
    first: u32,
    last: u32,
    beginning: bool,
    ending: bool,
}

impl UnorderedRun {
    fn insert(runs: &mut FxHashMap<u32, Self>, chunk: &ChunkPayloadData) -> Self {
        let mut run = Self {
            first: chunk.tsn,
            last: chunk.tsn,
            beginning: chunk.beginning_fragment,
            ending: chunk.ending_fragment,
        };
        if !run.beginning
            && let Some(left) = runs.get(&chunk.tsn.wrapping_sub(1)).copied()
            && left.last == chunk.tsn.wrapping_sub(1)
            && !left.ending
        {
            runs.remove(&left.first);
            runs.remove(&left.last);
            run.first = left.first;
            run.beginning = left.beginning;
        }
        if !run.ending
            && let Some(right) = runs.get(&chunk.tsn.wrapping_add(1)).copied()
            && right.first == chunk.tsn.wrapping_add(1)
            && !right.beginning
        {
            runs.remove(&right.first);
            runs.remove(&right.last);
            run.last = right.last;
            run.ending = right.ending;
        }
        if !(run.beginning && run.ending) {
            runs.insert(run.first, run);
            runs.insert(run.last, run);
        }
        run
    }
}

#[derive(Default, Debug)]
pub(crate) struct ReassemblyQueue {
    pub(crate) si: StreamId,
    /// Next ordered message to admit, independent of application reads.
    pub(crate) next_ssn: u16,
    /// Complete ordered messages admitted in sequence, including unread SSN wraps.
    pub(crate) ready_ordered: VecDeque<Chunks>,
    /// Future ordered messages awaiting their predecessors or more fragments.
    pub(crate) ordered: VecDeque<Chunks>,
    pub(crate) unordered: VecDeque<Chunks>,
    pub(crate) unordered_chunks: VecDeque<ChunkPayloadData>,
    unordered_runs: FxHashMap<u32, UnorderedRun>,
    pub(crate) n_bytes: usize,
    /// Monotonic counter stamped onto each reassembled message so `read` can tell which of a
    /// ready ordered and a ready unordered message arrived first.
    next_arrival: u64,
    #[cfg(test)]
    pub(crate) unordered_scan_steps: usize,
    #[cfg(test)]
    pub(crate) unordered_ordering_comparisons: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(crate) unordered_forward_steps: usize,
}

impl ReassemblyQueue {
    /// From RFC 4960 Sec 6.5:
    ///   The Stream Sequence Number in all the streams MUST start from 0 when
    ///   the association is Established.  Also, when the Stream Sequence
    ///   Number reaches the value 65535 the next Stream Sequence Number MUST
    ///   be set to 0.
    pub(crate) fn new(si: StreamId) -> Self {
        ReassemblyQueue {
            si,
            next_ssn: 0, // From RFC 4960 Sec 6.5:
            ready_ordered: VecDeque::new(),
            ordered: VecDeque::new(),
            unordered: VecDeque::new(),
            unordered_chunks: VecDeque::new(),
            unordered_runs: FxHashMap::default(),
            n_bytes: 0,
            next_arrival: 0,
            #[cfg(test)]
            unordered_scan_steps: 0,
            #[cfg(test)]
            unordered_ordering_comparisons: std::cell::Cell::new(0),
            #[cfg(test)]
            unordered_forward_steps: 0,
        }
    }

    pub(crate) fn push(&mut self, chunk: ChunkPayloadData) -> bool {
        if chunk.stream_identifier != self.si {
            return false;
        }

        if chunk.unordered {
            if chunk.beginning_fragment && chunk.ending_fragment {
                self.n_bytes += chunk.user_data.len();
                self.unordered.push_back(Chunks::new(
                    self.next_arrival,
                    0,
                    chunk.payload_type,
                    vec![chunk],
                ));
                self.next_arrival += 1;
                return true;
            }
            let precedes = |c: &ChunkPayloadData| {
                #[cfg(test)]
                self.unordered_ordering_comparisons
                    .set(self.unordered_ordering_comparisons.get() + 1);
                sna32lt(c.tsn, chunk.tsn)
            };
            let idx = if self.unordered_chunks.back().is_none_or(&precedes) {
                self.unordered_chunks.len()
            } else {
                self.unordered_chunks.partition_point(precedes)
            };
            if self
                .unordered_chunks
                .get(idx)
                .is_some_and(|c| c.tsn == chunk.tsn)
            {
                return false;
            }
            self.n_bytes += chunk.user_data.len();
            #[cfg(test)]
            {
                // At most two neighboring endpoints are inspected per arrival.
                self.unordered_scan_steps +=
                    usize::from(!chunk.beginning_fragment) + usize::from(!chunk.ending_fragment);
            }
            let run = UnorderedRun::insert(&mut self.unordered_runs, &chunk);
            self.unordered_chunks.insert(idx, chunk);
            if run.beginning && run.ending {
                let start = self
                    .unordered_chunks
                    .partition_point(|c| sna32lt(c.tsn, run.first));
                let count = run.last.wrapping_sub(run.first) as usize + 1;
                let chunks: Vec<_> = self.unordered_chunks.drain(start..start + count).collect();
                let arrival = self.next_arrival;
                self.next_arrival += 1;
                self.unordered
                    .push_back(Chunks::new(arrival, 0, chunks[0].payload_type, chunks));
                return true;
            }

            false
        } else {
            // This is an ordered chunk
            if sna16lt(chunk.stream_sequence_number, self.next_ssn) {
                return false;
            }

            self.n_bytes += chunk.user_data.len();

            // `ordered` is kept sorted by SSN, so binary-search for the chunk
            // set instead of scanning linearly (O(N) per arriving chunk when
            // the application drains slower than data arrives).
            let ssn = chunk.stream_sequence_number;
            let idx = self.ordered.partition_point(|s| sna16lt(s.ssn, ssn));
            if let Some(s) = self.ordered.get_mut(idx)
                && s.ssn == ssn
            {
                let was_complete = s.is_complete();
                let complete = s.push(chunk);
                if complete && !was_complete {
                    s.arrival = self.next_arrival;
                    self.next_arrival += 1;
                }
                self.promote_ordered();
                return complete;
            }

            // If not found, create a new chunkSet and insert it in SSN order
            // (this branch is only reached for ordered chunks).
            let mut cset = Chunks::new(self.next_arrival, ssn, chunk.payload_type, vec![]);
            let ok = cset.push(chunk);
            if ok {
                self.next_arrival += 1;
            }
            self.ordered.insert(idx, cset);
            self.promote_ordered();

            ok
        }
    }

    fn promote_ordered(&mut self) {
        while self
            .ordered
            .front()
            .is_some_and(|chunks| chunks.ssn == self.next_ssn && chunks.is_complete())
        {
            if let Some(chunks) = self.ordered.pop_front() {
                self.ready_ordered.push_back(chunks);
                self.next_ssn = self.next_ssn.wrapping_add(1);
            }
        }
    }

    pub(crate) fn is_readable(&self) -> bool {
        !self.ready_ordered.is_empty() || !self.unordered.is_empty()
    }

    pub(crate) fn read(&mut self) -> Option<Chunks> {
        let ordered_first = match (self.ready_ordered.front(), self.unordered.front()) {
            (Some(ordered), Some(unordered)) => ordered.arrival <= unordered.arrival,
            (Some(_), None) => true,
            _ => false,
        };
        let chunks = if ordered_first {
            self.ready_ordered.pop_front()?
        } else {
            self.unordered.pop_front()?
        };
        self.subtract_num_bytes(chunks.len());
        Some(chunks)
    }

    /// Use last_ssn to locate a chunkSet then remove it if the set has
    /// not been complete
    pub(crate) fn forward_tsn_for_ordered(&mut self, last_ssn: u16) {
        if sna16lt(last_ssn, self.next_ssn) {
            return;
        }
        while self
            .ordered
            .front()
            .is_some_and(|chunks| sna16lte(chunks.ssn, last_ssn))
        {
            if let Some(chunks) = self.ordered.pop_front() {
                if chunks.is_complete() {
                    self.ready_ordered.push_back(chunks);
                } else {
                    self.subtract_num_bytes(chunks.len());
                }
            }
        }
        self.next_ssn = last_ssn.wrapping_add(1);
        self.promote_ordered();
    }

    /// Remove all fragments in the unordered sets that contains chunks
    /// equal to or older than `new_cumulative_tsn`.
    /// We know all sets in the r.unordered are complete ones.
    /// Just remove chunks that are equal to or older than new_cumulative_tsn
    /// from the unordered_chunks
    pub(crate) fn forward_tsn_for_unordered(&mut self, new_cumulative_tsn: u32) {
        while let Some(chunk) = self.unordered_chunks.front() {
            #[cfg(test)]
            {
                self.unordered_forward_steps += 1;
            }
            if sna32gt(chunk.tsn, new_cumulative_tsn) {
                break;
            }
            if let Some(chunk) = self.unordered_chunks.pop_front() {
                self.subtract_num_bytes(chunk.user_data.len());
                if let Some(mut run) = self.unordered_runs.remove(&chunk.tsn) {
                    self.unordered_runs.remove(&run.first);
                    self.unordered_runs.remove(&run.last);
                    // A prefix cut can cross one retained run. Its suffix has
                    // no beginning; all later runs keep their existing index.
                    if sna32gt(run.last, new_cumulative_tsn) {
                        run.first = new_cumulative_tsn.wrapping_add(1);
                        run.beginning = false;
                        self.unordered_runs.insert(run.first, run);
                        self.unordered_runs.insert(run.last, run);
                    }
                }
            }
        }
    }

    pub(crate) fn subtract_num_bytes(&mut self, n_bytes: usize) {
        if self.n_bytes >= n_bytes {
            self.n_bytes -= n_bytes;
        } else {
            self.n_bytes = 0;
        }
    }

    pub(crate) fn get_num_bytes(&self) -> usize {
        self.n_bytes
    }
}
