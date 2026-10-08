//! Building placement pipeline and production stations
//! (docs/mechanics/crafting/crafting-and-building.md, "Building system"
//! and "Production stations" sections).
//!
//! Flow: a build pagina sends `act(<id>)` on the menugrid (the pagina's
//! own ad string, e.g. "oven" — decoded from res/paginae/build/*.res in
//! lib/haven-res.jar). The server answers with the mapview `place` uimsg;
//! the client shows the ghost plob and answers `place (coord, button,
//! modflags)`. A valid commit spawns a construction-plan gob whose sdt
//! byte carries the build stage; `itemact` with a demanded material sinks
//! it into the plan, advancing the stage (OD_RES re-render, same pattern
//! as crop growth). The last material converts the plan into the finished
//! structure gob. Stations (oven) add a fuel store, a Light flower-menu
//! action, and a tick-driven job that applies the station quality
//! formula (2*q_item + q_station + q_fuel)/4.
//!
//! Material demands are server registry data. Legacy wiki numbers are
//! recorded in the mechanics doc; this server's demands use items its
//! economy actually produces (see the doc's implementation notes).

/// Registry entry for a placeable structure.
pub struct Buildable {
    /// Pagina ad string and `act` argument ("act("oven")").
    pub id: &'static str,
    /// Finished-gob resource (`gfx/terobjs/...`).
    pub res: &'static str,
    /// Tile snapping for the placement ghost (mapview `place` arg 2).
    pub on_tile: bool,
    /// Optional placement display radius (mapview `place` arg 3).
    pub place_radius: Option<i32>,
    /// Material demand: (item resource name, units). Order is the sink
    /// order shown to the player; any demanded material may arrive in any
    /// sequence of deliveries.
    pub demand: &'static [(&'static str, u32)],
    /// Structure hit points (OD_HEALTH quarters render damage).
    pub hp: i32,
    /// Number of construction stages the plan walks through as materials
    /// arrive (the sdt byte; final stage = complete).
    pub stages: u8,
    /// Production behavior after completion, if any.
    pub station: Option<StationSpec>,
}

/// Production behavior of a completed station gob.
pub struct StationSpec {
    /// Which station behavior the job loop and the itemact input
    /// dispatch follow (oven roasts meat, smelter melts ore).
    pub kind: StationKind,
    /// Items accepted as fuel by `itemact` (one unit per delivery).
    pub fuel: &'static [&'static str],
    /// Ticks (10 Hz) per production job.
    pub job_ticks: u32,
}

/// Station behavior families. The dispatch points: itemact input
/// acceptance (station_itemact + the relay path), the job-output label
/// mapping (craft::roast_result vs craft::smelt_result), and the output
/// drop's resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StationKind {
    /// Roasts raw meat (craft::ROAST_MAP); output keeps the meat
    /// resource, only the label changes.
    Oven,
    /// Melts ore into metal bars (craft::SMELT_MAP); output rides the
    /// mapped bar resource.
    Smelter,
}

/// Station input/output policy this server: the oven roasts any raw meat
/// label handled by `craft::ROAST_MAP` (the same chain as the hand-craft
/// `roast` recipe); one input item per job. Output drops beside the
/// station (legacy used an internal inventory widget; deviation recorded
/// in the mechanics doc).
/// The placeable registry. Ids match the ad strings of the pushed paginae
/// (paginae/act/build -> paginae/build/cons -> paginae/build/<id>).
pub const BUILDABLES: &[Buildable] = &[
    Buildable {
        id: "oven",
        res: "gfx/terobjs/oven",
        on_tile: true,
        place_radius: None,
        // Legacy demand: Brick x45 (bricks are not produced by this
        // server's item economy yet). Server demand: stone x2 + branch
        // x1 so the whole loop is playable from the starter kit and the
        // kit's leftovers (branch + meat) drive the station flow.
        demand: &[("gfx/invobjs/stone", 2), ("gfx/invobjs/branch", 1)],
        hp: 1200,
        stages: 2,
        station: Some(StationSpec {
            kind: StationKind::Oven,
            fuel: &["gfx/invobjs/branch"],
            job_ticks: 8,
        }),
    },
    Buildable {
        id: "smelter",
        res: "gfx/terobjs/smelter",
        on_tile: true,
        place_radius: None,
        // Legacy demand: Brick x35 + Stone x10 + Bar of Hard Metal x3.
        // Server demand keeps the same stone dominance (session-66 note:
        // the metal chain now lands, but the hard-metal demand would be
        // circular - the first smelter cannot require its own product;
        // the doc records the policy).
        demand: &[("gfx/invobjs/stone", 6), ("gfx/invobjs/branch", 4)],
        hp: 2500,
        stages: 3,
        // Session 66: the smelter is a PRODUCTION station. Fuel policy:
        // branch (the fuel this economy produces - the charcoal/kiln leg
        // does not exist yet, recorded in the mechanics doc). Job length:
        // 30 ticks (3 s) - smelting is deliberately slower than the
        // oven's 8-tick roast.
        station: Some(StationSpec {
            kind: StationKind::Smelter,
            fuel: &["gfx/invobjs/branch"],
            job_ticks: 30,
        }),
    },
    Buildable {
        id: "trough",
        res: "gfx/terobjs/trough",
        on_tile: true,
        place_radius: None,
        // Legacy:Food_Trough names no build materials in the docs; the
        // wiki page is lost. Server policy: a branch-dominated wooden
        // build (the trough is furniture-grade, cheaper than an oven),
        // single stage. Fodder is loaded by itemact after completion;
        // the doc's 2x1 lift-able footprint and trough-to-trough fodder
        // transfer stay out of scope until a lift mechanic exists
        // (documented in the livestock doc's Open questions).
        demand: &[("gfx/invobjs/branch", 4)],
        hp: 300,
        stages: 1,
        station: None,
    },
];

pub fn buildable_by_ad(id: &str) -> Option<usize> {
    BUILDABLES.iter().position(|b| b.id == id)
}

/// One credited material delivery: (item resource name, units, average
/// quality of that delivery). Qualities are snapshotted at delivery time
/// (building quality rule: per-type averages, weighted by units).
#[derive(Debug, Clone)]
pub struct Credited {
    pub res: &'static str,
    pub count: u32,
    pub ql_sum: u64,
}

impl Credited {
    pub fn avg_ql(&self) -> u8 {
        if self.count == 0 {
            10
        } else {
            (self.ql_sum / self.count as u64).min(255) as u8
        }
    }
}

/// Remaining demand for one material line given what is credited.
pub fn remaining(buildable: &Buildable, credited: &[Credited], res: &str) -> u32 {
    let demanded = buildable
        .demand
        .iter()
        .find(|(r, _)| *r == res)
        .map(|(_, c)| *c)
        .unwrap_or(0);
    let got = credited
        .iter()
        .find(|c| c.res == res)
        .map(|c| c.count)
        .unwrap_or(0);
    demanded.saturating_sub(got)
}

/// Total demanded units (all materials).
pub fn total_demand(buildable: &Buildable) -> u32 {
    buildable.demand.iter().map(|(_, c)| c).sum()
}

/// Total credited units so far.
pub fn total_credited(credited: &[Credited]) -> u32 {
    credited.iter().map(|c| c.count).sum()
}

/// Construction stage from credited units: proportional progress, the
/// last stage reachable only at completion. `stages` = 2 walks 0 -> 1.
pub fn stage_for(buildable: &Buildable, credited: &[Credited]) -> u8 {
    let total = total_demand(buildable);
    if total == 0 {
        return 0;
    }
    let done = total_credited(credited);
    let stages = buildable.stages.max(1);
    // Incomplete progress caps one below the final stage; completion is
    // an explicit state, not a rounding artifact.
    let raw = ((done as u64 * stages as u64) / total as u64) as u8;
    raw.min(stages - 1)
}

/// Structure quality from credited deliveries: average the qualities of
/// the delivered items per material type, then weighted-average the
/// per-type averages weighted by units (crafting-and-building.md,
/// "Completion"; Legacy:Quality buildable rule).
pub fn structure_quality(credited: &[Credited]) -> u8 {
    let mut qsum: u64 = 0;
    let mut units: u64 = 0;
    for c in credited {
        qsum += c.avg_ql() as u64 * c.count as u64;
        units += c.count as u64;
    }
    let q = qsum.checked_div(units).unwrap_or(10);
    q.clamp(1, 255) as u8
}

/// Station output quality: (2*q_item + q_station + q_fuel)/4
/// (crafting-and-building.md, station formula; fuel quality is the
/// average quality of the fuel burned for that job).
pub fn station_output_ql(q_item: u8, q_station: u8, q_fuel: u8) -> u8 {
    ((2 * q_item as u32 + q_station as u32 + q_fuel as u32) / 4).min(255) as u8
}

/// Fuel consumed per job (server policy; legacy per-item fuel values are
/// open data): one fuel unit per job.
pub const FUEL_PER_JOB: u32 = 1;

/// Server-side state of a construction plan gob. Credited deliveries
/// snapshot their quality at arrival time (the buildable quality rule).
#[derive(Debug, Clone)]
pub struct PlanState {
    /// Index into `BUILDABLES`.
    pub spec: u8,
    /// Tile the plan occupies (11x11 map units per tile).
    pub tile: (i32, i32),
    /// Materials sunk so far, one entry per material type.
    pub credited: Vec<Credited>,
}

impl PlanState {
    /// Whether every demand line is fully credited.
    pub fn complete(&self, buildable: &Buildable) -> bool {
        buildable
            .demand
            .iter()
            .all(|(res, _)| remaining(buildable, &self.credited, res) == 0)
    }
}

/// Server-side state of a finished production station.
#[derive(Debug, Clone)]
pub struct StationState {
    /// Index into `BUILDABLES`.
    pub spec: u8,
    /// Stored fuel units (delivered by itemact, one unit each).
    pub fuel: u32,
    /// Quality bookkeeping for burned fuel: sum and count of delivered
    /// fuel qualities (the average feeds the station output formula).
    pub fuel_ql_sum: u64,
    pub fuel_seen: u64,
    /// Loaded input item: (inventory resource idx, quality, display
    /// label). One slot per station (legacy ovens had four dough slots;
    /// one slot is this server's policy, documented).
    pub input: Option<(u16, u8, &'static str)>,
    /// Whether the station is lit (a job is running).
    pub lit: bool,
    /// Elapsed job ticks (10 Hz) toward `StationSpec::job_ticks`.
    pub progress: u32,
    /// Structure quality snapshotted at completion (the credited-material
    /// average; the station term of the output formula).
    pub quality: u8,
}

impl StationState {
    /// Average quality of fuel delivered so far (default Q10).
    pub fn fuel_quality(&self) -> u8 {
        if self.fuel_seen == 0 {
            return 10;
        }
        (self.fuel_ql_sum / self.fuel_seen).min(255) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oven() -> &'static Buildable {
        &BUILDABLES[0]
    }

    fn smelter() -> &'static Buildable {
        &BUILDABLES[1]
    }

    #[test]
    fn registry_ids_match_paginae() {
        // The pushed paginae advertise these ad strings; the client sends
        // act(<id>) verbatim, so both ids must resolve.
        assert!(buildable_by_ad("oven").is_some());
        assert!(buildable_by_ad("smelter").is_some());
        assert!(buildable_by_ad("nonexistent").is_none());
    }

    #[test]
    fn demand_uses_producible_items() {
        // Every demand line must name an item this server actually hands
        // out (starter kit / harvests / drops): the stone and branch
        // invobjs are the guaranteed-obtainable pair.
        for b in BUILDABLES {
            for (res, count) in b.demand {
                assert!(*count > 0, "{}: zero demand line", b.id);
                assert!(
                    *res == "gfx/invobjs/stone" || *res == "gfx/invobjs/branch",
                    "{}: demand {} not obtainable in this economy",
                    b.id,
                    res
                );
            }
            assert!(b.stages >= 1 && b.stages <= 8, "{}: stage count", b.id);
        }
    }

    #[test]
    fn remaining_tracks_demand_lines() {
        let b = oven();
        let credited = vec![Credited {
            res: "gfx/invobjs/stone",
            count: 2,
            ql_sum: 20,
        }];
        assert_eq!(remaining(b, &credited, "gfx/invobjs/stone"), 0);
        assert_eq!(remaining(b, &credited, "gfx/invobjs/branch"), 1);
        // Over-crediting is impossible by construction: remaining clamps.
        let over = vec![
            Credited {
                res: "gfx/invobjs/stone",
                count: 5,
                ql_sum: 50,
            },
            Credited {
                res: "gfx/invobjs/branch",
                count: 5,
                ql_sum: 50,
            },
        ];
        assert_eq!(remaining(b, &over, "gfx/invobjs/stone"), 0);
        assert_eq!(remaining(b, &over, "gfx/invobjs/branch"), 0);
        assert_eq!(remaining(smelter(), &credited, "gfx/invobjs/stone"), 4);
    }

    #[test]
    fn stages_advance_proportionally_and_cap_before_completion() {
        let b = oven(); // demand 2+1=3, stages 2
        let partial = vec![Credited {
            res: "gfx/invobjs/stone",
            count: 1,
            ql_sum: 10,
        }];
        // 1*2/3 = 0 (still the first stage).
        assert_eq!(stage_for(b, &partial), 0);
        // Two of three units: 2*2/3 = 1 (second stage).
        let two_thirds = vec![Credited {
            res: "gfx/invobjs/stone",
            count: 2,
            ql_sum: 20,
        }];
        assert_eq!(stage_for(b, &two_thirds), 1);
        // Nothing credited: stage 0.
        assert_eq!(stage_for(b, &[]), 0);
        // Full credit keeps the last stage (completion is separate).
        let full = vec![
            Credited {
                res: "gfx/invobjs/stone",
                count: 2,
                ql_sum: 20,
            },
            Credited {
                res: "gfx/invobjs/branch",
                count: 1,
                ql_sum: 10,
            },
        ];
        assert_eq!(stage_for(b, &full), 1);
    }

    #[test]
    fn plan_completion_requires_every_demand_line() {
        let b = oven();
        let only_stone = vec![Credited {
            res: "gfx/invobjs/stone",
            count: 2,
            ql_sum: 20,
        }];
        let mut plan = PlanState {
            spec: 0,
            tile: (0, 0),
            credited: only_stone.clone(),
        };
        assert!(!plan.complete(b));
        plan.credited.push(Credited {
            res: "gfx/invobjs/branch",
            count: 1,
            ql_sum: 10,
        });
        assert!(plan.complete(b));
    }

    #[test]
    fn structure_quality_averages_per_type_then_by_units() {
        // Stone q10 x2, branch q30 x2: per-type averages 10 and 30,
        // weighted by equal units -> 20.
        let credited = vec![
            Credited {
                res: "gfx/invobjs/stone",
                count: 2,
                ql_sum: 20,
            },
            Credited {
                res: "gfx/invobjs/branch",
                count: 2,
                ql_sum: 60,
            },
        ];
        assert_eq!(structure_quality(&credited), 20);
        // Unequal weights: stone q10 x1 + branch q30 x3 -> (10 + 90)/4=25.
        let credited = vec![
            Credited {
                res: "gfx/invobjs/stone",
                count: 1,
                ql_sum: 10,
            },
            Credited {
                res: "gfx/invobjs/branch",
                count: 3,
                ql_sum: 90,
            },
        ];
        assert_eq!(structure_quality(&credited), 25);
        // Empty credit: default Q10 (natural-object default).
        assert_eq!(structure_quality(&[]), 10);
    }

    #[test]
    fn station_formula_matches_doc() {
        // (2*q_item + q_station + q_fuel)/4 with doc example values.
        assert_eq!(station_output_ql(10, 10, 10), 10);
        assert_eq!(station_output_ql(40, 10, 10), 25);
        assert_eq!(station_output_ql(90, 50, 30), 65);
        // Clamp at 255.
        assert_eq!(station_output_ql(255, 255, 255), 255);
    }
}
