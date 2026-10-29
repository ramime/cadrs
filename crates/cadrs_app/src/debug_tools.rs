//! Debugging aids, each off unless its environment variable is set (so they cost nothing
//! otherwise):
//!
//! - `CADRS_SLOW_FRAMES=<ms>`: prints every frame that took longer than this (wall clock, so
//!   scripted runs' fixed time steps don't hide it) with its slowest schedules, systems and
//!   systems' commands (observers included; those over 1 ms), from the frame profile ([`crate::frame_profile`]; it turns
//!   the system times on, which need the default `profiler` feature). `CADRS_SLOW_FRAMES=1` (or
//!   any value that isn't a number) uses 50 ms.
//! - `CADRS_FRAME_PROFILE=1`: times every system and schedule for the frame panel and
//!   `get_frame_stats`, as the *Time systems* preference does (`0` keeps them off;
//!   [`crate::frame_profile::CpuProfiling`]).
//! - `CADRS_NO_VSYNC=1`: presents frames as fast as they are made (no vsync), to see what a
//!   frame really costs.
//! - `CADRS_UI_CHANGES=1`: about once a second, how many UI nodes changed what Bevy lays them
//!   out from (their `Node`, measured content or render target) in the last frame, and which
//!   (by name, or their nearest named ancestor's). Any such change makes Bevy lay the whole UI
//!   out again, so a node that changes every frame costs a full layout every frame. Built with
//!   Bevy's `track_location` feature, it also says which line of code wrote the changed images.

use web_time::Instant;

use bevy::prelude::*;

use crate::frame_profile::{FrameProfile, Kind};

pub struct DebugToolsPlugin;

impl Plugin for DebugToolsPlugin {
    fn build(&self, app: &mut App) {
        if let Some(ms) = slow_frame_threshold() {
            app.insert_resource(SlowFrames { ms }).add_systems(
                First,
                report_slow_frames.after(crate::frame_profile::take_frame),
            );
        }
        if std::env::var_os("CADRS_NO_VSYNC").is_some() {
            app.add_systems(Startup, no_vsync);
        }
        if std::env::var_os("CADRS_UI_CHANGES").is_some() {
            app.add_systems(
                PostUpdate,
                report_ui_changes.before(bevy::ui::UiSystems::Layout),
            );
        }
    }
}

/// `CADRS_SLOW_FRAMES` in ms, if set.
fn slow_frame_threshold() -> Option<f32> {
    let v = std::env::var("CADRS_SLOW_FRAMES").ok()?;
    Some(match v.trim().parse::<f32>() {
        Ok(ms) if ms > 1.0 => ms,
        _ => 50.0,
    })
}

#[derive(Resource)]
struct SlowFrames {
    ms: f32,
}

fn report_slow_frames(
    slow: Res<SlowFrames>,
    profile: Res<FrameProfile>,
    mut last: Local<Option<Instant>>,
) {
    let now = Instant::now();
    let Some(ms) = last.replace(now).map(|t| (now - t).as_secs_f32() * 1000.0) else {
        return;
    };
    if ms < slow.ms {
        return;
    }
    let spans = |kind: Kind| {
        let mut v: Vec<_> = profile
            .last()
            .map(|f| {
                f.cpu
                    .iter()
                    .filter(|(k, _, t, _)| *k == kind && *t >= 1.0)
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| b.2.total_cmp(&a.2));
        v
    };
    let mut out = format!("slow frame: {ms:.0} ms");
    // The schedules show where time outside any system went (applying commands, waiting).
    for (label, kind, n) in [
        ("schedule", Kind::Schedule, 6),
        ("system", Kind::System, 12),
        ("commands of", Kind::Commands, 6),
    ] {
        for (_, name, t, runs) in spans(kind).iter().take(n) {
            out.push_str(&format!("\n  {t:8.1} ms  x{runs}  {label} {name}"));
        }
    }
    warn!("{out}");
}

/// `CADRS_UI_CHANGES`: the UI nodes whose layout inputs changed since the last frame.
#[allow(clippy::type_complexity)]
fn report_ui_changes(
    changed: Query<
        (
            Entity,
            Ref<Node>,
            Option<Ref<bevy::ui::ContentSize>>,
            Option<Ref<bevy::ui::ComputedUiRenderTargetInfo>>,
        ),
        Or<(
            Changed<Node>,
            Changed<bevy::ui::ContentSize>,
            Changed<bevy::ui::ComputedUiRenderTargetInfo>,
        )>,
    >,
    names: Query<&Name>,
    parents: Query<&ChildOf>,
    images: Query<Ref<ImageNode>>,
    texts: Query<Ref<Text>>,
    archetypes: &bevy::ecs::archetype::Archetypes,
    mut frames: Local<u32>,
) {
    *frames += 1;
    if *frames % 60 != 0 {
        return;
    }
    let (mut node, mut content, mut target, mut added) = (0, 0, 0, 0);
    let mut by_name = std::collections::BTreeMap::<String, u32>::new();
    for (e, n, c, t) in &changed {
        if n.is_added() {
            added += 1;
            continue;
        }
        node += u32::from(n.is_changed());
        content += u32::from(c.is_some_and(|c| c.is_changed()));
        target += u32::from(t.is_some_and(|t| t.is_changed()));
        let named = std::iter::successors(Some(e), |&e| parents.get(e).ok().map(ChildOf::parent))
            .find_map(|e| names.get(e).ok())
            .map_or("(unnamed)", |n| n.as_str());
        *by_name.entry(named.to_string()).or_default() += 1;
    }
    let (mut image_sized, mut text_sized) = (0, 0);
    for (e, n, c, _) in &changed {
        if !n.is_added() && c.is_some_and(|c| c.is_changed()) {
            image_sized += u32::from(images.contains(e));
            text_sized += u32::from(texts.contains(e));
        }
    }
    let images_changed = images
        .iter()
        .filter(|i| i.is_changed() && !i.is_added())
        .count();
    let texts_changed = texts
        .iter()
        .filter(|t| t.is_changed() && !t.is_added())
        .count();
    // Where the changed images were written, when Bevy tracks it (its `track_location` feature).
    let mut writers = std::collections::BTreeMap::<String, u32>::new();
    for i in images.iter().filter(|i| i.is_changed() && !i.is_added()) {
        if let Some(at) = i.changed_by().into_option() {
            *writers.entry(at.to_string()).or_default() += 1;
        }
    }
    let mut top: Vec<_> = by_name.into_iter().collect();
    top.sort_by_key(|t| std::cmp::Reverse(t.1));
    info!(
        "UI changes: {node} Node, {content} content size ({image_sized} images, {text_sized} \
         texts), {target} render target, {added} added; {images_changed} ImageNode and \
         {texts_changed} Text changed (written at {writers:?}); {} archetypes; by name: {:?}",
        archetypes.len(),
        &top[..top.len().min(15)]
    );
}

/// `CADRS_NO_VSYNC`: the window presents without waiting for the display.
fn no_vsync(mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>) {
    for mut w in &mut windows {
        w.present_mode = bevy::window::PresentMode::AutoNoVsync;
    }
}
