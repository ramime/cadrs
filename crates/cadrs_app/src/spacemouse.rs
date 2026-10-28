//! Six-degree-of-freedom input for the Part Studio and Assembly viewport.
//!
//! Device access runs on a background thread so a missing driver, device or spacenav daemon
//! cannot block the Bevy event loop. Backends publish normalized axis samples; this module owns
//! the camera mapping shared by Windows and Linux.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use bevy::picking::hover::HoverMap;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::camera::ViewState;
use crate::viewport::{self, ActiveKind, ViewportArea, ViewportView};
use crate::{AppState, eda::Eda2d};

const PAN_PIXELS_PER_SECOND: f32 = 700.0;
const ROTATION_DEGREES_PER_SECOND: f32 = 150.0;
const ZOOM_PER_SECOND: f32 = 2.0;
const MAX_INPUT_WINDOW: f32 = 0.05;
const MAX_QUEUED_SAMPLES: usize = 64;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct MotionSample {
    /// Translation and rotation rates, normalized to approximately -1..=1.
    axes: [f32; 6],
    /// Time represented by the sample, capped to avoid a large jump after a pause.
    dt: f32,
}

#[derive(Resource)]
struct MotionReceiver(Mutex<Receiver<MotionSample>>);

#[derive(Resource, Clone)]
struct MotionSender(Sender<MotionSample>);

/// The Windows 3Dconnexion driver can also emit forward/backward SpaceMouse translation as
/// ordinary wheel input. Suppress that legacy event briefly after raw HID translation so zoom is
/// applied only by the smooth 6-DoF input path.
#[derive(Resource, Clone, Default)]
pub(crate) struct LegacyWheelGuard(std::sync::Arc<Mutex<Option<Instant>>>);

impl LegacyWheelGuard {
    pub(crate) fn mark_space_mouse_translation(&self) {
        if let Ok(mut last_motion) = self.0.lock() {
            *last_motion = Some(Instant::now());
        }
    }

    pub(crate) fn suppress_legacy_wheel(&self) -> bool {
        self.0
            .lock()
            .ok()
            .and_then(|last_motion| *last_motion)
            .is_some_and(|last_motion| last_motion.elapsed() < Duration::from_millis(300))
    }
}

pub struct SpaceMousePlugin;

impl Plugin for SpaceMousePlugin {
    fn build(&self, app: &mut App) {
        let (sender, receiver) = mpsc::channel();
        app.insert_resource(MotionSender(sender))
            .insert_resource(MotionReceiver(Mutex::new(receiver)))
            .init_resource::<LegacyWheelGuard>()
            .add_systems(Startup, start_backend)
            .add_systems(Update, apply_motion.run_if(in_state(AppState::Document)));
    }
}

fn start_backend(sender: Res<MotionSender>, wheel_guard: Res<LegacyWheelGuard>) {
    #[cfg(target_os = "windows")]
    windows_backend::start(sender.0.clone(), wheel_guard.clone());
    #[cfg(target_os = "linux")]
    linux_backend::start(sender.0.clone());
    #[cfg(target_os = "linux")]
    let _ = wheel_guard;
}

#[allow(clippy::too_many_arguments)]
fn apply_motion(
    input: Res<MotionReceiver>,
    mut view: ResMut<ViewportView>,
    hover: Res<HoverMap>,
    q_area: Query<Entity, With<ViewportArea>>,
    kind: Res<ActiveKind>,
    eda2d: Res<Eda2d>,
    focus: Res<bevy::input_focus::InputFocus>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let sample = input
        .0
        .lock()
        .ok()
        .and_then(|receiver| coalesce_recent_samples(receiver.try_iter()));
    let Some(sample) = sample else {
        return;
    };

    let viewport_active = matches!(*kind, ActiveKind::PartStudio | ActiveKind::Assembly)
        && !eda2d.flat()
        && viewport::pointer_over_viewport(&hover, &q_area)
        && focus.get().is_none()
        && windows.iter().any(|window| window.focused);
    if !viewport_active {
        return;
    }

    apply_sample(&mut view, sample);
}

/// Coalesce a short recent input window instead of replaying every queued HID report after a
/// render stall. That keeps device motion responsive without turning a temporary frame drop into
/// a visible camera jump.
fn coalesce_recent_samples(
    samples: impl IntoIterator<Item = MotionSample>,
) -> Option<MotionSample> {
    let mut recent = VecDeque::with_capacity(MAX_QUEUED_SAMPLES);
    for sample in samples {
        if recent.len() == MAX_QUEUED_SAMPLES {
            recent.pop_front();
        }
        recent.push_back(sample);
    }

    let mut weighted_axes = [0.0; 6];
    let mut total_dt = 0.0;
    let mut remaining = MAX_INPUT_WINDOW;
    for sample in recent.iter().rev() {
        if remaining <= 0.0 {
            break;
        }
        let dt = sample.dt.clamp(0.0, MAX_INPUT_WINDOW);
        let used_dt = dt.min(remaining);
        if used_dt == 0.0 {
            continue;
        }
        for (weighted, axis) in weighted_axes.iter_mut().zip(sample.axes) {
            *weighted += axis * used_dt;
        }
        total_dt += used_dt;
        remaining -= used_dt;
    }
    if total_dt == 0.0 {
        return None;
    }

    Some(MotionSample {
        axes: weighted_axes.map(|axis| axis / total_dt),
        dt: total_dt,
    })
}

fn apply_sample(view: &mut ViewportView, sample: MotionSample) {
    if sample.axes.iter().all(|axis| axis.abs() < 0.0001) {
        return;
    }
    let dt = sample.dt.clamp(0.0, 0.05);
    if dt <= 0.0 {
        return;
    }
    let [tx, ty, tz, rx, ry, rz] = sample.axes;
    view.animation = None;

    // Horizontal translation follows screen right. The device's forward/backward translation
    // axis is inverted to match object-mode movement on screen. The third axis zooms about the
    // view centre, keeping the pivot from the current view intact.
    view.view.pan(Vec2::new(
        tx * PAN_PIXELS_PER_SECOND * dt,
        ty * PAN_PIXELS_PER_SECOND * dt,
    ));
    if tz.abs() > 0.0001 {
        let factor = (tz * ZOOM_PER_SECOND * dt).exp();
        view.view.zoom_at(factor, Vec2::ZERO);
    }

    let rotation = ROTATION_DEGREES_PER_SECOND.to_radians() * dt;
    // Map the rotation controls to the displayed frame so their effect does not change when the
    // model's world axes point in a different direction on screen. Read the frame once per sample
    // so simultaneous axis input does not drift as each rotation is applied.
    let screen_right = view.view.right();
    let screen_up = view.view.up();
    let screen_depth = view.view.back();
    let pivot = view.pivot;
    let turn = |state: &mut ViewState| {
        state.rotate_model_about(screen_right, rx * rotation);
        state.rotate_model_about(screen_depth, ry * rotation);
        state.rotate_model_about(screen_up, -rz * rotation);
    };
    if let Some(pivot) = pivot {
        view.view.orbit_about(pivot, turn);
    } else {
        turn(&mut view.view);
    }
}

#[cfg(target_os = "windows")]
mod windows_backend {
    use super::*;
    use hidapi::{HidApi, HidDevice};
    use std::thread;
    use std::time::Instant;

    const VENDOR_ID: u16 = 0x046d;
    const SPACE_NAVIGATOR_PRODUCT_ID: u16 = 0xc626;

    pub(super) fn start(sender: Sender<MotionSample>, wheel_guard: LegacyWheelGuard) {
        let _ = thread::Builder::new()
            .name("cadrs-spacemouse-hid".into())
            .spawn(move || run(sender, wheel_guard));
    }

    fn run(sender: Sender<MotionSample>, wheel_guard: LegacyWheelGuard) {
        loop {
            let Ok(api) = HidApi::new() else {
                thread::sleep(Duration::from_secs(2));
                continue;
            };
            let device = api
                .device_list()
                .find(|device| {
                    device.vendor_id() == VENDOR_ID
                        && device.product_id() == SPACE_NAVIGATOR_PRODUCT_ID
                })
                .and_then(|device| device.open_device(&api).ok());
            let Some(device) = device else {
                thread::sleep(Duration::from_millis(750));
                continue;
            };
            bevy::log::info!("SpaceNavigator HID input connected");
            if !read_device(&device, &sender, &wheel_guard) {
                let _ = sender.send(MotionSample::default());
                bevy::log::debug!("SpaceNavigator HID input disconnected");
            }
            thread::sleep(Duration::from_millis(250));
        }
    }

    fn read_device(
        device: &HidDevice,
        sender: &Sender<MotionSample>,
        wheel_guard: &LegacyWheelGuard,
    ) -> bool {
        let mut sample = MotionSample::default();
        let mut last_report = Instant::now();
        let mut report = [0_u8; 32];
        loop {
            let count = match device.read_timeout(&mut report, 20) {
                Ok(count) => count,
                Err(error) => {
                    bevy::log::debug!(%error, "SpaceNavigator HID read failed");
                    return false;
                }
            };
            if count == 0 {
                continue;
            }
            let now = Instant::now();
            sample.dt = now.duration_since(last_report).as_secs_f32().min(0.05);
            last_report = now;
            if let Some((report_id, values)) = parse_report(&report[..count]) {
                match report_id {
                    0x01 => {
                        sample.axes[..3].copy_from_slice(&values);
                        if values[1].abs() > 0.0001 || values[2].abs() > 0.0001 {
                            wheel_guard.mark_space_mouse_translation();
                        }
                    }
                    0x02 => sample.axes[3..].copy_from_slice(&values),
                    _ => continue,
                }
                if sender.send(sample).is_err() {
                    return false;
                }
            }
        }
    }

    fn parse_report(report: &[u8]) -> Option<(u8, [f32; 3])> {
        if report.len() < 7 {
            return None;
        }
        let report_id = report[0];
        if !matches!(report_id, 0x01 | 0x02) {
            return None;
        }
        let values = std::array::from_fn(|axis| {
            let start = 1 + axis * 2;
            let value = i16::from_le_bytes([report[start], report[start + 1]]) as f32;
            (value / 512.0).clamp(-1.0, 1.0)
        });
        Some((report_id, values))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_signed_little_endian_axis_reports() {
            let report = [1, 0x00, 0x02, 0x00, 0xfe, 0x00, 0x00];
            let (id, axes) = parse_report(&report).unwrap();
            assert_eq!(id, 1);
            assert_eq!(axes, [1.0, -1.0, 0.0]);
        }

        #[test]
        fn ignores_unknown_or_short_reports() {
            assert!(parse_report(&[1, 0, 0]).is_none());
            assert!(parse_report(&[3, 0, 0, 0, 0, 0, 0]).is_none());
        }
    }
}

#[cfg(target_os = "linux")]
mod linux_backend {
    use super::*;
    use spnav_rs::{SpnavClient, SpnavEvent};
    use std::thread;

    pub(super) fn start(sender: Sender<MotionSample>) {
        let _ = thread::Builder::new()
            .name("cadrs-spacemouse-spnav".into())
            .spawn(move || run(sender));
    }

    fn run(sender: Sender<MotionSample>) {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                bevy::log::warn!(%error, "cannot start SpaceMouse event runtime");
                return;
            }
        };
        loop {
            let mut client = match runtime.block_on(SpnavClient::open()) {
                Ok(client) => client,
                Err(_) => {
                    thread::sleep(Duration::from_secs(2));
                    continue;
                }
            };
            bevy::log::info!("Connected to the spacenav daemon");
            loop {
                let event = runtime.block_on(tokio::time::timeout(
                    Duration::from_millis(100),
                    client.wait_event(),
                ));
                match event {
                    Ok(Ok(SpnavEvent::Motion(motion))) => {
                        let sample = MotionSample {
                            axes: [
                                normalize(motion.x),
                                normalize(motion.y),
                                normalize(motion.z),
                                normalize(motion.rx),
                                normalize(motion.ry),
                                normalize(motion.rz),
                            ],
                            dt: (motion.period as f32 / 1000.0).clamp(0.0, 0.05),
                        };
                        if sender.send(sample).is_err() {
                            return;
                        }
                    }
                    Ok(Ok(_)) | Err(_) => {}
                    Ok(Err(error)) => {
                        bevy::log::debug!(%error, "spacenav connection ended");
                        let _ = sender.send(MotionSample::default());
                        break;
                    }
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    }

    fn normalize(value: i32) -> f32 {
        (value as f32 / 350.0).clamp(-1.0, 1.0)
    }

    #[cfg(test)]
    mod tests {
        use super::normalize;

        #[test]
        fn normalizes_and_clamps_daemon_axis_values() {
            assert_eq!(normalize(350), 1.0);
            assert_eq!(normalize(-175), -0.5);
            assert_eq!(normalize(700), 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_input_does_not_move_the_camera() {
        let mut view = ViewportView::default();
        let before = view.view;
        apply_sample(&mut view, MotionSample::default());
        assert_eq!(view.view, before);
    }

    #[test]
    fn translation_and_rotation_move_the_camera() {
        let mut view = ViewportView::default();
        apply_sample(
            &mut view,
            MotionSample {
                axes: [1.0, 0.5, 0.5, 0.5, 0.5, 0.5],
                dt: 1.0 / 60.0,
            },
        );
        assert_ne!(view.view.focus, Vec3::ZERO);
        assert_ne!(view.view.scale, ViewState::default().scale);
        assert_ne!(view.view.rotation(), ViewState::default().rotation());
    }

    #[test]
    fn forward_translation_matches_object_mode_and_positive_depth_zooms_in() {
        let mut view = ViewportView::default();
        let dt = 0.02;
        let mut expected = view.view;
        expected.pan(Vec2::new(0.0, PAN_PIXELS_PER_SECOND * dt));
        let initial_scale = view.view.scale;

        apply_sample(
            &mut view,
            MotionSample {
                axes: [0.0, 1.0, 1.0, 0.0, 0.0, 0.0],
                dt,
            },
        );

        assert_eq!(view.view.focus, expected.focus);
        assert!(view.view.scale < initial_scale);
    }

    #[test]
    fn orbit_about_pivot_keeps_the_pivot_stationary() {
        let mut view = ViewportView::default();
        let pivot = Vec3::new(20.0, 10.0, 5.0);
        let screen_before = view.view.project(pivot);
        view.pivot = Some(pivot);
        apply_sample(
            &mut view,
            MotionSample {
                axes: [0.0, 0.0, 0.0, 0.0, 0.0, 1.0],
                dt: 1.0 / 30.0,
            },
        );
        let screen_after = view.view.project(pivot);
        assert!(screen_before.distance(screen_after) < 1e-4);
    }

    #[test]
    fn rotation_axes_follow_the_displayed_view_frame() {
        for (axes, axis_index, camera_direction) in [
            ([0.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0, -1.0),
            ([0.0, 0.0, 0.0, 0.0, 1.0, 0.0], 1, -1.0),
            ([0.0, 0.0, 0.0, 0.0, 0.0, 1.0], 2, 1.0),
        ] {
            let mut view = ViewportView::default();
            view.view.azimuth = 53.0;
            view.view.elevation = 41.0;
            view.view.roll = 17.0;
            let before = view.view.rotation();
            let screen_axes = [view.view.right(), view.view.back(), view.view.up()];
            let radians = ROTATION_DEGREES_PER_SECOND.to_radians() / 30.0;

            apply_sample(
                &mut view,
                MotionSample {
                    axes,
                    dt: 1.0 / 30.0,
                },
            );

            let expected =
                Quat::from_axis_angle(screen_axes[axis_index], camera_direction * radians) * before;
            assert!(view.view.rotation().angle_between(expected) < 1e-4);
        }
    }

    #[test]
    fn legacy_wheel_is_suppressed_after_space_mouse_translation() {
        let guard = LegacyWheelGuard::default();
        assert!(!guard.suppress_legacy_wheel());
        guard.mark_space_mouse_translation();
        assert!(guard.suppress_legacy_wheel());
    }

    #[test]
    fn coalesces_only_the_recent_input_window() {
        let sample = |axis, dt| MotionSample {
            axes: [axis, 0.0, 0.0, 0.0, 0.0, 0.0],
            dt,
        };
        let coalesced =
            coalesce_recent_samples([sample(1.0, 0.05), sample(-1.0, 0.025), sample(0.0, 0.025)])
                .unwrap();

        assert!((coalesced.dt - MAX_INPUT_WINDOW).abs() < 1e-6);
        assert!((coalesced.axes[0] + 0.5).abs() < 1e-6);
    }
}
