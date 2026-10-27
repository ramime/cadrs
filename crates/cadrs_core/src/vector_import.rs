//! Vector drawings as sketch geometry (issue #9): SVG, EPS and PostScript, PDF and Illustrator
//! files, beside DXF and DWG ([`crate::dxf_import`]). Dropped on the window or picked in the
//! sketch's Insert panel, each becomes curves that share their end points, so closed shapes
//! make regions to extrude.
//!
//! - **SVG** is read here (`usvg` resolves its styles, transforms, `<use>`s and shapes into
//!   paths): straight segments become lines; curves that are circular arcs (as every circle,
//!   rounded corner and arc command is written) become arcs, a whole turn of them a circle;
//!   other curves stay exact cubic Béziers. Sizes are SVG's own (96 px to the inch, so a drawing
//!   sized in mm comes in at that size); Y is flipped up. A path drawn twice (filled, then
//!   stroked) comes in once; texts are left out.
//! - **EPS, PS, PDF, AI** go through external converters to SVG first: Ghostscript (EPS and
//!   PostScript to PDF) and Poppler's `pdftocairo` (PDF to SVG; an Illustrator file is a PDF).
//!   Only a PDF's first page is read.
//! - **DXF, DWG**: [`crate::dxf_import`], in the units the file says it is in.

use std::path::{Path, PathBuf};

use resvg::usvg;

use cadrs_sketch::{Sketch, Vec2};

use crate::dxf_import::{Builder, DxfUnits, ImportReport, JOIN};

/// A vector file's kind, by its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorFormat {
    Dxf,
    Dwg,
    Svg,
    /// EPS or PostScript.
    PostScript,
    /// PDF or Illustrator.
    Pdf,
}

impl VectorFormat {
    /// Every extension read (lower case).
    pub const EXTENSIONS: [&'static str; 8] =
        ["dxf", "dwg", "svg", "svgz", "eps", "ps", "pdf", "ai"];

    pub fn of_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "dxf" => Self::Dxf,
            "dwg" => Self::Dwg,
            "svg" | "svgz" => Self::Svg,
            "eps" | "ps" => Self::PostScript,
            "pdf" | "ai" => Self::Pdf,
            _ => return None,
        })
    }
}

/// Says how to get the converters EPS, PostScript, PDF and Illustrator files need.
pub const CONVERTER_HINT: &str = "reading EPS, PostScript, PDF and Illustrator files needs Ghostscript (gs) and Poppler (pdftocairo) installed";

/// A vector file's curves as sketch geometry (mm): at the file's own origin, or (`file_origin`
/// false) centred on the sketch origin. `units` overrides what a DXF or DWG says it is in.
pub fn read_sketch(
    path: &Path,
    units: Option<DxfUnits>,
    file_origin: bool,
) -> Result<(Sketch, ImportReport), String> {
    let format = VectorFormat::of_path(path)
        .ok_or_else(|| format!("{}: not a vector drawing", path.display()))?;
    let (mut s, r) = match format {
        VectorFormat::Dxf | VectorFormat::Dwg => {
            let d = crate::dxf_import::read_file(path)?;
            let units = units.unwrap_or_else(|| DxfUnits::of_file(d.unit_mm));
            return Ok(crate::dxf_import::sketch_of(&d, units, file_origin));
        }
        VectorFormat::Svg => {
            let bytes = cadrs_fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            sketch_of_svg(&bytes)?
        }
        VectorFormat::PostScript | VectorFormat::Pdf => {
            sketch_of_svg(converted_svg(path, format)?.as_bytes())?
        }
    };
    if !file_origin {
        centre(&mut s);
    }
    Ok((s, r))
}

/// A sketch's geometry's bounds (lowest and highest corner), if it has any curves.
pub fn bounds(s: &Sketch) -> Option<(Vec2, Vec2)> {
    let (mut lo, mut hi) = (Vec2::new(f64::MAX, f64::MAX), Vec2::new(f64::MIN, f64::MIN));
    for id in s.curves.keys() {
        for p in cadrs_sketch::hit::curve_polyline(s, id) {
            lo = Vec2::new(lo.x.min(p.x), lo.y.min(p.y));
            hi = Vec2::new(hi.x.max(p.x), hi.y.max(p.y));
        }
    }
    (lo.x <= hi.x).then_some((lo, hi))
}

/// Moves a sketch's geometry so its bounds' centre is on the origin.
pub fn centre(s: &mut Sketch) {
    let Some((lo, hi)) = bounds(s) else { return };
    let shift = Vec2::new(-(lo.x + hi.x) / 2.0, -(lo.y + hi.y) / 2.0);
    for p in s.points.values_mut() {
        p.pos += shift;
    }
}

/// An SVG drawing's paths as sketch geometry (see the module docs), at the drawing's top-left
/// corner, Y up.
pub fn sketch_of_svg(data: &[u8]) -> Result<(Sketch, ImportReport), String> {
    let tree = usvg::Tree::from_data(data, &usvg::Options::default())
        .map_err(|e| format!("not an SVG drawing: {e}"))?;
    let mut r = Reader {
        b: Builder::default(),
        seen: std::collections::HashSet::new(),
    };
    r.group(tree.root());
    Ok((r.b.s, r.b.r))
}

/// Millimetres per SVG pixel.
const PX_MM: f64 = 25.4 / 96.0;

/// How far a curve may stray from a circle (or a line) and still be read as one (mm).
const FIT: f64 = 1e-3;

/// The shortest chord, as a share of the radius, of a piece read as an arc (2·sin 5°: a 10° turn).
const MIN_TURN_CHORD: f64 = 0.174;

/// The same for a circle, as a share of its radius: one cubic written for a quarter turn (as
/// SVG's arcs are) is off by up to 0.027 % of the radius.
const FIT_RADIUS: f64 = 5e-4;

struct Reader {
    b: Builder,
    /// The paths read so far (rounded points), so a shape drawn twice comes in once.
    seen: std::collections::HashSet<Vec<(i64, i64)>>,
}

/// A run of circular pieces about one centre, being merged into one arc (or a circle).
struct ArcRun {
    c: Vec2,
    r: f64,
    from: Vec2,
    to: Vec2,
    /// Signed sweep so far (radians, counter-clockwise positive).
    sweep: f64,
}

impl Reader {
    fn group(&mut self, g: &usvg::Group) {
        for n in g.children() {
            match n {
                usvg::Node::Group(g) => self.group(g),
                usvg::Node::Path(p) if p.fill().is_some() || p.stroke().is_some() => self.path(p),
                usvg::Node::Path(_) => {}
                usvg::Node::Text(_) | usvg::Node::Image(_) => self.b.r.skipped += 1,
            }
        }
    }

    fn path(&mut self, p: &usvg::Path) {
        use usvg::tiny_skia_path::PathSegment as S;
        let t = p.abs_transform();
        let pt = |q: usvg::tiny_skia_path::Point| {
            let mut q = q;
            t.map_point(&mut q);
            Vec2::new(q.x as f64 * PX_MM, -(q.y as f64) * PX_MM)
        };
        let segs: Vec<S> = p.data().segments().collect();
        let key: Vec<(i64, i64)> = segs
            .iter()
            .flat_map(|s| match *s {
                S::MoveTo(a) | S::LineTo(a) => vec![a],
                S::QuadTo(c, a) => vec![c, a],
                S::CubicTo(c1, c2, a) => vec![c1, c2, a],
                S::Close => vec![],
            })
            .map(|q| {
                let v = pt(q);
                ((v.x / JOIN).round() as i64, (v.y / JOIN).round() as i64)
            })
            .collect();
        if !self.seen.insert(key) {
            return;
        }
        let (mut start, mut last) = (Vec2::ZERO, Vec2::ZERO);
        let mut run: Option<ArcRun> = None;
        for s in segs {
            match s {
                S::MoveTo(a) => {
                    self.flush(&mut run);
                    (start, last) = (pt(a), pt(a));
                }
                S::LineTo(a) => {
                    self.flush(&mut run);
                    let a = pt(a);
                    self.b.line(last, a);
                    last = a;
                }
                S::QuadTo(c, a) => {
                    let (c, a) = (pt(c), pt(a));
                    // The same curve as a cubic.
                    let (c1, c2) = (last + (c - last) * (2.0 / 3.0), a + (c - a) * (2.0 / 3.0));
                    self.cubic(&mut run, last, c1, c2, a);
                    last = a;
                }
                S::CubicTo(c1, c2, a) => {
                    let (c1, c2, a) = (pt(c1), pt(c2), pt(a));
                    self.cubic(&mut run, last, c1, c2, a);
                    last = a;
                }
                S::Close => {
                    self.flush(&mut run);
                    if last.distance(start) > JOIN {
                        self.b.line(last, start);
                    }
                    last = start;
                }
            }
        }
        self.flush(&mut run);
    }

    /// A cubic piece: a line if it is straight, part of an arc if circular, else a Bézier.
    fn cubic(&mut self, run: &mut Option<ArcRun>, a: Vec2, c1: Vec2, c2: Vec2, b: Vec2) {
        if off_line(a, b, c1) <= FIT && off_line(a, b, c2) <= FIT {
            self.flush(run);
            return self.b.line(a, b);
        }
        let Some((c, r)) = circular(a, c1, c2, b) else {
            self.flush(run);
            return self.b.bezier(a, c1, c2, b);
        };
        let sweep = (a - c).cross(b - c).atan2((a - c).dot(b - c));
        // (Each piece's centre is found from three of its points, so pieces of one circle,
        // written with rounded numbers, agree only to a few times the fit.)
        let same = 4.0 * FIT.max(r * FIT_RADIUS);
        if let Some(arc) = run
            && arc.c.distance(c) <= same
            && (arc.r - r).abs() <= same
            && arc.to.distance(a) <= JOIN
            && arc.sweep.signum() == sweep.signum()
        {
            arc.to = b;
            arc.sweep += sweep;
            return;
        }
        self.flush(run);
        *run = Some(ArcRun {
            c,
            r,
            from: a,
            to: b,
            sweep,
        });
    }

    /// Adds a run of circular pieces: a circle if it goes all the way round, else one arc.
    fn flush(&mut self, run: &mut Option<ArcRun>) {
        let Some(arc) = run.take() else { return };
        if arc.sweep.abs() > std::f64::consts::TAU - 1e-3 && arc.from.distance(arc.to) <= JOIN {
            return self.b.circle(arc.c, arc.r);
        }
        if arc.sweep > 0.0 {
            self.b.arc(arc.c, arc.from, arc.to)
        } else {
            self.b.arc(arc.c, arc.to, arc.from)
        }
    }
}

/// How far `p` is from the line through `a` and `b` (from `a` when they meet).
fn off_line(a: Vec2, b: Vec2, p: Vec2) -> f64 {
    let d = b - a;
    let len = d.length();
    if len <= JOIN {
        p.distance(a)
    } else {
        (p - a).cross(d).abs() / len
    }
}

fn bezier_at(a: Vec2, c1: Vec2, c2: Vec2, b: Vec2, u: f64) -> Vec2 {
    let v = 1.0 - u;
    a * (v * v * v) + c1 * (3.0 * v * v * u) + c2 * (3.0 * v * u * u) + b * (u * u * u)
}

/// The circle a cubic piece follows (centre, radius), if it is a circular arc: the circle
/// through its ends and middle, which its quarter points must lie on too.
fn circular(a: Vec2, c1: Vec2, c2: Vec2, b: Vec2) -> Option<(Vec2, f64)> {
    let m = bezier_at(a, c1, c2, b, 0.5);
    let (c, r) = circle_through(a, m, b)?;
    // A shallow curve fits some huge circle (a radius of metres over a few mm, its centre far
    // off the drawing): only pieces that turn at least ~10° are arcs (rounded corners and
    // circles are written in quarter turns), the rest stay Béziers.
    if a.distance(b) <= JOIN || a.distance(b) < MIN_TURN_CHORD * r {
        return None;
    }
    let on = |u: f64| (bezier_at(a, c1, c2, b, u).distance(c) - r).abs() <= FIT.max(r * FIT_RADIUS);
    [0.125, 0.25, 0.375, 0.625, 0.75, 0.875]
        .into_iter()
        .all(on)
        .then_some((c, r))
}

fn circle_through(a: Vec2, b: Vec2, c: Vec2) -> Option<(Vec2, f64)> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() < 1e-12 {
        return None;
    }
    let (a2, b2, c2) = (a.dot(a), b.dot(b), c.dot(c));
    let o = Vec2::new(
        (a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d,
        (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d,
    );
    Some((o, o.distance(a)))
}

/// A program on this machine (`PATH`), also as `name.exe`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|d| [d.join(name), d.join(format!("{name}.exe"))])
        .find(|f| cadrs_fs::is_file(f))
}

fn run(mut c: std::process::Command, what: &str) -> Result<(), String> {
    let out = c.output().map_err(|e| format!("cannot run {what}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(format!(
        "{what} failed: {}",
        err.lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
    ))
}

/// An EPS, PostScript, PDF or Illustrator file as SVG text, through Ghostscript and Poppler.
fn converted_svg(path: &Path, format: VectorFormat) -> Result<String, String> {
    let pdftocairo = which("pdftocairo").ok_or(CONVERTER_HINT)?;
    let dir = std::env::temp_dir().join(format!("cadrs-vector-{}", uuid::Uuid::new_v4()));
    cadrs_fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let result = (|| {
        let pdf = match format {
            VectorFormat::PostScript => {
                let gs = ["gs", "gswin64c", "gswin32c"]
                    .into_iter()
                    .find_map(which)
                    .ok_or(CONVERTER_HINT)?;
                let pdf = dir.join("drawing.pdf");
                let mut c = std::process::Command::new(gs);
                c.args([
                    "-q",
                    "-dNOPAUSE",
                    "-dBATCH",
                    "-dSAFER",
                    "-dEPSCrop",
                    "-sDEVICE=pdfwrite",
                ])
                .arg(format!("-sOutputFile={}", pdf.display()))
                .arg(path);
                run(c, "Ghostscript")?;
                pdf
            }
            _ => path.to_path_buf(),
        };
        let svg = dir.join("drawing.svg");
        let mut c = std::process::Command::new(pdftocairo);
        c.args(["-svg", "-f", "1", "-l", "1"]).arg(&pdf).arg(&svg);
        run(c, "pdftocairo")?;
        cadrs_fs::read_to_string(&svg).map_err(|e| e.to_string())
    })();
    let _ = cadrs_fs::remove_dir_all(&dir);
    result
}

/// Inserts a vector drawing into the Part Studio as a new sketch on `plane` holding its
/// geometry moved by `offset` (one undo step, "Insert <file>").
#[derive(Debug, Clone)]
pub struct AddVectorSketch {
    pub element: crate::ids::ElementId,
    pub feature: crate::ids::FeatureId,
    pub plane: cadrs_sketch::PlaneRef,
    pub file_name: String,
    pub geometry: Sketch,
    pub offset: Vec2,
}

impl crate::command::Command for AddVectorSketch {
    fn label(&self) -> String {
        format!("Insert {}", self.file_name)
    }
    fn scope(&self) -> crate::command::Scope {
        crate::command::Scope::Element(self.element)
    }
    fn apply(
        &self,
        doc: &mut crate::document::Document,
    ) -> Result<(), crate::command::CommandError> {
        crate::commands::AddSketch {
            element: self.element,
            feature: self.feature,
            plane: Some(self.plane),
        }
        .apply(doc)?;
        InsertVector {
            element: self.element,
            feature: self.feature,
            file_name: self.file_name.clone(),
            geometry: self.geometry.clone(),
            offset: self.offset,
        }
        .apply(doc)
    }
}

/// Adds a vector drawing's geometry, moved by `offset`, to a sketch (one undo step,
/// "Insert <file>").
#[derive(Debug, Clone)]
pub struct InsertVector {
    pub element: crate::ids::ElementId,
    pub feature: crate::ids::FeatureId,
    pub file_name: String,
    pub geometry: Sketch,
    pub offset: Vec2,
}

impl crate::command::Command for InsertVector {
    fn label(&self) -> String {
        format!("Insert {}", self.file_name)
    }
    fn scope(&self) -> crate::command::Scope {
        crate::command::Scope::Element(self.element)
    }
    fn apply(
        &self,
        doc: &mut crate::document::Document,
    ) -> Result<(), crate::command::CommandError> {
        let op = cadrs_sketch::SketchOp::Paste {
            sketch: Box::new(self.geometry.clone()),
            offset: self.offset,
        };
        crate::commands::EditSketch {
            element: self.element,
            feature: self.feature,
            op,
        }
        .apply(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(svg: &str) -> (Sketch, ImportReport) {
        sketch_of_svg(svg.as_bytes()).unwrap()
    }

    #[test]
    fn a_plate_with_a_hole_and_a_rounded_corner() {
        // 40 × 20 mm, a 5 mm radius corner (an arc command), a 3 mm radius hole (a circle,
        // which usvg writes as four cubics), filled and then stroked as a second path.
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="40mm" height="20mm" viewBox="0 0 40 20">
            <path d="M0 0 H35 A5 5 0 0 1 40 5 V20 H0 Z" fill="black"/>
            <path d="M0 0 H35 A5 5 0 0 1 40 5 V20 H0 Z" fill="none" stroke="red"/>
            <circle cx="20" cy="10" r="3" fill="white"/>
        </svg>"#;
        let (s, r) = read(svg);
        assert_eq!(
            (r.lines, r.arcs, r.circles, r.beziers),
            (4, 1, 1, 0),
            "{r:?}"
        );
        let regions = cadrs_sketch::region::regions(&s);
        let plate = regions
            .iter()
            .find(|g| !g.holes.is_empty())
            .expect("the plate closes round its hole");
        let corner = 25.0 - std::f64::consts::PI * 25.0 / 4.0;
        let want = 40.0 * 20.0 - corner - std::f64::consts::PI * 9.0;
        assert!(
            (plate.area() - want).abs() < 0.01,
            "{} vs {want}",
            plate.area()
        );
        // Y up: the drawing lies below its top-left corner.
        assert!(
            s.points
                .values()
                .all(|p| p.pos.y <= 1e-4 && p.pos.y >= -20.0 - 1e-4)
        );
    }

    #[test]
    fn free_curves_stay_beziers() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96"><path d="M0 0 C 30 80 60 -40 90 40 L 0 40 Z" stroke="black"/></svg>"#;
        let (s, r) = read(svg);
        assert_eq!((r.lines, r.beziers), (2, 1), "{r:?}");
        // 96 px is an inch.
        let x = s.points.values().map(|p| p.pos.x).fold(f64::MIN, f64::max);
        assert!((x - 90.0 * 25.4 / 96.0).abs() < 1e-4, "{x}");
        assert_eq!(cadrs_sketch::region::regions(&s).len(), 1);
    }

    #[test]
    fn centred() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="40mm" height="20mm" viewBox="0 0 40 20"><rect x="10" y="0" width="30" height="20" fill="black"/></svg>"#;
        let (mut s, _) = read(svg);
        centre(&mut s);
        let (lo, hi) = s.points.values().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
            (lo.min(p.pos.x), hi.max(p.pos.x))
        });
        assert!(
            (lo + 15.0).abs() < 1e-6 && (hi - 15.0).abs() < 1e-6,
            "{lo} {hi}"
        );
    }
}

#[cfg(test)]
mod converter_tests {
    use super::*;

    // Through Ghostscript and pdftocairo; skipped where they aren't installed.
    #[test]
    fn an_eps_plate() {
        if which("pdftocairo").is_none()
            || ["gs", "gswin64c"].into_iter().all(|g| which(g).is_none())
        {
            eprintln!("skipped: {CONVERTER_HINT}");
            return;
        }
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vector/bracket.eps");
        let (s, r) = read_sketch(&path, None, true).unwrap();
        assert_eq!((r.arcs, r.circles), (1, 1), "{r:?}");
        let regions = cadrs_sketch::region::regions(&s);
        let plate = regions
            .iter()
            .find(|g| !g.holes.is_empty())
            .expect("the plate closes round its hole");
        let corner = 25.0 - std::f64::consts::PI * 25.0 / 4.0;
        let want = 40.0 * 20.0 - corner - std::f64::consts::PI * 9.0;
        assert!(
            (plate.area() - want).abs() < 0.5,
            "{} vs {want}",
            plate.area()
        );
    }
}
