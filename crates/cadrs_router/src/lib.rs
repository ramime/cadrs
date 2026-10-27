//! The PCB autorouter: a board's unconnected nets routed as tracks and vias.
//!
//! - [`grid`]: the board on a grid of cells per copper layer, each cell saying which nets may
//!   use it (copper grown by its clearance, holes, the edge, keep-outs), and where vias may go.
//! - [`search`]: the cheapest path from a net's tree to the nearest of its other pieces of
//!   copper, by A* over [`cadrs_router_kernels`]' moves (octilinear steps, layer preferences,
//!   vias) and congestion.
//! - [`negotiate`]: PathFinder's negotiated congestion (as OrthoRoute routes): every net routed,
//!   the ones whose copper runs over ground another claims rerouted with that ground dearer,
//!   pass by pass, until no two overlap.
//! - [`gpu`] (feature `gpu`): the same search on the GPU, many nets at once, from the kernels
//!   compiled by rust-gpu to SPIR-V.
//! - [`emit`]: routes back to tracks (from turn to turn) and through vias.
//!
//! Ideas and tests come from OrthoRoute (PathFinder on a lattice, GPU search), cuflow (a Lee
//! wavefront spreading over a cost grid, preferred directions per layer) and KiCadRoutingTools
//! (octilinear A* with vias, history costs).

// wgpu's types nest deep enough that proving the shared device `Sync` needs more than the
// default recursion limit.
#![recursion_limit = "256"]

use cadrs_eda::board::{Board, Track, Via};
use cadrs_eda::copper::{self, Item};
use cadrs_eda::units::{Nm, mm};

pub mod emit;
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod grid;
pub mod negotiate;
pub mod search;
pub mod smooth;

use grid::Grid;
use negotiate::{Job, Negotiation, Pricing, usable};

/// Which way tracks run on each layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Directions {
    /// Any way on any layer.
    #[default]
    Any,
    /// Alternating: the top layer horizontal, the next vertical, and so on (as cuflow's
    /// two-layer router, and a Manhattan lattice's layers).
    Alternate,
}

/// [`Backend::Auto`] searches on the GPU from this many nets.
pub const GPU_FROM: usize = 256;

/// Where the search runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    Gpu,
    /// The GPU for big jobs ([`GPU_FROM`] nets) when there is one (and the shader is built),
    /// else the CPU.
    #[default]
    Auto,
}

#[derive(Clone, Debug)]
pub struct Options {
    /// The grid's spacing.
    pub pitch: Nm,
    /// A via's cost, in tenths of a grid step.
    pub via_cost: u32,
    pub directions: Directions,
    /// What a step against its layer's direction adds, in tenths of a grid step.
    pub wrong_way: u32,
    /// Negotiation passes before giving up on the nets still in conflict.
    pub passes: u32,
    pub backend: Backend,
    /// The nets to route (empty: every net with unconnected copper).
    pub nets: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            pitch: mm(0.1),
            via_cost: 80,
            directions: Directions::Any,
            wrong_way: 30,
            passes: 40,
            backend: Backend::Auto,
            nets: vec![],
        }
    }
}

/// What the router made.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub tracks: Vec<Track>,
    pub vias: Vec<Via>,
    /// Nets left (partly) unconnected: no way through, or still in conflict.
    pub unrouted: Vec<String>,
    /// Nets still overlapping another when the passes ran out: their connections in conflict
    /// are left out (the rest are routed).
    pub conflicts: Vec<String>,
    pub passes: u32,
    /// The GPU's name when it searched.
    pub gpu: Option<String>,
}

/// Each net with copper in more than one piece, as a job: its pieces' terminal cells (where on
/// its copper a track may end), shortest nets first.
pub fn jobs(board: &Board, grid: &Grid, nets: &[String]) -> (Vec<Job>, Vec<String>) {
    let items = copper::items(board);
    let mut jobs = vec![];
    let mut cut_off = vec![];
    for name in &grid.nets {
        if !nets.is_empty() && !nets.contains(name) {
            continue;
        }
        let mine: Vec<copper::Copper> = items.iter().filter(|c| &c.net == name).cloned().collect();
        let groups: Vec<Vec<usize>> = copper::islands(&mine)
            .into_iter()
            .filter(|g| g.iter().any(|&i| copper::needs_connection(board, &mine[i])))
            .collect();
        if groups.len() < 2 {
            continue;
        }
        let net = grid.net_id(name);
        let mut terminals: Vec<Vec<u32>> = vec![];
        for g in &groups {
            let mut cells = vec![];
            for &i in g {
                let c = &mine[i];
                for (l, r) in &c.layers {
                    let Some(li) = grid.layer_index(*l) else {
                        continue;
                    };
                    let on: Vec<u32> = grid
                        .cells_in(r)
                        .into_iter()
                        .map(|(x, y)| grid.params.index(x, y, li as u32))
                        .filter(|&cell| usable(grid, net, cell))
                        .collect();
                    // A pad smaller than a cell: the usable cells nearest its anchor.
                    if on.is_empty() && matches!(c.item, Item::Pad(..)) {
                        cells.extend(near_anchor(grid, net, li as u32, &c.anchors));
                    }
                    cells.extend(on);
                }
            }
            cells.sort_unstable();
            cells.dedup();
            if !cells.is_empty() {
                terminals.push(cells);
            }
        }
        if terminals.len() < groups.len() {
            cut_off.push(name.clone());
        }
        if terminals.len() < 2 {
            continue;
        }
        let class = board.rules.class_of(name);
        jobs.push(Job {
            net,
            width: class.track_width.max(board.rules.min_track_width),
            via: class.via_diameter,
            clearance: grid.sizes.clearance,
            groups: terminals,
        });
    }
    // Short nets first (they have the fewest ways round), then by name: the same order always.
    let span = |j: &Job| {
        let p = &grid.params;
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for &c in j.groups.iter().flatten() {
            let (x, y, _) = p.coords(c);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        (x1 - x0) + (y1 - y0)
    };
    jobs.sort_by_key(|j| (span(j), j.net));
    (jobs, cut_off)
}

/// The usable cells within a few cells of `anchors`, the nearest ring found.
fn near_anchor(grid: &Grid, net: u32, l: u32, anchors: &[cadrs_eda::units::Pt]) -> Vec<u32> {
    let p = &grid.params;
    let mut out = vec![];
    for a in anchors {
        let Some((cx, cy)) = grid.cell_at(*a) else {
            continue;
        };
        for r in 0..4i32 {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dy.abs()) != r {
                        continue;
                    }
                    let (x, y) = (cx as i32 + dx, cy as i32 + dy);
                    if x >= 0 && y >= 0 && x < p.nx as i32 && y < p.ny as i32 {
                        let c = p.index(x as u32, y as u32, l);
                        if usable(grid, net, c) {
                            out.push(c);
                        }
                    }
                }
            }
            if !out.is_empty() {
                break;
            }
        }
    }
    out
}

/// The search's costs for `opts` on `grid`.
pub fn tune(grid: &mut Grid, opts: &Options) {
    let p = &mut grid.params;
    p.ortho = 10;
    p.diag = 14;
    p.via = opts.via_cost;
    p.wrong_way = opts.wrong_way;
    (p.h_layers, p.v_layers) = match opts.directions {
        Directions::Any => (0, 0),
        Directions::Alternate => {
            let (mut h, mut v) = (0, 0);
            for l in 0..p.nl.min(32) {
                if l % 2 == 0 {
                    h |= 1 << l;
                } else {
                    v |= 1 << l;
                }
            }
            (h, v)
        }
    };
}

/// How contested ground is priced pass by pass.
pub fn pricing(opts: &Options) -> Pricing {
    Pricing {
        present: 20,
        growth: 160,
        history: 30,
        passes: opts.passes,
    }
}

/// Routes the board's unconnected nets (or `opts.nets`). The board is not changed: put the
/// outcome down with [`apply`]. `CADRS_ROUTER_TRACE=1` prints where the time goes.
pub fn route(board: &Board, opts: &Options) -> Outcome {
    let trace = std::env::var_os("CADRS_ROUTER_TRACE").is_some();
    let t0 = web_time::Instant::now();
    let mut grid = Grid::build(board, opts.pitch);
    tune(&mut grid, opts);
    let t_grid = t0.elapsed();
    let (jobs, cut_off) = jobs(board, &grid, &opts.nets);
    if trace {
        let p = &grid.params;
        eprintln!(
            "router: grid {}x{}x{} ({} cells) in {:.2} s, {} nets to route (jobs in {:.2} s)",
            p.nx,
            p.ny,
            p.nl,
            p.cells(),
            t_grid.as_secs_f64(),
            jobs.len(),
            (t0.elapsed() - t_grid).as_secs_f64()
        );
    }
    let pricing = pricing(opts);
    let mut neg = Negotiation::new(&grid, jobs, &pricing);
    let mut out = Outcome::default();
    // Auto: the GPU for big jobs only (each batch costs it a few round trips; the CPU's A* is
    // quicker until there are hundreds of nets).
    #[cfg(feature = "gpu")]
    let gpu = match opts.backend {
        Backend::Cpu => None,
        Backend::Auto if neg.jobs.len() < GPU_FROM => None,
        _ => gpu::Gpu::shared()
            .ok()
            .filter(|g| g.slots(grid.params.cells()) > 0),
    };
    #[cfg(not(feature = "gpu"))]
    let gpu: Option<()> = None;
    let stuck = match &gpu {
        #[cfg(feature = "gpu")]
        Some(g) => {
            out.gpu = Some(g.name.clone());
            gpu::run(&mut neg, g, &pricing)
        }
        _ => neg.run_cpu(&pricing),
    };
    out.passes = neg.passes;
    smooth::all(&mut neg);
    if trace {
        eprintln!(
            "router: {} passes, {} stuck, {:.2} s in all",
            neg.passes,
            stuck.len(),
            t0.elapsed().as_secs_f64()
        );
    }
    let drill = |net: &str| board.rules.class_of(net).via_drill;
    for (j, job) in neg.jobs.iter().enumerate() {
        let name = grid.net_name(job.net).to_string();
        // A net that lost connections to settle the last conflicts keeps the rest.
        if stuck.contains(&j) {
            out.conflicts.push(name.clone());
        }
        let r = &neg.routes[j];
        let (t, v) = emit::copper(&grid, job, r, drill(&name));
        out.tracks.extend(t);
        out.vias.extend(v);
        if r.missing > 0 {
            out.unrouted.push(name);
        }
    }
    out.unrouted.extend(cut_off);
    out.unrouted.sort();
    out.unrouted.dedup();
    out
}

/// Puts a route's tracks and vias on the board.
pub fn apply(board: &mut Board, outcome: &Outcome) {
    board.tracks.extend(outcome.tracks.iter().cloned());
    board.vias.extend(outcome.vias.iter().cloned());
}
