//! Negotiated congestion (PathFinder, McMurchie and Ebeling, as OrthoRoute routes): every net
//! is routed as if alone, its route claims the ground around it (its track grown by a track
//! and a clearance, its vias by theirs, [`claim`]), and a net whose route runs over ground
//! another claims is in conflict. Each pass reroutes the nets in conflict with that ground
//! dearer: the present price of a claim rises pass by pass, and every cell fought over keeps a
//! history cost, so nets that can go elsewhere do and the one that can't keeps it. It ends
//! when no net is in conflict (or after the passes allowed; the rest are reported).
//!
//! A net of several pieces of copper is routed as a tree ([`route_net`]): from the first
//! piece to the nearest other, then from all it reaches so far to the nearest left, and so on.

use cadrs_eda::units::Nm;
use cadrs_router_kernels::passable;

use crate::grid::Grid;
use crate::search::{Costs, Searcher};

/// A net to connect: its pieces of copper as terminal cells, and its track width.
#[derive(Clone, Debug)]
pub struct Job {
    pub net: u32,
    pub width: Nm,
    pub via: Nm,
    pub clearance: Nm,
    pub groups: Vec<Vec<u32>>,
}

/// A net's route: each connection's cells, from where it starts on the tree to the piece it
/// reaches.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetRoute {
    pub paths: Vec<Vec<u32>>,
    /// Pieces it could not reach.
    pub missing: usize,
}

/// Offsets `(dx, dy)` of the cells whose centres are nearer than `r` (nm) to a cell's.
pub fn disc(r: Nm, pitch: Nm) -> Vec<(i32, i32)> {
    let k = (r / pitch.max(1)) as i32 + 1;
    let mut out = vec![];
    for dy in -k..=k {
        for dx in -k..=k {
            let d2 = ((dx as i64 * pitch) as f64).powi(2) + ((dy as i64 * pitch) as f64).powi(2);
            if d2 < (r as f64).powi(2) {
                out.push((dx, dy));
            }
        }
    }
    out
}

/// The cells a route claims, sorted: around each cell of its track (on that layer) closer than
/// a track width and a clearance (and half a cell, for diagonals passing between centres), and
/// around each via (on every layer) closer than a via, a track's half and a clearance.
pub fn claim(grid: &Grid, job: &Job, route: &NetRoute) -> Vec<u32> {
    let p = &grid.params;
    let track = disc(job.width + job.clearance + grid.pitch / 2, grid.pitch);
    let via = disc(
        job.via / 2 + job.width.max(job.via) / 2 + job.clearance + grid.pitch / 2,
        grid.pitch,
    );
    let mut out = vec![];
    let put = |x: u32, y: u32, l: u32, d: &[(i32, i32)], out: &mut Vec<u32>| {
        for (dx, dy) in d {
            let (cx, cy) = (x as i32 + dx, y as i32 + dy);
            if cx >= 0 && cy >= 0 && cx < p.nx as i32 && cy < p.ny as i32 {
                out.push(p.index(cx as u32, cy as u32, l));
            }
        }
    };
    for path in &route.paths {
        for (k, &c) in path.iter().enumerate() {
            let (x, y, l) = p.coords(c);
            put(x, y, l, &track, &mut out);
            if path.get(k + 1).is_some_and(|&n| p.coords(n).2 != l) {
                for m in 0..p.nl {
                    put(x, y, m, &via, &mut out);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The cells a route's copper is on (its tracks' cells, and at each via every layer), where
/// another net's claim is a conflict.
pub fn copper_cells(grid: &Grid, route: &NetRoute) -> Vec<u32> {
    let p = &grid.params;
    let mut out = vec![];
    for path in &route.paths {
        for (k, &c) in path.iter().enumerate() {
            out.push(c);
            let (x, y, l) = p.coords(c);
            if path.get(k + 1).is_some_and(|&n| p.coords(n).2 != l) {
                out.extend((0..p.nl).map(|m| p.index(x, y, m)));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Routes `job` as a tree on the CPU: from its first piece, each time to the nearest piece not
/// yet reached.
pub fn route_net(search: &mut Searcher, costs: &Costs, job: &Job) -> NetRoute {
    let mut route = NetRoute::default();
    let Some((first, rest)) = job.groups.split_first() else {
        return route;
    };
    let mut tree: Vec<u32> = first.clone();
    let mut left: Vec<&Vec<u32>> = rest.iter().collect();
    while !left.is_empty() {
        let targets: Vec<u32> = left.iter().flat_map(|g| g.iter().copied()).collect();
        let Some(path) = search.search(costs, job.net, &tree, &targets) else {
            route.missing = left.len();
            break;
        };
        let end = *path.last().unwrap();
        let k = left.iter().position(|g| g.contains(&end)).unwrap();
        tree.extend(path.iter().copied());
        tree.extend(left.remove(k).iter().copied());
        route.paths.push(path);
    }
    route
}

/// How the passes price contested ground.
#[derive(Clone, Copy, Debug)]
pub struct Pricing {
    /// The first pass's price of a claim, and the factor it grows by each pass (×100).
    pub present: u32,
    pub growth: u32,
    /// What each pass in conflict adds to a contested cell for good.
    pub history: u32,
    pub passes: u32,
}

/// The negotiation's state: each net's route and claim, how many claim each cell, and the
/// history of the fights.
pub struct Negotiation<'a> {
    pub grid: &'a Grid,
    pub jobs: Vec<Job>,
    pub routes: Vec<NetRoute>,
    claims: Vec<Vec<u32>>,
    pub occ: Vec<u16>,
    pub hist: Vec<u32>,
    pub present: u32,
    pub passes: u32,
}

impl<'a> Negotiation<'a> {
    pub fn new(grid: &'a Grid, jobs: Vec<Job>, pricing: &Pricing) -> Negotiation<'a> {
        let n = grid.params.cells() as usize;
        let k = jobs.len();
        Negotiation {
            grid,
            jobs,
            routes: vec![NetRoute::default(); k],
            claims: vec![vec![]; k],
            occ: vec![0; n],
            hist: vec![0; n],
            present: pricing.present,
            passes: 0,
        }
    }

    /// Takes job `j`'s route off the board (its claim no longer counts).
    pub fn rip(&mut self, j: usize) {
        for &c in &self.claims[j] {
            self.occ[c as usize] = self.occ[c as usize].saturating_sub(1);
        }
        self.claims[j].clear();
        self.routes[j] = NetRoute::default();
    }

    /// Puts route `r` down as job `j`'s.
    pub fn put(&mut self, j: usize, r: NetRoute) {
        let claim = claim(self.grid, &self.jobs[j], &r);
        for &c in &claim {
            self.occ[c as usize] = self.occ[c as usize].saturating_add(1);
        }
        self.claims[j] = claim;
        self.routes[j] = r;
    }

    pub fn costs(&self) -> Costs<'_> {
        Costs {
            grid: self.grid,
            occ: &self.occ,
            hist: &self.hist,
            present: self.present,
        }
    }

    /// The jobs in conflict (their copper on ground another claims), and the cells fought over.
    pub fn conflicts(&self) -> (Vec<usize>, Vec<u32>) {
        let mut nets = vec![];
        let mut cells = vec![];
        for (j, r) in self.routes.iter().enumerate() {
            let mine = &self.claims[j];
            let mut hit = false;
            for c in copper_cells(self.grid, r) {
                let own = u16::from(mine.binary_search(&c).is_ok());
                if self.occ[c as usize] > own {
                    hit = true;
                    cells.push(c);
                }
            }
            if hit {
                nets.push(j);
            }
        }
        cells.sort_unstable();
        cells.dedup();
        (nets, cells)
    }

    /// When the passes run out: the connections still in conflict taken up, the net with the
    /// most copper over others' ground first, until nothing overlaps. A connection that branches
    /// off one taken up goes with it; the net's other connections stay. Returns the nets that
    /// lost connections.
    pub fn settle(&mut self) -> Vec<usize> {
        let mut out = vec![];
        loop {
            let over = |neg: &Self, j: usize, cells: Vec<u32>| {
                let mine = &neg.claims[j];
                cells
                    .into_iter()
                    .filter(|&c| neg.occ[c as usize] > u16::from(mine.binary_search(&c).is_ok()))
                    .count()
            };
            let worst = (0..self.routes.len())
                .map(|j| {
                    (
                        over(self, j, copper_cells(self.grid, &self.routes[j])),
                        std::cmp::Reverse(j),
                    )
                })
                .filter(|(n, _)| *n > 0)
                .max();
            let Some((_, std::cmp::Reverse(j))) = worst else {
                return out;
            };
            let route = self.routes[j].clone();
            // Its connections in conflict, and those starting on one of them.
            let mut gone: Vec<bool> = route
                .paths
                .iter()
                .map(|p| {
                    over(
                        self,
                        j,
                        copper_cells(
                            self.grid,
                            &NetRoute {
                                paths: vec![p.clone()],
                                missing: 0,
                            },
                        ),
                    ) > 0
                })
                .collect();
            for k in 0..route.paths.len() {
                let start = route.paths[k][0];
                if !gone[k] && (0..k).any(|i| gone[i] && route.paths[i].contains(&start)) {
                    gone[k] = true;
                }
            }
            let kept = NetRoute {
                paths: route
                    .paths
                    .iter()
                    .zip(&gone)
                    .filter(|(_, g)| !**g)
                    .map(|(p, _)| p.clone())
                    .collect(),
                missing: route.missing + gone.iter().filter(|g| **g).count(),
            };
            self.rip(j);
            self.put(j, kept);
            if !out.contains(&j) {
                out.push(j);
            }
        }
    }

    /// After a pass in conflict: the fought-over cells dearer for good, a claim dearer now.
    pub fn reprice(&mut self, cells: &[u32], pricing: &Pricing) {
        for &c in cells {
            self.hist[c as usize] = self.hist[c as usize].saturating_add(pricing.history);
        }
        self.present =
            (self.present as u64 * pricing.growth as u64 / 100).min(u32::MAX as u64 / 4) as u32;
    }

    /// The CPU passes: route every net, then reroute those in conflict until none is (or the
    /// passes run out). Returns the jobs taken up to leave no conflict ([`Negotiation::settle`]).
    pub fn run_cpu(&mut self, pricing: &Pricing) -> Vec<usize> {
        let mut search = Searcher::default();
        let mut todo: Vec<usize> = (0..self.jobs.len()).collect();
        let trace = std::env::var_os("CADRS_ROUTER_TRACE").is_some();
        for pass in 0..pricing.passes.max(1) {
            self.passes = pass + 1;
            let t = web_time::Instant::now();
            for &j in &todo {
                self.rip(j);
                let r = route_net(&mut search, &self.costs(), &self.jobs[j]);
                self.put(j, r);
            }
            let (nets, cells) = self.conflicts();
            if trace {
                eprintln!(
                    "router: pass {} routed {} nets in {:.2} s, {} in conflict over {} cells",
                    pass + 1,
                    todo.len(),
                    t.elapsed().as_secs_f64(),
                    nets.len(),
                    cells.len()
                );
            }
            if nets.is_empty() {
                return vec![];
            }
            self.reprice(&cells, pricing);
            todo = nets;
        }
        self.settle()
    }
}

/// Whether `net` may stand on cell `c` (a terminal it can reach).
pub fn usable(grid: &Grid, net: u32, c: u32) -> bool {
    passable(grid.owner[c as usize], net)
}
