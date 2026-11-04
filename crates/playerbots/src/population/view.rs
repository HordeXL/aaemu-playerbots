//! The World panel: players and bots by continent and side, zones, heat squares and totals.
//!
//! [`world_view`] is a pure function over the bots' rows, the census and the server's numbers, so
//! the arithmetic is tested without a runtime. Two grids are used: 512 m squares for the heat the
//! map draws, and 128 m cells for the area the online bots watch (about what a bot sees).

use pb_web::{
    Alliance, Continent, HeatCell, PlayerSeen, RegionCell, ServerView, WorldTotals, WorldView,
    ZoneRow,
};
use rustc_hash::{FxHashMap, FxHashSet};

use super::census::{Seen, TTL_MS};
use super::region::Geo;

/// Side of a heat square, metres.
pub const HEAT_CELL_M: f32 = 512.0;
/// Side of a cell of the watched-area grid, metres (about what a bot sees around it).
pub const WATCH_CELL_M: f32 = 128.0;
/// The most heat squares sent.
pub const MAX_HEAT: usize = 400;
/// The most zone rows sent.
pub const MAX_ZONES: usize = 40;
/// The most players listed.
pub const MAX_PLAYERS_LISTED: usize = 50;

/// Whether a bot is in the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Offline,
    /// Logging in (or waiting to try again).
    Connecting,
    Online,
}

/// What the director and the World panel need of one bot; read from its hub slot without copying
/// any text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BotRow {
    pub id: u32,
    pub alliance: Alliance,
    pub continent: Continent,
    pub zone_key: u16,
    pub x: f32,
    pub y: f32,
    pub presence: Presence,
    /// Seconds in the world this session.
    pub online_secs: u32,
    /// The bot is doing something that is harmless to interrupt (see
    /// [`super::select::stop_rank`]).
    pub calm: bool,
}

/// The bots by side, as the director counts them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BotCounts {
    /// Bots in the world or logging in, per side `[Nuia, Haranya]`.
    pub by_side: [u32; 2],
    /// Bots in the world or logging in, all sides.
    pub total: u32,
    /// Bots logging in.
    pub connecting: u32,
}

/// Counts the bots that are in the world or on their way, per side.
pub fn count_bots(rows: &[BotRow]) -> BotCounts {
    let mut c = BotCounts::default();
    for r in rows {
        if r.presence == Presence::Offline {
            continue;
        }
        c.total += 1;
        if r.presence == Presence::Connecting {
            c.connecting += 1;
        }
        if let Some(i) = r.alliance.side() {
            c.by_side[i] += 1;
        }
    }
    c
}

/// What the world panel is made of.
pub struct WorldInputs<'a> {
    pub now_ms: u64,
    pub rows: &'a [BotRow],
    /// The players in view, most recent first (`Census::fresh`).
    pub players: &'a [(u32, Seen)],
    pub server: ServerView,
    pub geo: &'a dyn Geo,
}

/// Builds the World panel.
pub fn world_view(i: WorldInputs<'_>) -> WorldView {
    let mut cells: FxHashMap<(Continent, Alliance), RegionCell> = FxHashMap::default();
    let mut zones: FxHashMap<u16, ZoneRow> = FxHashMap::default();
    let mut heat: FxHashMap<(i32, i32), HeatCell> = FxHashMap::default();
    let mut watched: FxHashSet<(i32, i32)> = FxHashSet::default();

    let square = |v: f32, size: f32| (v / size).floor() as i32;
    let mut add =
        |continent: Continent, alliance: Alliance, zone_key: u16, x: f32, y: f32, player: bool| {
            let c = cells
                .entry((continent, alliance))
                .or_insert_with(|| RegionCell {
                    continent,
                    alliance,
                    ..RegionCell::default()
                });
            let z = zones.entry(zone_key).or_insert_with(|| ZoneRow {
                zone_key,
                continent,
                ..ZoneRow::default()
            });
            let (gx, gy) = (square(x, HEAT_CELL_M), square(y, HEAT_CELL_M));
            let h = heat.entry((gx, gy)).or_insert_with(|| HeatCell {
                x: (gx as f32 + 0.5) * HEAT_CELL_M,
                y: (gy as f32 + 0.5) * HEAT_CELL_M,
                players: 0,
                bots: 0,
            });
            if player {
                c.players_seen += 1;
                z.players_seen += 1;
                h.players += 1;
            } else {
                c.bots_online += 1;
                z.bots_online += 1;
                h.bots += 1;
            }
        };

    let mut bots_online = 0u32;
    for r in i.rows.iter().filter(|r| r.presence == Presence::Online) {
        if !(r.x.is_finite() && r.y.is_finite()) {
            continue;
        }
        bots_online += 1;
        add(r.continent, r.alliance, r.zone_key, r.x, r.y, false);
        let (cx, cy) = (square(r.x, WATCH_CELL_M), square(r.y, WATCH_CELL_M));
        for dx in -1..=1 {
            for dy in -1..=1 {
                watched.insert((cx + dx, cy + dy));
            }
        }
    }
    let players_seen = i.players.len() as u32;
    for (_, p) in i.players {
        add(
            p.continent,
            p.alliance,
            p.zone_key,
            p.pos[0],
            p.pos[1],
            true,
        );
    }

    let mut cells: Vec<RegionCell> = cells.into_values().collect();
    cells.sort_by_key(|c| (c.continent, c.alliance));

    let mut zones: Vec<ZoneRow> = zones
        .into_values()
        .map(|mut z| {
            z.name = if z.zone_key == 0 {
                "(outside the zone grid)".to_owned()
            } else {
                i.geo
                    .zone_name(z.zone_key)
                    .unwrap_or_else(|| format!("zone {}", z.zone_key))
            };
            z
        })
        .collect();
    zones.sort_by(|a, b| {
        (b.players_seen, b.bots_online, a.zone_key).cmp(&(
            a.players_seen,
            a.bots_online,
            b.zone_key,
        ))
    });
    zones.truncate(MAX_ZONES);

    let mut heat: Vec<HeatCell> = heat.into_values().collect();
    heat.sort_by(|a, b| {
        (b.players * 4 + b.bots)
            .cmp(&(a.players * 4 + a.bots))
            .then(a.x.total_cmp(&b.x))
            .then(a.y.total_cmp(&b.y))
    });
    heat.truncate(MAX_HEAT);

    let players: Vec<PlayerSeen> = i
        .players
        .iter()
        .take(MAX_PLAYERS_LISTED)
        .map(|(_, p)| PlayerSeen {
            name: p.name.to_string(),
            level: p.level,
            alliance: p.alliance,
            continent: p.continent,
            zone: if p.zone_key == 0 {
                String::new()
            } else {
                i.geo.zone_name(p.zone_key).unwrap_or_default()
            },
            x: p.pos[0],
            y: p.pos[1],
            age_secs: (i.now_ms.saturating_sub(p.at_ms) / 1000).min(u64::from(u32::MAX)) as u32,
            seen_by: p.by,
        })
        .collect();

    // the server's count of players: its online characters minus our bots (the ones the server
    // names as ours; without the character list, the bots that are in the world here)
    let players_server = match (i.server.online, i.server.online_bots) {
        (Some(all), Some(bots)) => Some(all.saturating_sub(bots)),
        (Some(all), None) => Some(all.saturating_sub(bots_online)),
        _ => None,
    };
    let totals = WorldTotals {
        players_seen,
        bots_online,
        bots_total: i.rows.len() as u32,
        players_server,
        players_unlocated: players_server.map(|s| s.saturating_sub(players_seen)),
        coverage_pct: players_server
            .filter(|s| *s > 0)
            .map(|s| (u64::from(players_seen.min(s)) * 100 / u64::from(s)) as u32),
        watched_km2: watched.len() as f32 * (WATCH_CELL_M * WATCH_CELL_M) / 1_000_000.0,
    };

    WorldView {
        at_ms: i.now_ms,
        ttl_secs: (TTL_MS / 1000) as u32,
        cells,
        zones,
        players,
        heat,
        totals,
        server: i.server,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::population::region::fake::Table;

    fn row(
        id: u32,
        alliance: Alliance,
        continent: Continent,
        x: f32,
        presence: Presence,
    ) -> BotRow {
        BotRow {
            id,
            alliance,
            continent,
            zone_key: if continent == Continent::East { 9 } else { 5 },
            x,
            y: 100.0,
            presence,
            online_secs: 0,
            calm: true,
        }
    }

    fn seen(
        name: &str,
        alliance: Alliance,
        continent: Continent,
        x: f32,
        at_ms: u64,
    ) -> (u32, Seen) {
        (
            name.len() as u32 * 100 + x as u32,
            Seen {
                name: name.into(),
                level: 20,
                pos: [x, 100.0],
                alliance,
                continent,
                zone_key: if continent == Continent::East { 9 } else { 5 },
                at_ms,
                by: 1,
            },
        )
    }

    #[test]
    fn bots_are_counted_per_side_online_or_on_their_way() {
        use Presence::*;
        let rows = [
            row(1, Alliance::Nuia, Continent::West, 10.0, Online),
            row(2, Alliance::Nuia, Continent::West, 10.0, Connecting),
            row(3, Alliance::Haranya, Continent::East, 1500.0, Online),
            row(4, Alliance::Haranya, Continent::East, 1500.0, Offline),
            row(5, Alliance::Unknown, Continent::Unknown, 0.0, Connecting),
            row(6, Alliance::Other, Continent::West, 0.0, Online),
        ];
        let c = count_bots(&rows);
        assert_eq!(c.by_side, [2, 1]);
        assert_eq!(c.total, 5, "the offline one is not counted");
        assert_eq!(c.connecting, 2);
        assert_eq!(count_bots(&[]), BotCounts::default());
    }

    #[test]
    fn the_world_is_split_by_continent_and_side() {
        use Presence::*;
        let rows = [
            row(1, Alliance::Nuia, Continent::West, 10.0, Online),
            row(2, Alliance::Nuia, Continent::West, 20.0, Online),
            row(3, Alliance::Haranya, Continent::East, 1500.0, Online),
            row(4, Alliance::Haranya, Continent::East, 1500.0, Offline),
        ];
        let players = [
            seen("Alice", Alliance::Haranya, Continent::East, 1520.0, 9_000),
            seen("Bob", Alliance::Haranya, Continent::East, 1530.0, 8_000),
            seen("Cleo", Alliance::Nuia, Continent::East, 1540.0, 7_000),
            seen("Dan", Alliance::Nuia, Continent::West, 30.0, 6_000),
        ];
        let v = world_view(WorldInputs {
            now_ms: 10_000,
            rows: &rows,
            players: &players,
            server: ServerView::default(),
            geo: &Table,
        });
        let cell = |c: Continent, a: Alliance| {
            v.cells
                .iter()
                .find(|x| x.continent == c && x.alliance == a)
                .map(|x| (x.players_seen, x.bots_online))
        };
        assert_eq!(cell(Continent::West, Alliance::Nuia), Some((1, 2)));
        assert_eq!(cell(Continent::East, Alliance::Haranya), Some((2, 1)));
        assert_eq!(
            cell(Continent::East, Alliance::Nuia),
            Some((1, 0)),
            "a Nuia visitor in the east"
        );
        assert_eq!(cell(Continent::West, Alliance::Haranya), None);
        assert_eq!(v.cells.len(), 3);
        assert_eq!(v.totals.players_seen, 4);
        assert_eq!((v.totals.bots_online, v.totals.bots_total), (3, 4));
        assert_eq!(v.ttl_secs, 60);
        // the rows come in a fixed order: west first
        assert_eq!(v.cells[0].continent, Continent::West);
        // zones: the east zone has three players and one bot
        assert_eq!(v.zones[0].zone_key, 9);
        assert_eq!((v.zones[0].players_seen, v.zones[0].bots_online), (3, 1));
        assert_eq!(v.zones[0].name, "zone9");
        assert_eq!(v.zones[0].continent, Continent::East);
        // the players, most recent first, with their age
        assert_eq!(v.players[0].name, "Alice");
        assert_eq!(v.players[0].age_secs, 1);
        assert_eq!(v.players[0].zone, "zone9");
        assert_eq!(v.players.len(), 4);
        // no server numbers without the API
        assert_eq!(v.totals.players_server, None);
        assert_eq!(v.totals.players_unlocated, None);
        assert_eq!(v.totals.coverage_pct, None);
    }

    #[test]
    fn heat_squares_add_up_and_rank_players_over_bots() {
        use Presence::*;
        let rows = [
            row(1, Alliance::Nuia, Continent::West, 10.0, Online),
            row(2, Alliance::Nuia, Continent::West, 12.0, Online),
            row(3, Alliance::Nuia, Continent::West, 14.0, Online),
            row(4, Alliance::Haranya, Continent::East, 5000.0, Online),
        ];
        let players = [seen(
            "Alice",
            Alliance::Haranya,
            Continent::East,
            5010.0,
            1_000,
        )];
        let v = world_view(WorldInputs {
            now_ms: 2_000,
            rows: &rows,
            players: &players,
            server: ServerView::default(),
            geo: &Table,
        });
        assert_eq!(v.heat.len(), 2);
        // one player and one bot (score 5) outrank three bots (score 3)
        assert_eq!((v.heat[0].players, v.heat[0].bots), (1, 1));
        assert_eq!((v.heat[1].players, v.heat[1].bots), (0, 3));
        // the centre of the square, in metres
        assert_eq!((v.heat[1].x, v.heat[1].y), (256.0, 256.0));
        assert_eq!(
            v.heat[0].x,
            (5000.0f32 / HEAT_CELL_M).floor() * HEAT_CELL_M + 256.0
        );
    }

    #[test]
    fn the_watched_area_counts_each_cell_once() {
        use Presence::*;
        // two bots in the same cell watch 3x3 cells between them
        let rows = [
            row(1, Alliance::Nuia, Continent::West, 10.0, Online),
            row(2, Alliance::Nuia, Continent::West, 11.0, Online),
        ];
        let v = world_view(WorldInputs {
            now_ms: 0,
            rows: &rows,
            players: &[],
            server: ServerView::default(),
            geo: &Table,
        });
        assert!(
            (v.totals.watched_km2 - 9.0 * 0.016384).abs() < 1e-5,
            "{}",
            v.totals.watched_km2
        );
        // a connecting or offline bot watches nothing
        let rows = [
            row(1, Alliance::Nuia, Continent::West, 10.0, Connecting),
            row(2, Alliance::Nuia, Continent::West, 11.0, Offline),
        ];
        let v = world_view(WorldInputs {
            now_ms: 0,
            rows: &rows,
            players: &[],
            server: ServerView::default(),
            geo: &Table,
        });
        assert_eq!(v.totals.watched_km2, 0.0);
        assert!(v.cells.is_empty());
    }

    #[test]
    fn the_server_count_gives_coverage_and_the_unlocated_remainder() {
        let players = [
            seen("Alice", Alliance::Haranya, Continent::East, 1520.0, 1_000),
            seen("Bob", Alliance::Haranya, Continent::East, 1530.0, 1_000),
        ];
        let mk = |online: u32, bots: u32| {
            world_view(WorldInputs {
                now_ms: 2_000,
                rows: &[],
                players: &players,
                server: ServerView {
                    configured: true,
                    reachable: true,
                    online: Some(online),
                    online_bots: Some(bots),
                    ..ServerView::default()
                },
                geo: &Table,
            })
            .totals
        };
        // 14 characters online, 10 of them ours: 4 players, 2 of them seen
        let t = mk(14, 10);
        assert_eq!(t.players_server, Some(4));
        assert_eq!(t.players_unlocated, Some(2));
        assert_eq!(t.coverage_pct, Some(50));
        // more seen than the server counts (a player logged out a moment ago): never above 100
        let t = mk(11, 10);
        assert_eq!(t.players_server, Some(1));
        assert_eq!(t.players_unlocated, Some(0));
        assert_eq!(t.coverage_pct, Some(100));
        // nobody online but us: coverage is not defined
        let t = mk(10, 10);
        assert_eq!(t.players_server, Some(0));
        assert_eq!(t.coverage_pct, None);
        // our bots on a server count that is lower than ours (a bot is still logging out)
        assert_eq!(mk(3, 5).players_server, Some(0));
    }

    #[test]
    fn without_the_character_list_our_online_bots_are_taken_off_the_server_count() {
        let row = |id: u32, presence: Presence| BotRow {
            id,
            alliance: Alliance::Nuia,
            continent: Continent::West,
            zone_key: 5,
            x: 10.0,
            y: 10.0,
            presence,
            online_secs: 0,
            calm: true,
        };
        let rows = [
            row(1, Presence::Online),
            row(2, Presence::Online),
            row(3, Presence::Offline),
        ];
        let t = world_view(WorldInputs {
            now_ms: 2_000,
            rows: &rows,
            players: &[],
            server: ServerView {
                configured: true,
                reachable: true,
                online: Some(7),
                online_bots: None,
                ..ServerView::default()
            },
            geo: &Table,
        })
        .totals;
        assert_eq!(
            t.players_server,
            Some(5),
            "7 characters, 2 of them our bots in the world"
        );
    }

    #[test]
    fn the_lists_are_bounded() {
        use Presence::*;
        let rows: Vec<BotRow> = (0..3000)
            .map(|i| BotRow {
                id: i,
                alliance: Alliance::Nuia,
                continent: Continent::West,
                zone_key: (i % 100) as u16 + 1,
                x: i as f32 * 600.0,
                y: 0.0,
                presence: Online,
                online_secs: 0,
                calm: true,
            })
            .collect();
        let players: Vec<(u32, Seen)> = (0..200)
            .map(|i| seen("P", Alliance::Nuia, Continent::West, i as f32, 1_000 - i))
            .collect();
        let v = world_view(WorldInputs {
            now_ms: 2_000,
            rows: &rows,
            players: &players,
            server: ServerView::default(),
            geo: &Table,
        });
        assert_eq!(v.heat.len(), MAX_HEAT);
        assert_eq!(v.zones.len(), MAX_ZONES);
        assert_eq!(v.players.len(), MAX_PLAYERS_LISTED);
        assert_eq!(v.totals.players_seen, 200, "the total counts all of them");
        // a position that is not a number does not break the grids
        let rows = [BotRow {
            x: f32::NAN,
            ..row(1, Alliance::Nuia, Continent::West, 0.0, Online)
        }];
        let v = world_view(WorldInputs {
            now_ms: 0,
            rows: &rows,
            players: &[],
            server: ServerView::default(),
            geo: &Table,
        });
        assert!(v.heat.is_empty());
    }
}
