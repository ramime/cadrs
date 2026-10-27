//! The local preferences (P3E.3, TD6.2, D2.2): the app menu's **Preferences…** opens a
//! dialog with the **3D view mouse controls** preset (Onshape, SolidWorks, Inventor, Creo) and
//! what its gestures do (Rotate, Rotate without roll, Pan, Zoom). OK keeps the choice in the store root's
//! `preferences.ron` ([`cadrs_core::preferences`]); it is this machine's, not a document's, so
//! nothing is undone. The Part Studio and Assembly viewports (`crate::viewport`) and the
//! drawing sheet (`crate::drawing`) read it for their drags.
//!
//! **Interface › UI scale** (issue #12): 75–200 % of the display's own scale, for large or
//! high-resolution screens; applied to the whole window as soon as OK is pressed.
//!
//! **Color scheme**: the top bar's light / dark / system toggle ([`cadrs_ui::scheme_toggle`])
//! is saved here too, as soon as it's clicked. `CADRS_COLOR_SCHEME=light|dark|system` overrides
//! it for one run without saving (scenarios use it to render in dark).
//!
//! Below it, **AI assistants (MCP)**: the *Enable MCP server* switch and the endpoint it serves
//! (`crate::mcp` starts and stops the server as OK changes it).
//!
//! Last, **Collaboration**: the cadrs ID while online, and *Connect when cadrs starts*
//! (`autoconnect_p2p`, off by default: cadrs stays offline until the user connects,
//! [`crate::network`]). It takes effect at the next start; the top bar's network button goes
//! online or offline now.

use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;
use bevy::text::{EditableText, FontWeight};
use bevy::ui_widgets::{Activate, observe};
use cadrs_core::preferences::{
    ColorScheme, Modifiers, MouseButton, MousePreset, Preferences, UI_SCALES, ViewAction,
};
use cadrs_ui::prelude::*;
use cadrs_ui::{
    Button, DialogClose, NumberField, NumberFieldCommit, NumberFieldEdit, NumberFieldState, Select,
    SelectChange, SelectState, SelectableText, SwitchState, form_row,
};
// The MCP server's switch (not in a page).
#[cfg(not(target_arch = "wasm32"))]
use cadrs_ui::Switch;

use crate::{AppState, DocumentStore};

pub struct PreferencesPlugin;

impl Plugin for PreferencesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LocalPreferences>()
            .add_systems(Startup, load_preferences)
            .add_systems(Update, (apply_ui_scale, save_color_scheme))
            .add_observer(on_preset_change)
            .add_observer(on_rotation_snap_commit);
    }
}

/// The preferences in force (read at startup from the store root).
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct LocalPreferences(pub Preferences);

impl LocalPreferences {
    pub fn mouse(&self) -> MousePreset {
        self.0.mouse
    }
}

fn load_preferences(
    store: Option<Res<DocumentStore>>,
    mut prefs: ResMut<LocalPreferences>,
    mut appearance: ResMut<cadrs_ui::Appearance>,
) {
    if let Some(store) = store {
        prefs.0 = Preferences::load(store.0.root());
    }
    appearance.preference = to_ui(scheme_override().unwrap_or(prefs.0.color_scheme));
}

/// `CADRS_COLOR_SCHEME`, when set to a scheme.
fn scheme_override() -> Option<ColorScheme> {
    match std::env::var("CADRS_COLOR_SCHEME").ok()?.as_str() {
        "light" => Some(ColorScheme::Light),
        "dark" => Some(ColorScheme::Dark),
        "system" => Some(ColorScheme::System),
        _ => None,
    }
}

fn to_ui(c: ColorScheme) -> cadrs_ui::SchemePreference {
    match c {
        ColorScheme::System => cadrs_ui::SchemePreference::System,
        ColorScheme::Light => cadrs_ui::SchemePreference::Light,
        ColorScheme::Dark => cadrs_ui::SchemePreference::Dark,
    }
}

fn from_ui(p: cadrs_ui::SchemePreference) -> ColorScheme {
    match p {
        cadrs_ui::SchemePreference::System => ColorScheme::System,
        cadrs_ui::SchemePreference::Light => ColorScheme::Light,
        cadrs_ui::SchemePreference::Dark => ColorScheme::Dark,
    }
}

/// Saves the scheme the toggle picks (not one `CADRS_COLOR_SCHEME` set).
fn save_color_scheme(world: &mut World) {
    let picked = from_ui(world.resource::<cadrs_ui::Appearance>().preference);
    let prefs = world.resource::<LocalPreferences>().0;
    if prefs.color_scheme == picked || scheme_override() == Some(picked) {
        return;
    }
    world.resource_mut::<LocalPreferences>().0.color_scheme = picked;
    save(world);
}

/// Draws the interface at the preferred size (issue #12): the window's scale factor is the
/// display's own times the preference, so text, panels and the view's handles grow together and
/// pointer positions stay in the same units as the layout.
fn apply_ui_scale(
    prefs: Res<LocalPreferences>,
    mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
    mut applied: Local<Option<f32>>,
    mut original: Local<Option<Option<f32>>>,
) {
    let k = prefs.0.ui_scale_factor();
    if *applied == Some(k) {
        return;
    }
    let Ok(mut w) = windows.single_mut() else {
        return;
    };
    // The window's own override (a scenario replay's 1.0) is what 100% puts back.
    let own = *original.get_or_insert(w.resolution.scale_factor_override());
    if applied.is_none() && k == 1.0 {
        *applied = Some(k);
        return;
    }
    let want = if k == 1.0 {
        own
    } else {
        Some(own.unwrap_or(w.resolution.base_scale_factor()) * k)
    };
    w.resolution.set_scale_factor_override(want);
    *applied = Some(k);
}

/// Sets and saves the interface size in percent (the dialog's OK; scenarios too).
pub fn set_ui_scale(world: &mut World, percent: u16) {
    world.resource_mut::<LocalPreferences>().0.ui_scale = percent;
    save(world);
}

/// The modifier keys held.
pub fn modifiers(keys: &ButtonInput<KeyCode>) -> Modifiers {
    Modifiers {
        shift: keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]),
        ctrl: keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]),
        alt: keys.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]),
    }
}

/// A pointer button as the preferences name it.
pub fn mouse_button(b: PointerButton) -> MouseButton {
    match b {
        PointerButton::Primary => MouseButton::Left,
        PointerButton::Secondary => MouseButton::Right,
        PointerButton::Middle => MouseButton::Middle,
    }
}

/// Sets and saves the mouse preset (the dialog's OK; scenarios too).
pub fn set_mouse_preset(world: &mut World, preset: MousePreset) {
    world.resource_mut::<LocalPreferences>().0.mouse = preset;
    save(world);
}

/// Turns the MCP server on or off and saves the choice (the dialog's OK; scenarios too).
pub fn set_mcp_enabled(world: &mut World, on: bool) {
    world.resource_mut::<LocalPreferences>().0.mcp.enabled = on;
    save(world);
}

/// Whether cadrs goes online as soon as it starts, saved (the network button's menu).
pub fn set_autoconnect_p2p(world: &mut World, on: bool) {
    world.resource_mut::<LocalPreferences>().0.autoconnect_p2p = on;
    save(world);
}

/// Remembers that the user has seen "What's new" up to `edition`
/// ([`crate::notifications::WHATS_NEW`]).
pub(crate) fn mark_whats_new_seen(world: &mut World, edition: u32) {
    if world.resource::<LocalPreferences>().0.whats_new_seen >= edition {
        return;
    }
    world.resource_mut::<LocalPreferences>().0.whats_new_seen = edition;
    save(world);
}

fn save(world: &mut World) {
    let prefs = world.resource::<LocalPreferences>().0;
    if let Some(store) = world.get_resource::<DocumentStore>()
        && let Err(e) = prefs.save(store.0.root())
    {
        warn!("cannot save the preferences: {e}");
    }
}

// ---------------------------------------------------------------------------------------------
// The dialog

/// The dialog's root.
#[derive(Component)]
struct PreferencesDialog;

/// A gesture row's text (its action).
#[derive(Component)]
struct GestureText(ViewAction);

const ACTIONS: [(ViewAction, &str); 4] = [
    (ViewAction::Rotate, "pref-mouse-rotate"),
    (ViewAction::RotateTurntable, "pref-mouse-turntable"),
    (ViewAction::Pan, "pref-mouse-pan"),
    (ViewAction::Zoom, "pref-mouse-zoom"),
];

fn gesture_text(preset: MousePreset, action: ViewAction) -> String {
    let g = preset.gestures(action);
    if g.is_empty() {
        "—".into()
    } else {
        g.join(", ")
    }
}

/// Opens the Preferences dialog with the preferences in force.
pub fn open_preferences(world: &mut World) {
    let preset = world.resource::<LocalPreferences>().mouse();
    #[cfg(not(target_arch = "wasm32"))]
    let mcp = world.resource::<LocalPreferences>().0.mcp;
    let autoconnect = world.resource::<LocalPreferences>().0.autoconnect_p2p;
    let snap = world
        .resource::<LocalPreferences>()
        .0
        .rotation_snap_degrees();
    let tolerance = world
        .resource::<LocalPreferences>()
        .0
        .rotation_snap_tolerance_degrees();
    let ui_scale = world.resource::<LocalPreferences>().0.ui_scale;
    #[cfg(not(target_arch = "wasm32"))]
    let endpoint = cadrs_mcp::url(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        mcp.port,
    )));
    let cadrs_id = cadrs_net::cadrs_id(world.get_resource::<cadrs_net::bevy_iroh::Iroh>());
    let state = *world.resource::<State<AppState>>().get();
    let theme = world.resource::<Theme>().clone();
    let (tb, tf) = (theme.clone(), theme.clone());
    world.commands().spawn((
        Dialog::new("preferences-dialog")
            .title("Preferences")
            .width(440.0)
            .body(move |b| {
                let t = &tb;
                b.spawn((
                    Name::new("pref-mouse-heading"),
                    t.text("3D view mouse controls", t.font_base, FontWeight::BOLD, t.foreground),
                    Node { margin: UiRect::bottom(Val::Px(4.0)), ..default() },
                ));
                let mut select = Select::new("pref-mouse-preset").width(Val::Px(200.0));
                for p in MousePreset::ALL {
                    select = select.option(p.label(), true);
                }
                let selected = MousePreset::ALL.iter().position(|p| *p == preset).unwrap_or(0);
                b.spawn(form_row(t, "pref-mouse-row", "Mouse controls", 150.0)).with_child(select.selected(selected).build(t));
                for (action, name) in ACTIONS {
                    b.spawn(form_row(t, format!("{name}-row"), action.label(), 150.0)).with_child((
                        Name::new(name),
                        GestureText(action),
                        t.text(gesture_text(preset, action), t.font_base, FontWeight::NORMAL, t.muted_foreground),
                    ));
                }
                b.spawn((
                    Name::new("pref-interface-heading"),
                    t.text("Interface", t.font_base, FontWeight::BOLD, t.foreground),
                    Node { margin: UiRect::new(Val::ZERO, Val::ZERO, Val::Px(14.0), Val::Px(4.0)), ..default() },
                ));
                let mut sizes = Select::new("pref-ui-scale").width(Val::Px(200.0));
                for p in UI_SCALES {
                    sizes = sizes.option(if p == 100 { "100% (display default)".to_string() } else { format!("{p}%") }, true);
                }
                let at = UI_SCALES.iter().position(|p| *p == ui_scale).unwrap_or(2);
                b.spawn(form_row(t, "pref-ui-scale-row", "UI scale", 150.0)).with_child(sizes.selected(at).build(t));
                b.spawn((
                    Name::new("pref-assembly-heading"),
                    t.text("Assembly manipulation", t.font_base, FontWeight::BOLD, t.foreground),
                    Node { margin: UiRect::new(Val::ZERO, Val::ZERO, Val::Px(14.0), Val::Px(4.0)), ..default() },
                ));
                b.spawn(NumberField::new("pref-rotation-snap", "Rotation snap increment").text(format!("{snap} deg")).label_width(200.0).build(t));
                b.spawn(NumberField::new("pref-rotation-snap-tolerance", "Snap tolerance").text(format!("{tolerance} deg")).label_width(200.0).build(t));
                b.spawn((
                    Name::new("pref-rotation-snap-hint"),
                    t.text("Snap when within the tolerance of an increment. Hold Shift to disable snapping. Increment: 0–180° (0 disables). Tolerance: 0–90°.", t.font_sm, FontWeight::NORMAL, t.muted_foreground),
                    Node { width: Val::Px(400.0), margin: UiRect::top(Val::Px(6.0)), ..default() },
                )).insert(TextLayout::default());
                // A page can't serve one.
                #[cfg(not(target_arch = "wasm32"))]
                {
                b.spawn((
                    Name::new("pref-mcp-heading"),
                    t.text("AI assistants (MCP)", t.font_base, FontWeight::BOLD, t.foreground),
                    Node { margin: UiRect::new(Val::ZERO, Val::ZERO, Val::Px(14.0), Val::Px(4.0)), ..default() },
                ));
                b.spawn(form_row(t, "pref-mcp-row", "MCP server", 150.0))
                    .with_child(Switch::new("pref-mcp-enabled").label("Enabled").on(mcp.enabled).build(t));
                b.spawn(form_row(t, "pref-mcp-endpoint-row", "Endpoint", 150.0)).with_child((
                    Name::new("pref-mcp-endpoint"),
                    t.text(endpoint, t.font_base, FontWeight::NORMAL, t.muted_foreground),
                ));
                b.spawn((
                    Name::new("pref-mcp-note"),
                    t.text(
                        "While it is on, AI assistants on this computer (Claude, Cursor, …) can create and edit documents.",
                        t.font_sm,
                        FontWeight::NORMAL,
                        t.muted_foreground,
                    ),
                    Node { width: Val::Px(400.0), margin: UiRect::top(Val::Px(6.0)), ..default() },
                ))
                // Wrapped (the theme's text doesn't wrap).
                .insert(TextLayout::default());
                }
                b.spawn((
                    Name::new("pref-collab-heading"),
                    t.text("Collaboration", t.font_base, FontWeight::BOLD, t.foreground),
                    Node { margin: UiRect::new(Val::ZERO, Val::ZERO, Val::Px(14.0), Val::Px(4.0)), ..default() },
                ));
                b.spawn(form_row(t, "pref-cadrs-id-row", "cadrs ID", 150.0)).with_children(|r| {
                    match &cadrs_id {
                        Some(id) => {
                            r.spawn(SelectableText::new("pref-cadrs-id").value(id.clone()).font(t.font_sm, FontWeight::NORMAL).copy_button().build(t));
                        }
                        None => {
                            r.spawn((
                                Name::new("pref-cadrs-id"),
                                t.text("Shown while online", t.font_base, FontWeight::NORMAL, t.muted_foreground),
                            ));
                        }
                    }
                });
                b.spawn(form_row(t, "pref-p2p-row", "P2P networking", 150.0))
                    .with_child(Switch::new("pref-p2p-autoconnect").label("Connect when cadrs starts").on(autoconnect).build(t));
                b.spawn((
                    Name::new("pref-collab-note"),
                    t.text(
                        "Documents are shared peer to peer, with no server, through iroh's relays. Off, cadrs stays offline until you connect with the network button in the top bar, or share. Give your cadrs ID to someone who wants to share a document with you.",
                        t.font_sm,
                        FontWeight::NORMAL,
                        t.muted_foreground,
                    ),
                    Node { width: Val::Px(400.0), margin: UiRect::top(Val::Px(6.0)), ..default() },
                ))
                .insert(TextLayout::default());
            })
            .footer(move |f| {
                let t = &tf;
                f.spawn((
                    Button::new("pref-ok").label("OK").primary().build(t),
                    observe(|_: On<Activate>, mut commands: Commands| commands.queue(save_preferences)),
                ));
                f.spawn((
                    Button::new("pref-cancel").label("Cancel").build(t),
                    observe(|_: On<Activate>, q: Query<Entity, With<PreferencesDialog>>, mut commands: Commands| {
                        for e in &q {
                            commands.trigger(DialogClose { entity: e });
                        }
                    }),
                ));
            })
            .build(&theme),
        PreferencesDialog,
        DespawnOnExit(state),
    ));
    world.flush();
}

/// The preset changed in the dialog: its gestures follow.
fn on_preset_change(
    ev: On<SelectChange>,
    q: Query<&Name>,
    mut q_text: Query<(&GestureText, &mut Text)>,
) {
    if q.get(ev.entity).map(|n| n.as_str()) != Ok("pref-mouse-preset") {
        return;
    }
    let Some(preset) = MousePreset::ALL.get(ev.index).copied() else {
        return;
    };
    for (g, mut t) in &mut q_text {
        t.0 = gesture_text(preset, g.0);
    }
}

/// OK: the chosen preset, interface size and the MCP and P2P switches are kept.
fn save_preferences(world: &mut World) {
    let mut q = world.query_filtered::<Entity, With<PreferencesDialog>>();
    let Some(dialog) = q.iter(world).next() else {
        return;
    };
    // Read the live edit so clicking OK works even before a blur-commit is processed.
    let mut fields = world.query::<(&NumberFieldEdit, &EditableText)>();
    let values: Vec<_> = fields
        .iter(world)
        .filter_map(|(edit, text)| {
            let name = world.get::<Name>(edit.0)?.as_str();
            matches!(name, "pref-rotation-snap" | "pref-rotation-snap-tolerance")
                .then(|| (edit.0, name.to_string(), text.value().to_string()))
        })
        .collect();
    let mut validated = Vec::new();
    for (entity, name, text) in values {
        let value = parse_rotation_snap(&text)
            .filter(|v| name != "pref-rotation-snap-tolerance" || *v <= 90.0);
        let Some(value) = value else {
            if let Some(mut state) = world.get_mut::<NumberFieldState>(entity) {
                state.text = text;
                state.error = true;
            }
            return;
        };
        validated.push((name, value));
    }
    for (name, value) in validated {
        let mut prefs = world.resource_mut::<LocalPreferences>();
        if name == "pref-rotation-snap-tolerance" {
            prefs.0.rotation_snap_tolerance_degrees = value;
        } else {
            prefs.0.rotation_snap_degrees = value;
        }
    }
    let mut qs = world.query::<(&Name, &SelectState)>();
    let preset = qs
        .iter(world)
        .find(|(n, _)| n.as_str() == "pref-mouse-preset")
        .and_then(|(_, s)| MousePreset::ALL.get(s.selected).copied());
    if let Some(p) = preset {
        world.resource_mut::<LocalPreferences>().0.mouse = p;
    }
    let size = qs
        .iter(world)
        .find(|(n, _)| n.as_str() == "pref-ui-scale")
        .and_then(|(_, s)| UI_SCALES.get(s.selected).copied());
    if let Some(p) = size {
        set_ui_scale(world, p);
    }
    let mut qm = world.query::<(&Name, &SwitchState)>();
    let mcp = qm
        .iter(world)
        .find(|(n, _)| n.as_str() == "pref-mcp-enabled")
        .map(|(_, s)| s.on);
    if let Some(on) = mcp {
        world.resource_mut::<LocalPreferences>().0.mcp.enabled = on;
    }
    let autoconnect = qm
        .iter(world)
        .find(|(n, _)| n.as_str() == "pref-p2p-autoconnect")
        .map(|(_, s)| s.on);
    if let Some(on) = autoconnect {
        world.resource_mut::<LocalPreferences>().0.autoconnect_p2p = on;
    }
    save(world);
    world.trigger(DialogClose { entity: dialog });
}

fn parse_rotation_snap(text: &str) -> Option<f64> {
    let value: f64 = text
        .trim()
        .trim_end_matches("deg")
        .trim_end_matches('°')
        .trim()
        .parse()
        .ok()?;
    (value.is_finite() && (0.0..=180.0).contains(&value)).then_some(value)
}

fn on_rotation_snap_commit(
    ev: On<NumberFieldCommit>,
    mut fields: Query<(&Name, &mut NumberFieldState)>,
) {
    let Ok((name, mut state)) = fields.get_mut(ev.entity) else {
        return;
    };
    if !matches!(
        name.as_str(),
        "pref-rotation-snap" | "pref-rotation-snap-tolerance"
    ) {
        return;
    }
    state.text = ev.text.clone();
    state.error = parse_rotation_snap(&ev.text)
        .filter(|v| name.as_str() != "pref-rotation-snap-tolerance" || *v <= 90.0)
        .is_none();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_dialog_exposes_the_rotation_increment_and_shift_hint() {
        let mut world = World::new();
        world.init_resource::<LocalPreferences>();
        world.init_resource::<Theme>();
        world.insert_resource(State::new(AppState::Document));
        open_preferences(&mut world);
        let mut fields = world.query::<(&Name, &NumberFieldState)>();
        let (_, snap) = fields
            .iter(&world)
            .find(|(name, _)| name.as_str() == "pref-rotation-snap")
            .unwrap();
        assert_eq!(snap.text, "45 deg");
        let (_, tolerance) = fields
            .iter(&world)
            .find(|(name, _)| name.as_str() == "pref-rotation-snap-tolerance")
            .unwrap();
        assert_eq!(tolerance.text, "10 deg");
        let mut texts = world.query::<(&Name, &Text)>();
        let (_, hint) = texts
            .iter(&world)
            .find(|(name, _)| name.as_str() == "pref-rotation-snap-hint")
            .unwrap();
        assert!(hint.0.contains("Shift"));
    }

    #[test]
    fn rotation_snap_accepts_fractional_degrees_and_validates_before_saving() {
        assert_eq!(parse_rotation_snap("22.5 deg"), Some(22.5));
        assert_eq!(parse_rotation_snap("0°"), Some(0.0));
        assert_eq!(parse_rotation_snap("180"), Some(180.0));
        for text in ["-1", "181", "NaN", "inf", "", "abc"] {
            assert_eq!(parse_rotation_snap(text), None);
        }
        let mut world = World::new();
        world.init_resource::<LocalPreferences>();
        world.spawn(PreferencesDialog);
        let field = world
            .spawn((
                Name::new("pref-rotation-snap"),
                NumberFieldState {
                    text: "45 deg".into(),
                    error: false,
                },
            ))
            .id();
        let edit = world
            .spawn((NumberFieldEdit(field), EditableText::new("22.5")))
            .id();
        let tolerance = world
            .spawn((
                Name::new("pref-rotation-snap-tolerance"),
                NumberFieldState::default(),
            ))
            .id();
        let tolerance_edit = world
            .spawn((NumberFieldEdit(tolerance), EditableText::new("15")))
            .id();
        save_preferences(&mut world);
        assert_eq!(
            world
                .resource::<LocalPreferences>()
                .0
                .rotation_snap_degrees(),
            22.5,
            "OK must read the live edit without requiring Enter"
        );
        assert_eq!(
            world
                .resource::<LocalPreferences>()
                .0
                .rotation_snap_tolerance_degrees(),
            15.0
        );
        world.entity_mut(edit).insert(EditableText::new("-10"));
        save_preferences(&mut world);
        assert_eq!(
            world
                .resource::<LocalPreferences>()
                .0
                .rotation_snap_degrees(),
            22.5,
            "invalid edits must not change saved preferences"
        );
        assert!(world.get::<NumberFieldState>(field).unwrap().error);
        world.entity_mut(edit).insert(EditableText::new("60"));
        world
            .entity_mut(tolerance_edit)
            .insert(EditableText::new("91"));
        save_preferences(&mut world);
        assert_eq!(
            world
                .resource::<LocalPreferences>()
                .0
                .rotation_snap_degrees(),
            22.5,
            "invalid tolerance must not partially apply a new increment"
        );
        assert_eq!(
            world
                .resource::<LocalPreferences>()
                .0
                .rotation_snap_tolerance_degrees(),
            15.0
        );
        assert!(world.get::<NumberFieldState>(tolerance).unwrap().error);
    }

    #[test]
    fn gesture_rows_read_like_the_table() {
        assert_eq!(
            gesture_text(MousePreset::Onshape, ViewAction::Rotate),
            "Right drag"
        );
        assert_eq!(
            gesture_text(MousePreset::Onshape, ViewAction::RotateTurntable),
            "Alt+Right drag"
        );
        assert_eq!(
            gesture_text(MousePreset::SolidWorks, ViewAction::RotateTurntable),
            "—"
        );
        assert_eq!(
            gesture_text(MousePreset::Onshape, ViewAction::Pan),
            "Middle drag, Ctrl+Right drag"
        );
        assert_eq!(
            gesture_text(MousePreset::SolidWorks, ViewAction::Zoom),
            "Scroll wheel, Shift+Middle drag"
        );
    }
}
