/// Scalar flow-control state sampled without retaining payloads or changing protocol state.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct AssociationFlowControl {
    /// Current local receive credit, in bytes; sent in the next SACK.
    pub receive_window: u32,
    /// Configured local reassembly receive limit, in bytes.
    pub receive_buffer_limit: u32,
    /// Peer receive credit available to new DATA, after outstanding bytes.
    pub peer_receive_window: u32,
    /// Local congestion window, in bytes.
    pub congestion_window: u32,
    /// Payload bytes retained in the outbound flight queue.
    pub inflight_bytes: usize,
    /// Chunks retained in the outbound flight queue, including gap-acked entries.
    pub inflight_chunks: usize,
    /// Payload bytes waiting for first transmission selection.
    pub pending_bytes: usize,
    /// DATA chunks waiting for first transmission selection.
    pub pending_chunks: usize,
    /// Received TSNs retained above the cumulative receive point.
    pub received_gap_chunks: usize,
    /// Peer cumulative acknowledgment of local outbound TSNs.
    pub cumulative_ack: u32,
    /// Next local outbound transmission sequence number.
    pub next_tsn: u32,
    /// Local cumulative receive TSN.
    pub receive_cumulative_tsn: u32,
    /// First sent TSN that still requires peer acknowledgment.
    pub earliest_outstanding_tsn: Option<u32>,
    /// At least one stream retains a complete reassembled message.
    pub has_complete_queued_data: bool,
}

/// Identity and policy of a locally queued message. IDs belong to one association.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct OutboundMessageInfo {
    /// Zero-based local message identity; independent of wire TSN and SSN reuse.
    pub message_id: u64,
    /// Wire stream identifier.
    pub stream_id: u16,
    /// Local stream incarnation.
    pub stream_generation: u64,
    /// Wire stream sequence number (not a unique identity for unordered messages).
    pub sequence_number: u16,
    /// Whether the message uses unordered delivery.
    pub unordered: bool,
    /// Captured policy: reliable, lifetime, or retransmits.
    pub reliability: &'static str,
    /// A fragment was selected for transmission before this observation.
    /// Selection is not proof of packet serialization or UDP delivery.
    pub transmission_selected: bool,
}

/// Association statistics
#[derive(Default, Debug, Copy, Clone)]
pub struct AssociationStats {
    n_datas: u64,
    n_emitted_datas: u64,
    n_emitted_retransmissions: u64,
    n_abandoned_messages: u64,
    last_enqueued_message: Option<OutboundMessageInfo>,
    first_abandoned_message: Option<OutboundMessageInfo>,
    last_abandoned_message: Option<OutboundMessageInfo>,
    n_accepted_datas: u64,
    n_redundant_datas: u64,
    n_refused_datas: u64,
    n_sacks: u64,
    n_t3timeouts: u64,
    n_ack_timeouts: u64,
    n_fast_retrans: u64,
}

impl AssociationStats {
    pub(crate) fn note_enqueued_message(&mut self, message: OutboundMessageInfo) {
        self.last_enqueued_message = Some(message);
    }

    pub(crate) fn note_abandoned_message(&mut self, message: OutboundMessageInfo) {
        self.n_abandoned_messages = self.n_abandoned_messages.saturating_add(1);
        self.first_abandoned_message.get_or_insert(message);
        self.last_abandoned_message = Some(message);
    }

    pub(crate) fn note_emitted_data(&mut self, retransmission: bool) {
        self.n_emitted_datas = self.n_emitted_datas.saturating_add(1);
        if retransmission {
            self.n_emitted_retransmissions = self.n_emitted_retransmissions.saturating_add(1);
        }
    }

    /// DATA chunks serialized into outbound SCTP packets, including retries.
    /// This does not prove UDP transmission or remote receipt.
    pub fn get_num_emitted_datas(&self) -> u64 {
        self.n_emitted_datas
    }

    /// Emitted DATA chunks selected for retransmission.
    pub fn get_num_emitted_retransmissions(&self) -> u64 {
        self.n_emitted_retransmissions
    }

    /// Messages retired by partial reliability, counted once per identity.
    pub fn get_num_abandoned_messages(&self) -> u64 {
        self.n_abandoned_messages
    }

    /// Most recently enqueued message. No payload is retained.
    pub fn last_enqueued_message(&self) -> Option<OutboundMessageInfo> {
        self.last_enqueued_message
    }

    /// First abandoned message, retained even after later retirements.
    pub fn first_abandoned_message(&self) -> Option<OutboundMessageInfo> {
        self.first_abandoned_message
    }

    /// Most recently abandoned message. No payload is retained.
    pub fn last_abandoned_message(&self) -> Option<OutboundMessageInfo> {
        self.last_abandoned_message
    }

    /// Counts one DATA chunk received.
    pub fn inc_datas(&mut self) {
        self.n_datas += 1;
    }

    /// The number of DATA chunks received.
    pub fn get_num_datas(&mut self) -> u64 {
        self.n_datas
    }

    /// Counts new DATA admitted to the receive TSN space, not application delivery.
    pub(crate) fn inc_accepted_datas(&mut self) {
        self.n_accepted_datas = self.n_accepted_datas.saturating_add(1);
    }

    /// New DATA chunks admitted to the receive TSN space.
    pub fn get_num_accepted_datas(&self) -> u64 {
        self.n_accepted_datas
    }

    /// Counts DATA whose TSN was already received or passed by FORWARD TSN.
    pub(crate) fn inc_redundant_datas(&mut self) {
        self.n_redundant_datas = self.n_redundant_datas.saturating_add(1);
    }

    /// DATA chunks already covered by the receive TSN space.
    pub fn get_num_redundant_datas(&self) -> u64 {
        self.n_redundant_datas
    }

    /// Counts new DATA not admitted to the receive TSN space.
    pub(crate) fn inc_refused_datas(&mut self) {
        self.n_refused_datas = self.n_refused_datas.saturating_add(1);
    }

    /// New DATA chunks refused by stream admission or receive credit.
    pub fn get_num_refused_datas(&self) -> u64 {
        self.n_refused_datas
    }

    /// Counts one SACK chunk received.
    pub fn inc_sacks(&mut self) {
        self.n_sacks += 1;
    }

    /// The number of SACK chunks received.
    pub fn get_num_sacks(&mut self) -> u64 {
        self.n_sacks
    }

    /// Counts one T3-rtx retransmission timeout.
    pub fn inc_t3timeouts(&mut self) {
        self.n_t3timeouts += 1;
    }

    /// The number of T3-rtx retransmission timeouts, a signal of loss or a stalled path.
    pub fn get_num_t3timeouts(&mut self) -> u64 {
        self.n_t3timeouts
    }

    /// Counts one delayed-acknowledgement timeout.
    pub fn inc_ack_timeouts(&mut self) {
        self.n_ack_timeouts += 1;
    }

    /// The number of delayed-acknowledgement timeouts.
    pub fn get_num_ack_timeouts(&mut self) -> u64 {
        self.n_ack_timeouts
    }

    /// Counts one fast retransmission.
    pub fn inc_fast_retrans(&mut self) {
        self.n_fast_retrans += 1;
    }

    /// The number of fast retransmissions, triggered by SACK gap reports rather than a timeout.
    pub fn get_num_fast_retrans(&mut self) -> u64 {
        self.n_fast_retrans
    }

    /// Zeroes every counter.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
