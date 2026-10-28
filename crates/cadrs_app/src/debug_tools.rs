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
