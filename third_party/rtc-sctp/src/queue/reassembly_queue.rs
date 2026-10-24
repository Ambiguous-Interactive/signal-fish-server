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

        let mut buf = BytesMut::with_capacity(max_length);

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
        }
    }

    pub(crate) fn push(&mut self, chunk: ChunkPayloadData) -> bool {
        // Binary-search the insertion point (fragments are kept in TSN order),
        // which also detects duplicates -- instead of an O(n) dup scan plus a
        // full re-sort of every fragment on each push.
        let idx = self.chunks.partition_point(|c| sna32lt(c.tsn, chunk.tsn));
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
    pub(crate) next_ssn: u16,
    /// expected SSN for next ordered chunk
    ///
    /// `ordered`/`unordered` are consumed strictly from the front by `read`;
    /// `VecDeque` makes that O(1) instead of `Vec::remove(0)`'s full shift.
    pub(crate) ordered: VecDeque<Chunks>,
    pub(crate) unordered: VecDeque<Chunks>,
    pub(crate) unordered_chunks: Vec<ChunkPayloadData>,
    unordered_runs: FxHashMap<u32, UnorderedRun>,
    pub(crate) n_bytes: usize,
    /// Monotonic counter stamped onto each reassembled message so `read` can tell which of a
    /// ready ordered and a ready unordered message arrived first.
    next_arrival: u64,
    #[cfg(test)]
    pub(crate) unordered_scan_steps: usize,
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
            ordered: VecDeque::new(),
            unordered: VecDeque::new(),
            unordered_chunks: vec![],
            unordered_runs: FxHashMap::default(),
            n_bytes: 0,
            next_arrival: 0,
            #[cfg(test)]
            unordered_scan_steps: 0,
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
            let idx = self
                .unordered_chunks
                .partition_point(|c| sna32lt(c.tsn, chunk.tsn));
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

            ok
        }
    }

    pub(crate) fn is_readable(&self) -> bool {
        // Check unordered first
        if !self.unordered.is_empty() {
            // The chunk sets in self.unordered should all be complete.
            return true;
        }

        // Check ordered sets
        if !self.ordered.is_empty() {
            let cset = &self.ordered[0];
            if cset.is_complete() && sna16lte(cset.ssn, self.next_ssn) {
                return true;
            }
        }
        false
    }

    fn readable_unordered_chunks(&self) -> Option<&Chunks> {
        self.unordered.front()
    }

    fn readable_ordered_chunks(&self) -> Option<&Chunks> {
        let ordered = self.ordered.front();
        if let Some(chunks) = ordered {
            if !chunks.is_complete() {
                return None;
            }
            if sna16gt(chunks.ssn, self.next_ssn) {
                return None;
            }
            Some(chunks)
        } else {
            None
        }
    }

    pub(crate) fn read(&mut self) -> Option<Chunks> {
        let chunks = if let (Some(unordered_chunks), Some(ordered_chunks)) = (
            self.readable_unordered_chunks(),
            self.readable_ordered_chunks(),
        ) {
            if unordered_chunks.arrival < ordered_chunks.arrival {
                self.unordered.pop_front().unwrap()
            } else {
                if ordered_chunks.ssn == self.next_ssn {
                    self.next_ssn = self.next_ssn.wrapping_add(1);
                }
                self.ordered.pop_front().unwrap()
            }
        } else {
            // Check unordered first
            if !self.unordered.is_empty() {
                self.unordered.pop_front().unwrap()
            } else if !self.ordered.is_empty() {
                // Now, check ordered
                let chunks = &self.ordered[0];
                if !chunks.is_complete() {
                    return None;
                }
                if sna16gt(chunks.ssn, self.next_ssn) {
                    return None;
                }
                if chunks.ssn == self.next_ssn {
                    self.next_ssn = self.next_ssn.wrapping_add(1);
                }
                self.ordered.pop_front().unwrap()
            } else {
                return None;
            }
        };

        self.subtract_num_bytes(chunks.len());

        Some(chunks)
    }

    /// Use last_ssn to locate a chunkSet then remove it if the set has
    /// not been complete
    pub(crate) fn forward_tsn_for_ordered(&mut self, last_ssn: u16) {
        let num_bytes = self
            .ordered
            .iter()
            .filter(|s| sna16lte(s.ssn, last_ssn) && !s.is_complete())
            .fold(0, |n, s| {
                n + s.chunks.iter().fold(0, |acc, c| acc + c.user_data.len())
            });
        self.subtract_num_bytes(num_bytes);

        self.ordered
            .retain(|s| !sna16lte(s.ssn, last_ssn) || s.is_complete());

        // Finally, forward next_ssn
        if sna16lte(self.next_ssn, last_ssn) {
            self.next_ssn = last_ssn.wrapping_add(1);
        }
    }

    /// Remove all fragments in the unordered sets that contains chunks
    /// equal to or older than `new_cumulative_tsn`.
    /// We know all sets in the r.unordered are complete ones.
    /// Just remove chunks that are equal to or older than new_cumulative_tsn
    /// from the unordered_chunks
    pub(crate) fn forward_tsn_for_unordered(&mut self, new_cumulative_tsn: u32) {
        let mut last_idx: isize = -1;
        for (i, c) in self.unordered_chunks.iter().enumerate() {
            if sna32gt(c.tsn, new_cumulative_tsn) {
                break;
            }
            last_idx = i as isize;
        }
        if last_idx >= 0 {
            for i in 0..(last_idx + 1) as usize {
                self.subtract_num_bytes(self.unordered_chunks[i].user_data.len());
            }
            self.unordered_chunks.drain(..(last_idx + 1) as usize);
            // FORWARD TSN can cut through a run. Rebuild endpoints for the
            // retained suffix; it must not inherit a discarded beginning flag.
            self.unordered_runs.clear();
            for chunk in &self.unordered_chunks {
                UnorderedRun::insert(&mut self.unordered_runs, chunk);
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
