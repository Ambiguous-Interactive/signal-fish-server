use super::*;
use crate::peer_connection::RTCPeerConnectionBuilder;
use crate::peer_connection::handler::sctp::established_test_association;
use std::time::Instant;

#[test]
fn association_stats_preserve_distinct_counters_without_resetting_them() {
    let mut stats = ::sctp::AssociationStats::default();
    stats.inc_datas();
    for _ in 0..2 {
        stats.inc_sacks();
    }
    for _ in 0..3 {
        stats.inc_t3timeouts();
    }
    for _ in 0..4 {
        stats.inc_ack_timeouts();
    }
    for _ in 0..5 {
        stats.inc_fast_retrans();
    }
    let snapshot = RTCSctpAssociationStats::from(stats);
    assert_eq!(snapshot, RTCSctpAssociationStats::from(stats));
    assert_eq!(snapshot.data_chunks_received, 1);
    assert_eq!(snapshot.sack_chunks_received, 2);
    assert_eq!(snapshot.retransmission_timeouts, 3);
    assert_eq!(snapshot.acknowledgement_timeouts, 4);
    assert_eq!(snapshot.fast_retransmissions, 5);
}

#[test]
fn association_stats_distinguish_an_absent_association_from_zero_data() {
    let mut pc = RTCPeerConnectionBuilder::new()
        .build(Instant::now())
        .unwrap();
    assert_eq!(
        RTCSctpTransport {
            peer_connection: &pc
        }
        .association_stats(),
        None
    );
    pc.sctp_transport_mut().sctp_associations.insert(
        ::sctp::AssociationHandle(0),
        established_test_association(RTCDtlsRole::Client),
    );
    let snapshot = RTCSctpTransport {
        peer_connection: &pc,
    }
    .association_stats()
    .unwrap();
    assert_eq!(snapshot.data_chunks_received, 0);
    assert_eq!(snapshot.data_chunks_accepted, 0);
    assert_eq!(snapshot.data_chunks_redundant, 0);
    assert_eq!(snapshot.data_chunks_refused, 0);
    assert_eq!(snapshot.retransmission_timeouts, 0);
    pc.sctp_transport_mut().sctp_associations.clear();
    assert_eq!(
        RTCSctpTransport {
            peer_connection: &pc
        }
        .association_stats(),
        None
    );
}
