//! Isolated allocation contract for player-name comparisons in rooms.

use signal_fish_server::protocol::validation::validate_player_name_uniqueness;
use signal_fish_server::protocol::PlayerInfo;
use std::collections::HashMap;
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// ASCII joins must preserve case-insensitive identity without normalization storage.
#[test]
fn ascii_player_name_checks_allocate_only_the_collision_error() {
    for count in [0_u128, 1, 16, 64, 255] {
        let players: HashMap<_, _> = (0..count)
            .map(|index| {
                let id = Uuid::from_u128(index + 1);
                (
                    id,
                    PlayerInfo {
                        id,
                        name: format!("Player{index}"),
                        is_authority: false,
                        is_ready: false,
                        connected_at: None,
                        connection_info: None,
                        epoch: None,
                        seq: None,
                        region_id: "test".into(),
                    },
                )
            })
            .collect();
        for candidate in ["NewPlayer", "pLaYeR0"] {
            let region = stats_alloc::Region::new(ALLOCATOR);
            let result = validate_player_name_uniqueness(candidate, &players);
            let stats = region.change();
            let collision = count > 0 && candidate == "pLaYeR0";
            assert_eq!(result.is_err(), collision);
            assert_eq!(
                stats.allocations,
                usize::from(collision),
                "count={count} candidate={candidate}: {stats:?}"
            );
            assert_eq!(
                stats.reallocations, 0,
                "count={count} candidate={candidate}"
            );
        }
    }
}
