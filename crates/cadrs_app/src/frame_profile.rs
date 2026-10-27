//! Where a frame's time goes: the CPU time of each schedule and system, from Bevy's tracing
//! spans (the `bevy/trace` feature makes a span for every system and schedule run;
//! [`profiler_layer`] adds them up), and the GPU time of each render pass, from Bevy's render
//! diagnostics (timestamp queries), and the line vertices each gizmo group sent to the GPU
//! (gizmos are drawn anew every frame). [`FrameProfile`] keeps the last second of frames; the frame
//! panel ([`crate::frame_stats`]) draws it as bars and `get_frame_stats` returns it over MCP.
//!
//! Systems run in parallel on several threads, so their times can add up to more than the
//! frame; the schedules on the main thread add up to about the frame.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use web_time::Instant;

use bevy::diagnostic::DiagnosticsStore;
use bevy::gizmos::config::GizmoConfigStore;
use bevy::gizmos::{GizmoAsset, GizmoHandles};
use bevy::log::BoxedLayer;
use bevy::log::tracing::Subscriber;
use bevy::log::tracing::field::{Field, Visit};
use bevy::log::tracing::span::{Attributes, Id};
use bevy::log::tracing_subscriber::Layer;
use bevy::log::tracing_subscriber::layer::Context;
use bevy::log::tracing_subscriber::registry::LookupSpan;
use bevy::prelude::*;
use bevy::render::diagnostic::RenderDiagnosticsPlugin;
use serde_json::{Value, json};

/// How far back the profile looks.
const WINDOW_SECS: f64 = 1.0;
/// GPU passes not measured for this long have stopped running.
const GPU_STALE_SECS: f32 = 1.0;

/// The schedules the main loop runs one after another each frame (`Main`'s, then the render
/// world's extract): their bars add up to about the frame.
pub const MAIN_SCHEDULES: &[&str] = &[
    "First",
    "PreUpdate",
    "StateTransition",
    "RunFixedMainLoop",
    "Update",
    "SpawnScene",
    "PostUpdate",
    "Last",
    "ExtractSchedule",
];
/// The render thread's schedules (it renders the frame before while the main loop runs; the
/// extract waits for it). `RenderGraph` runs inside `Render`.
pub const RENDER_SCHEDULES: &[&str] = &["Render"];

pub struct FrameProfilePlugin;

impl Plugin for FrameProfilePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FrameProfile>()
            .add_systems(First, take_frame);
        if !app.is_plugin_added::<RenderDiagnosticsPlugin>() {
            app.add_plugins(RenderDiagnosticsPlugin);
        }
    }
}

/// What a span measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Schedule,
    System,
    /// Applying a system's commands (and the observers they trigger).
    Commands,
}

/// What the layer adds up between two frames: per span, the total time (ns) and the runs.
type Tally = HashMap<(Kind, Arc<str>), (u64, u32)>;

/// The layer's tally, which [`take_frame`] empties each frame.
#[derive(Resource, Clone, Default)]
pub struct ProfilerTally(Arc<Mutex<Tally>>);

/// The [`bevy::log::LogPlugin::custom_layer`] that times system and schedule spans (none
/// without the `bevy/trace` feature: the profile then has only the GPU passes).
pub fn profiler_layer(app: &mut App) -> Option<BoxedLayer> {
    let tally = ProfilerTally::default();
    app.insert_resource(tally.clone());
    Some(Box::new(ProfilerLayer(tally.0)))
}

struct ProfilerLayer(Arc<Mutex<Tally>>);

/// A timed span's name, kept on the span.
struct SpanLabel(Kind, Arc<str>);
/// When the span was entered.
struct Entered(Instant);

/// Reads a span's `name` field.
#[derive(Default)]
struct NameField(Option<String>);

impl Visit for NameField {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "name" {
            self.0 = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "name" {
            self.0 = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for ProfilerLayer {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let kind = match attrs.metadata().name() {
            "system" => Kind::System,
            "schedule" => Kind::Schedule,
            "system_commands" => Kind::Commands,
            _ => return,
        };
        let mut name = NameField::default();
        attrs.record(&mut name);
        if let (Some(name), Some(span)) = (name.0, ctx.span(id)) {
            span.extensions_mut().insert(SpanLabel(kind, name.into()));
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut ext = span.extensions_mut();
            if ext.get_mut::<SpanLabel>().is_some() {
                ext.replace(Entered(Instant::now()));
            }
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut ext = span.extensions_mut();
        let Some(Entered(start)) = ext.remove::<Entered>() else {
            return;
        };
        let Some(SpanLabel(kind, name)) = ext.get_mut::<SpanLabel>() else {
            return;
        };
        let ns = start.elapsed().as_nanos() as u64;
        if let Ok(mut tally) = self.0.lock() {
            let e = tally.entry((*kind, name.clone())).or_default();
            e.0 += ns;
            e.1 += 1;
        }
    }
}

/// One frame's times.
#[derive(Debug, Clone, Default)]
pub struct FrameSample {
    /// When it ended (s since start) and how long it took (ms).
    pub at: f64,
    pub frame_ms: f32,
    /// CPU spans: kind, name, ms, runs.
    pub cpu: Vec<(Kind, Arc<str>, f32, u32)>,
    /// GPU passes measured: path below `render/`, ms.
    pub gpu: Vec<(String, f32)>,
    /// Gizmo groups that drew: type path, line vertices.
    pub gizmos: Vec<(String, u32)>,
}

/// The last [`WINDOW_SECS`] of frames.
#[derive(Resource, Debug, Default)]
pub struct FrameProfile {
    frames: VecDeque<FrameSample>,
}

/// One row of a [`Summary`]: a span's mean time per frame over the window, its most in one
/// frame, and its runs per frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: String,
    pub ms: f32,
    pub max_ms: f32,
    pub runs: f32,
}

/// [`FrameProfile`] averaged over its window, each list slowest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    pub frames: usize,
    pub frame_ms: f32,
    /// [`MAIN_SCHEDULES`] that ran, in the loop's order.
    pub main: Vec<Row>,
    /// [`RENDER_SCHEDULES`] that ran.
    pub render: Vec<Row>,
    pub schedules: Vec<Row>,
    pub systems: Vec<Row>,
    pub gpu: Vec<Row>,
    /// Gizmo groups by line vertices a frame (in `ms`: a count, not a time).
    pub gizmos: Vec<Row>,
}

impl FrameProfile {
    /// The newest frame.
    pub fn last(&self) -> Option<&FrameSample> {
        self.frames.back()
    }

    pub fn push(&mut self, sample: FrameSample) {
        let now = sample.at;
        self.frames.push_back(sample);
        // At least the last frame stays, however long it took.
        while self.frames.len() > 1
            && self
                .frames
                .front()
                .is_some_and(|f| now - f.at > WINDOW_SECS)
        {
            self.frames.pop_front();
        }
    }

    pub fn summary(&self) -> Summary {
        let n = self.frames.len();
        if n == 0 {
            return Summary::default();
        }
        let rows = |items: &mut dyn Iterator<Item = (String, f32, u32)>| {
            let mut by: HashMap<String, (f32, f32, u32)> = HashMap::new();
            for (name, ms, runs) in items {
                let e = by.entry(name).or_default();
                e.0 += ms;
                e.1 = e.1.max(ms);
                e.2 += runs;
            }
            let mut rows: Vec<Row> = by
                .into_iter()
                .map(|(name, (sum, max_ms, runs))| Row {
                    name,
                    ms: sum / n as f32,
                    max_ms,
                    runs: runs as f32 / n as f32,
                })
                .collect();
            rows.sort_by(|a, b| b.ms.total_cmp(&a.ms).then_with(|| a.name.cmp(&b.name)));
            rows
        };
        let cpu = |kind: Kind| {
            let mut it = self.frames.iter().flat_map(move |f| {
                f.cpu
                    .iter()
                    .filter(move |c| c.0 == kind)
                    .map(|c| (c.1.to_string(), c.2, c.3))
            });
            rows(&mut it)
        };
        let schedules = cpu(Kind::Schedule);
        let pick = |names: &[&str]| {
            names
                .iter()
                .filter_map(|s| schedules.iter().find(|r| r.name == *s).cloned())
                .collect()
        };
        let (main, render) = (pick(MAIN_SCHEDULES), pick(RENDER_SCHEDULES));
        let mut gpu = self
            .frames
            .iter()
            .flat_map(|f| f.gpu.iter().map(|(p, ms)| (p.clone(), *ms, 1)));
        let mut gizmos = self
            .frames
            .iter()
            .flat_map(|f| f.gizmos.iter().map(|(g, n)| (g.clone(), *n as f32, 1)));
        Summary {
            frames: n,
            frame_ms: self.frames.iter().map(|f| f.frame_ms).sum::<f32>() / n as f32,
            main,
            render,
            systems: cpu(Kind::System),
            schedules,
            gpu: rows(&mut gpu),
            gizmos: rows(&mut gizmos),
        }
    }

    /// What `get_frame_stats` returns about the frame's parts: the `top` slowest systems.
    pub fn describe(&self, top: usize) -> Value {
        let s = self.summary();
        let r = |x: f32| (x as f64 * 100.0).round() / 100.0;
        let rows = |rows: &[Row], top: usize| -> Vec<Value> {
            rows.iter()
                .take(top)
                .map(|w| json!({ "name": w.name, "ms": r(w.ms), "max_ms": r(w.max_ms), "runs": r(w.runs) }))
                .collect()
        };
        json!({
            "window_frames": s.frames,
            "cpu": {
                "main_loop": rows(&s.main, usize::MAX),
                "render_thread": rows(&s.render, usize::MAX),
                "schedules": rows(&s.schedules, top),
                "systems": rows(&s.systems, top),
            },
            "gpu": {
                "measured": !s.gpu.is_empty(),
                "passes": rows(&s.gpu, top),
            },
            "gizmo_vertices": {
                "total": s.gizmos.iter().map(|g| g.ms as f64).sum::<f64>().round(),
                "groups": s.gizmos.iter().take(top).map(|g| json!({ "group": g.name, "vertices": g.ms.round() })).collect::<Vec<_>>(),
            },
        })
    }
}

/// A system's name for the panel: no generics, the last two parts of its path.
pub fn short_name(name: &str) -> String {
    let mut plain = String::with_capacity(name.len());
    let mut depth = 0usize;
    for ch in name.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => plain.push(ch),
            _ => {}
        }
    }
    let parts: Vec<&str> = plain.split("::").collect();
    parts[parts.len().saturating_sub(2)..].join("::")
}

/// Ends the frame's tally: the spans timed since the last frame and the GPU passes measured.
#[allow(clippy::too_many_arguments)]
pub(crate) fn take_frame(
    time: Res<Time<Real>>,
    tally: Option<Res<ProfilerTally>>,
    store: Option<Res<DiagnosticsStore>>,
    gizmo_handles: Option<Res<GizmoHandles>>,
    gizmo_configs: Option<Res<GizmoConfigStore>>,
    gizmo_assets: Option<Res<Assets<GizmoAsset>>>,
    mut profile: ResMut<FrameProfile>,
) {
    let cpu = tally
        .and_then(|t| t.0.lock().ok().map(|mut m| std::mem::take(&mut *m)))
        .unwrap_or_default()
        .into_iter()
        .map(|((kind, name), (ns, runs))| (kind, name, ns as f32 / 1e6, runs))
        .collect();
    let now = Instant::now();
    let gpu = store
        .map(|s| {
            s.iter()
                .filter_map(|d| {
                    let path = d
                        .path()
                        .as_str()
                        .strip_prefix("render/")?
                        .strip_suffix("/elapsed_gpu")?;
                    let m = d.measurement()?;
                    (now.saturating_duration_since(m.time).as_secs_f32() < GPU_STALE_SECS)
                        .then(|| (path.to_string(), m.value as f32))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut gizmos = Vec::new();
    if let (Some(handles), Some(configs), Some(assets)) =
        (gizmo_handles, gizmo_configs, gizmo_assets)
    {
        let names: HashMap<_, _> = configs
            .iter()
            .map(|(id, _, group)| (*id, group.reflect_type_path().to_string()))
            .collect();
        for (id, handle) in handles.handles() {
            let Some(asset) = handle.as_ref().and_then(|h| assets.get(h)) else {
                continue;
            };
            let b = asset.buffer();
            let n = (b.list_positions.len() + b.strip_positions.len()) as u32;
            if n > 0 {
                gizmos.push((names.get(id).cloned().unwrap_or_else(|| "?".into()), n));
            }
        }
    }
    profile.push(FrameSample {
        at: time.elapsed_secs_f64(),
        frame_ms: time.delta_secs() * 1000.0,
        cpu,
        gpu,
        gizmos,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names_drop_generics_and_crates() {
        assert_eq!(
            short_name("cadrs_app::plane_display::place_plane_feature_labels"),
            "plane_display::place_plane_feature_labels"
        );
        assert_eq!(
            short_name("bevy_ecs::event::update<bevy_window::WindowResized>"),
            "event::update"
        );
        assert_eq!(short_name("tick"), "tick");
    }

    #[test]
    fn the_summary_averages_over_the_window() {
        let mut p = FrameProfile::default();
        let sys: Arc<str> = "a::b::slow".into();
        for i in 0..4 {
            p.push(FrameSample {
                at: i as f64 * 0.1,
                frame_ms: 100.0,
                cpu: vec![
                    (Kind::System, sys.clone(), 10.0 * (i + 1) as f32, 1),
                    (Kind::Schedule, "Update".into(), 50.0, 1),
                ],
                gpu: vec![("main_pass".into(), 2.0)],
                gizmos: vec![("Lines".into(), 100)],
            });
        }
        let s = p.summary();
        assert_eq!(s.frames, 4);
        assert_eq!(
            s.systems[0],
            Row {
                name: "a::b::slow".into(),
                ms: 25.0,
                max_ms: 40.0,
                runs: 1.0
            }
        );
        assert_eq!(s.main.len(), 1);
        assert_eq!(s.gpu[0].ms, 2.0);
        assert_eq!(s.gizmos[0].ms, 100.0);
        // A frame long after drops the others, but the last frame always stays.
        p.push(FrameSample {
            at: 5.0,
            frame_ms: 3000.0,
            ..default()
        });
        assert_eq!(p.summary().frames, 1);
        assert_eq!(p.describe(5)["window_frames"], 1);
    }
}
