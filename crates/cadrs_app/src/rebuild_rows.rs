//! The feature list while a rebuild runs for longer than a moment: each feature it rebuilds
//! shows where it stands, done (as it always looks), busy (a spinner at its end and a pulsing
//! accent at its left edge) or queued (veiled), from [`PartCache::rebuild_steps`]; the parts in
//! the view show how far it has got ([`PartCache`] takes them as it goes). The list follows the
//! busy row while the one before it was in view (not once it is scrolled away from it).
//!
//! The rows are spawned again whenever their key changes ([`crate::document::FeatureRowsSet`]),
//! so each one's step is an overlay of its own, added, updated and faded out in place.

use std::collections::HashMap;

use bevy::prelude::*;
use cadrs_core::FeatureId;
use cadrs_ui::Theme;
use cadrs_ui::spinner::Spinner;
use cadrs_ui::spring::MotionSettings;

use crate::AppState;
use crate::document::{FeatureRow, RevealFeature};
use crate::parts::{PartCache, RebuildStep};
use crate::rebuild_indicator::SHOW_AFTER_SECS;

/// How much a queued row is veiled (the list's background over it, 0–1).
const QUEUED_VEIL: f32 = 0.55;
/// How long the veil takes to come or go (s).
const FADE: f32 = 0.15;
/// The busy row's accent: its width (px), and how often it pulses (per second).
const ACCENT_WIDTH: f32 = 3.0;
const PULSE_HZ: f32 = 1.2;

pub struct RebuildRowsPlugin;

impl Plugin for RebuildRowsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                sync_steps.after(crate::document::FeatureRowsSet),
                animate_steps.after(sync_steps),
            )
                .run_if(in_state(AppState::Document)),
        );
    }
}

/// A row's rebuild step, over the row (its last child).
#[derive(Component)]
struct StepOverlay {
    /// `None`: the rebuild is over (the overlay fades out and goes).
    step: Option<RebuildStep>,
    /// The veil's opacity now (it eases to its step's).
    veil: f32,
}

/// What shows only while the row is busy: its accent and its spinner.
#[derive(Component)]
struct BusyMark;

/// The busy row's pulsing accent.
#[derive(Component)]
struct Accent;

/// Puts each row's step on it: an overlay for a row of the rebuild running (once it has run
/// as long as the "Rebuilding…" indicator waits), its step updated, and none left showing a
/// step once the rebuild is over.
fn sync_steps(
    mut commands: Commands,
    cache: Res<PartCache>,
    time: Res<Time<Real>>,
    theme: Res<Theme>,
    mut since: Local<Option<f32>>,
    mut busy_was: Local<Option<FeatureId>>,
    revealing: Option<Res<RevealFeature>>,
    rows: Query<(Entity, &FeatureRow, &Name)>,
    mut overlays: Query<(&ChildOf, &mut StepOverlay)>,
    laid_out: Query<(&FeatureRow, &ComputedNode, &bevy::ui::UiGlobalTransform)>,
    lists: Query<(&Name, &ComputedNode, &bevy::ui::UiGlobalTransform)>,
) {
    let now = time.elapsed_secs();
    let steps: HashMap<FeatureId, RebuildStep> = match cache.rebuild_steps() {
        Some(s) if now - *since.get_or_insert(now) >= SHOW_AFTER_SECS => s.into_iter().collect(),
        Some(_) => HashMap::new(),
        None => {
            *since = None;
            HashMap::new()
        }
    };
    // The list follows the busy row, while the row busy before it was in view (or still being
    // scrolled to: the rows can be busy for less than the scroll takes).
    let busy = steps
        .iter()
        .find(|(_, s)| **s == RebuildStep::Busy)
        .map(|(id, _)| *id);
    if busy != *busy_was {
        let in_view = |id: FeatureId| {
            let span = |n: &ComputedNode, at: &bevy::ui::UiGlobalTransform| {
                let half = n.size().y / 2.0;
                (at.translation.y - half, at.translation.y + half)
            };
            let row = laid_out.iter().find(|(r, ..)| r.0 == id);
            let list = lists.iter().find(|(n, ..)| n.as_str() == "feature-scroll");
            match (row, list) {
                (Some((_, rn, ra)), Some((_, ln, la))) => {
                    let ((top, bottom), (view_top, view_bottom)) = (span(rn, ra), span(ln, la));
                    top >= view_top - 1.0 && bottom <= view_bottom + 1.0
                }
                _ => false,
            }
        };
        if let Some(id) = busy
            && (revealing.is_some() || busy_was.is_none_or(in_view))
        {
            commands.insert_resource(RevealFeature::new(id));
        }
        *busy_was = busy;
    }
    let mut shown: HashMap<Entity, Mut<StepOverlay>> = overlays
        .iter_mut()
        .map(|(parent, o)| (parent.parent(), o))
        .collect();
    for (row, FeatureRow(id), name) in &rows {
        let step = steps.get(id).copied();
        if let Some(o) = shown.get_mut(&row) {
            if o.step != step {
                o.step = step;
            }
            continue;
        }
        let Some(step) = step else { continue };
        let busy = step == RebuildStep::Busy;
        let display = |on: bool| if on { Display::Flex } else { Display::None };
        commands.entity(row).with_children(|r| {
            r.spawn((
                Name::new(format!("{name}-step")),
                StepOverlay {
                    step: Some(step),
                    veil: 0.0,
                },
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    right: Val::Px(0.0),
                    top: Val::Px(0.0),
                    bottom: Val::Px(0.0),
                    ..default()
                },
                BackgroundColor(theme.sidebar.with_alpha(0.0)),
                Pickable::IGNORE,
            ))
            .with_children(|o| {
                o.spawn((
                    Name::new(format!("{name}-busy-accent")),
                    BusyMark,
                    Accent,
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(0.0),
                        top: Val::Px(0.0),
                        bottom: Val::Px(0.0),
                        width: Val::Px(ACCENT_WIDTH),
                        display: display(busy),
                        ..default()
                    },
                    BackgroundColor(theme.primary),
                    Pickable::IGNORE,
                ));
                o.spawn((
                    Name::new(format!("{name}-busy")),
                    BusyMark,
                    Node {
                        position_type: PositionType::Absolute,
                        right: Val::Px(theme.space[2]),
                        top: Val::Px(0.0),
                        bottom: Val::Px(0.0),
                        align_items: AlignItems::Center,
                        display: display(busy),
                        ..default()
                    },
                    Pickable::IGNORE,
                ))
                .with_children(|w| {
                    w.spawn(
                        Spinner::new(format!("{name}-spinner"))
                            .size(12.0)
                            .thickness(2.0)
                            .build(&theme),
                    );
                });
            });
        });
    }
}

/// Eases each overlay's veil to its step's, pulses the busy row's accent, shows the busy marks
/// on the busy row alone, and takes an overlay away once the rebuild is over and it has faded.
fn animate_steps(
    mut commands: Commands,
    time: Res<Time<Real>>,
    theme: Res<Theme>,
    motion: Res<MotionSettings>,
    mut overlays: Query<(Entity, &mut StepOverlay, &mut BackgroundColor, &Children)>,
    mut marks: Query<
        (&mut Node, Option<&mut BackgroundColor>, Has<Accent>),
        (With<BusyMark>, Without<StepOverlay>),
    >,
) {
    let dt = time.delta_secs();
    let pulse = if motion.enabled {
        0.55 + 0.45 * (std::f32::consts::TAU * PULSE_HZ * time.elapsed_secs()).cos()
    } else {
        1.0
    };
    for (e, mut o, mut bg, children) in &mut overlays {
        let target = if o.step == Some(RebuildStep::Queued) {
            QUEUED_VEIL
        } else {
            0.0
        };
        let veil = if motion.enabled {
            let step = QUEUED_VEIL * dt / FADE;
            if o.veil < target {
                (o.veil + step).min(target)
            } else {
                (o.veil - step).max(target)
            }
        } else {
            target
        };
        if veil != o.veil {
            o.veil = veil;
            bg.0 = theme.sidebar.with_alpha(veil);
        }
        if o.step.is_none() && o.veil <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        let busy = o.step == Some(RebuildStep::Busy);
        for c in children {
            let Ok((mut node, color, accent)) = marks.get_mut(*c) else {
                continue;
            };
            let display = if busy { Display::Flex } else { Display::None };
            if node.display != display {
                node.display = display;
            }
            if busy
                && accent
                && let Some(mut color) = color
            {
                color.0 = theme.primary.with_alpha(pulse);
            }
        }
    }
}
