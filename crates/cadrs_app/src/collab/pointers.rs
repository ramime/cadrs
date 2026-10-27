//! Everyone's pointer and camera, live, in a shared document ([`Pointer`]).
//!
//! - Each peer tells the room where its pointer is in its tab, many times a second while it
//!   moves: in a 3D view (Part Studio, Assembly, a PCB or CAM Studio's 3D) the model point
//!   under it, on the part there or else on the ground grid; on a PCB Studio's schematic,
//!   layout, symbol or footprint the design point; on a drawing the sheet point.
//! - Someone in the same tab (and on the same 2D view or sheet) shows in their colour
//!   ([`super::presence::color_of`]), smoothed between updates:
//!   - in 3D, as gizmos drawn over the model: a marker where their pointer is, their camera
//!     (where their eye is in perspective; in orthographic, which has none, brought in to just
//!     outside the parts, looking the way they look) and a faint line from it to the pointer,
//!     with their name beside the marker;
//!   - in 2D, as an arrow pointer with their name.
//!
//! Names: `collab-pointer-<first 8 of their cadrs ID>` (a badge), `-name` its name.

use std::collections::BTreeMap;

use bevy::ecs::system::SystemState;
use bevy::picking::hover::HoverMap;
use bevy::prelude::*;
use bevy::text::FontWeight;
use cadrs_core::{DocumentId, ElementId};
use cadrs_net::bevy_iroh::Iroh;
use cadrs_net::bevy_iroh::prelude::{NetSender, PeerLeft, Received};
use cadrs_net::{EdaOf, Pointer, PointerAt};
use cadrs_ui::prelude::*;

use super::live::Live;
use super::presence::{People, color_of};
use crate::ActiveDocument;
use crate::camera::ViewState;
use crate::eda::{Eda2d, EdaPointer, EdaUi, Mode, Subject};
use crate::viewport::{
    ActiveKind, OVERLAY_LAYER, ViewportArea, ViewportDrag, ViewportRect, ViewportView,
};

/// How often a moving pointer is sent at most (20 a second).
const SEND_EVERY: f64 = 0.05;
/// How often a resting pointer is sent again, for whoever joined since.
const RESEND_EVERY: f64 = 2.0;
/// Someone not heard from this long is gone.
const FORGET_AFTER: f64 = 15.0;
/// How quickly a shown pointer catches up with the last one heard (per second).
const FOLLOW_RATE: f32 = 18.0;
/// The arrow's size (px) and where its tip is on it (icon-rs's `cursor`: 5, 2.5 of 24).
const ARROW: f32 = 18.0;
const ARROW_TIP: Vec2 = Vec2::new(5.0 / 24.0 * ARROW, 2.5 / 24.0 * ARROW);
/// A pointer that has not moved for this long is put away (it is back when it moves).
const IDLE_AFTER: f64 = 10.0;
/// The 3D pointer's dot and the camera chip (px), and the camera's length on this screen.
const DOT: f32 = 12.0;
const CHIP: f32 = 18.0;
const CAMERA_PX: f32 = 34.0;

pub(super) fn plugin(app: &mut App) {
    // Registered here as well: without the network the systems below still need the message.
    app.add_message::<Received<Pointer>>()
        .add_message::<PeerLeft>()
        .init_resource::<Pointers>()
        .init_gizmo_group::<CollabGizmos>()
        .init_gizmo_group::<CollabHiddenGizmos>()
        .add_systems(Startup, configure_gizmos)
        .add_systems(
            Update,
            (
                (tell, hear).run_if(resource_exists::<Iroh>),
                (smooth, sync_badges, draw_gizmos),
            )
                .chain(),
        );
}

/// The collaborators' 3D marks (their cameras, the lines to their pointers), where the parts
/// don't hide them.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct CollabGizmos;

/// The same marks faint, over the parts: where they are behind something.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct CollabHiddenGizmos;

fn configure_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<CollabGizmos>();
    config.line.width = 2.0;
    let (config, _) = store.config_mut::<CollabHiddenGizmos>();
    config.line.width = 1.5;
    config.depth_bias = -1.0;
    config.render_layers = bevy::camera::visibility::RenderLayers::layer(OVERLAY_LAYER);
}

/// Someone else's pointer.
#[derive(Debug, Clone)]
struct Remote {
    tab: ElementId,
    /// The last heard, and the one shown (catching up with it).
    at: Option<PointerAt>,
    shown: Option<PointerAt>,
    heard: f64,
    /// When it last moved (it is put away after [`IDLE_AFTER`]).
    moved: f64,
}

impl Remote {
    fn idle(&self, t: f64) -> bool {
        t - self.moved > IDLE_AFTER
    }
}

/// The others' pointers in the shared document open now, by cadrs ID.
#[derive(Resource, Default)]
pub struct Pointers {
    document: Option<DocumentId>,
    by_id: BTreeMap<String, Remote>,
}

fn now(world: &World) -> f64 {
    world.resource::<Time<Real>>().elapsed_secs_f64()
}

// ---------------------------------------------------------------------------------------------
// Telling

/// The app's code for a 2D view's mode, as it travels.
fn mode_code(m: Mode) -> u8 {
    match m {
        Mode::Schematic => 0,
        Mode::Layout => 1,
        Mode::ThreeD => 2,
        Mode::Symbol => 3,
        Mode::Footprint => 4,
    }
}

fn eda_of(s: Subject) -> EdaOf {
    match s {
        Subject::Board(b) => EdaOf::Board(b.0),
        Subject::Component(c) => EdaOf::Component(c.0),
    }
}

/// A 3D tab's view (not a drawing, a render or a table).
fn three_d(kind: ActiveKind) -> bool {
    !kind.is_flat()
}

/// Where this user's pointer is in the active tab, if it is over the tab's view.
fn here(world: &mut World) -> Option<PointerAt> {
    let kind = *world.resource::<ActiveKind>();
    if let Some((_, subject, mode)) = world.resource::<Eda2d>().0
        && mode != Mode::ThreeD
    {
        let p = world.resource::<EdaPointer>();
        return p.over.then_some(PointerAt::Design {
            of: eda_of(subject),
            mode: mode_code(mode),
            at: [p.at.x, p.at.y],
        });
    }
    if kind == ActiveKind::Drawing {
        let doc = world.get_resource::<ActiveDocument>()?;
        let ui = world.get_resource::<crate::drawing::DrawingUi>()?;
        let ((_, sheet), _) = crate::drawing::current_view(doc, ui)?;
        let at = ui.pointer?;
        return over_view(world).then_some(PointerAt::Sheet {
            sheet: sheet.0.as_u128(),
            at: at.to_array(),
        });
    }
    if !three_d(kind) || !over_view(world) {
        return None;
    }
    let offset = world
        .resource::<ViewportRect>()
        .offset(world.resource::<ViewportDrag>().pointer());
    let view = world.resource::<ViewportView>().view;
    let cache = world.resource::<crate::parts::PartCache>();
    Some(PointerAt::Model(
        model_point(cache, &view, offset).to_array(),
    ))
}

/// The model point under a screen offset: on the nearest part there, else on the ground grid
/// (the XY plane), else (looking along it) on the plane through the focus.
fn model_point(cache: &crate::parts::PartCache, view: &ViewState, offset: Vec2) -> Vec3 {
    let (o, d) = view.ray(offset);
    if let Some((_, _, t)) = crate::parts::pick_opaque_face(cache, view, offset, None) {
        return o + d * t;
    }
    if d.z.abs() > 1e-6 {
        let t = -o.z / d.z;
        if t > 0.0 && t < crate::camera::camera_far(view.scale) {
            return o + d * t;
        }
    }
    view.unproject(offset)
}

/// The pointer is over the tab's view, not a panel, menu or dialog.
fn over_view(world: &mut World) -> bool {
    let mut state = SystemState::<(Res<HoverMap>, Query<Entity, With<ViewportArea>>)>::new(world);
    state
        .get(world)
        .is_ok_and(|(hover, area)| crate::viewport::pointer_over_viewport(&hover, &area))
}

fn tell(world: &mut World, mut last: Local<Option<(Pointer, f64)>>) {
    let Some((document, room)) = world.get_resource::<Live>().map(|l| (l.document, l.room()))
    else {
        *last = None;
        return;
    };
    let Some(tab) = world
        .get_resource::<ActiveDocument>()
        .and_then(|d| d.active)
    else {
        return;
    };
    let p = Pointer {
        document,
        tab,
        at: here(world),
    };
    let t = now(world);
    let due = match &*last {
        None => true,
        Some((prev, at)) => {
            (*prev != p && t - at >= SEND_EVERY) || (p.at.is_some() && t - at >= RESEND_EVERY)
        }
    };
    if !due {
        return;
    }
    let mut state = SystemState::<NetSender>::new(world);
    if let Ok(net) = state.get(world) {
        net.broadcast(room, &p);
    }
    *last = Some((p, t));
}

// ---------------------------------------------------------------------------------------------
// Hearing

fn hear(
    mut rx: MessageReader<Received<Pointer>>,
    mut left: MessageReader<PeerLeft>,
    live: Option<Res<Live>>,
    mut pointers: ResMut<Pointers>,
    time: Res<Time<Real>>,
) {
    let t = time.elapsed_secs_f64();
    let Some(live) = live else {
        if pointers.document.is_some() || !pointers.by_id.is_empty() {
            *pointers = Pointers::default();
        }
        rx.clear();
        left.clear();
        return;
    };
    if pointers.document != Some(live.document) {
        *pointers = Pointers {
            document: Some(live.document),
            by_id: BTreeMap::new(),
        };
    }
    for m in rx.read() {
        if m.msg.document != live.document {
            continue;
        }
        let entry = pointers.by_id.entry(m.from.to_string()).or_insert(Remote {
            tab: m.msg.tab,
            at: None,
            shown: None,
            heard: t,
            moved: t,
        });
        if entry.at != m.msg.at || entry.tab != m.msg.tab {
            entry.moved = t;
        }
        // Another tab, or another view of it: shown where it is at once, not slid there.
        if entry.tab != m.msg.tab || !same_place(entry.shown, m.msg.at) {
            entry.shown = m.msg.at;
        }
        entry.tab = m.msg.tab;
        entry.at = m.msg.at;
        entry.heard = t;
    }
    for l in left.read() {
        if l.room == live.room() {
            pointers.by_id.remove(&l.id.to_string());
        }
    }
    pointers.by_id.retain(|_, r| t - r.heard <= FORGET_AFTER);
}

/// Both on the same view (3D, the same 2D view, the same sheet), so one can slide to the other.
fn same_place(a: Option<PointerAt>, b: Option<PointerAt>) -> bool {
    match (a, b) {
        (Some(PointerAt::Model(_)), Some(PointerAt::Model(_))) => true,
        (
            Some(PointerAt::Design { of, mode, .. }),
            Some(PointerAt::Design { of: o, mode: m, .. }),
        ) => of == o && mode == m,
        (Some(PointerAt::Sheet { sheet, .. }), Some(PointerAt::Sheet { sheet: s, .. })) => {
            sheet == s
        }
        _ => false,
    }
}

/// Each shown pointer a step closer to the last one heard.
fn smooth(mut pointers: ResMut<Pointers>, time: Res<Time<Real>>) {
    let k = 1.0 - (-FOLLOW_RATE * time.delta_secs()).exp();
    for r in pointers.by_id.values_mut() {
        r.shown = match (r.shown, r.at) {
            (Some(PointerAt::Model(a)), Some(PointerAt::Model(b))) => Some(PointerAt::Model(
                Vec3::from_array(a).lerp(Vec3::from_array(b), k).to_array(),
            )),
            (
                Some(PointerAt::Design { at: a, .. }),
                Some(PointerAt::Design { of, mode, at: b }),
            ) => {
                let lerp = |a: i64, b: i64| a + ((b - a) as f64 * k as f64).round() as i64;
                Some(PointerAt::Design {
                    of,
                    mode,
                    at: [lerp(a[0], b[0]), lerp(a[1], b[1])],
                })
            }
            (Some(PointerAt::Sheet { at: a, .. }), Some(PointerAt::Sheet { sheet, at: b })) => {
                Some(PointerAt::Sheet {
                    sheet,
                    at: Vec2::from_array(a).lerp(Vec2::from_array(b), k).to_array(),
                })
            }
            (_, at) => at,
        };
    }
}

// ---------------------------------------------------------------------------------------------
// Showing

/// What this user's own tab shows, to place the others' pointers on.
struct Here<'a> {
    tab: Option<ElementId>,
    kind: ActiveKind,
    eda: Option<(Subject, Mode, cadrs_eda::view::View)>,
    /// The sheet shown, its view and where on the screen it is drawn.
    sheet: Option<(u128, crate::drawing::SheetView, Rect)>,
    view: &'a ViewState,
    rect: Rect,
}

impl Here<'_> {
    /// Where a pointer is on this screen (logical px), and whether it is a 3D one.
    fn screen(&self, tab: ElementId, at: PointerAt) -> Option<(Vec2, bool)> {
        if self.tab != Some(tab) {
            return None;
        }
        let (p, model) = match at {
            PointerAt::Model(p) => {
                if !three_d(self.kind) || self.eda.is_some_and(|e| e.1 != Mode::ThreeD) {
                    return None;
                }
                let p = Vec3::from_array(p);
                (self.rect.center() + self.view.project(p), true)
            }
            PointerAt::Design { of, mode, at } => {
                let (subject, m, view) = self.eda?;
                if eda_of(subject) != of || mode_code(m) != mode {
                    return None;
                }
                let s = view.to_screen([at[0] as f64, at[1] as f64]);
                (self.rect.min + Vec2::new(s[0] as f32, s[1] as f32), false)
            }
            PointerAt::Sheet { sheet, at } => {
                let (id, view, area) = self.sheet?;
                if id != sheet {
                    return None;
                }
                (
                    crate::drawing::sheet_to_screen(view, area, Vec2::from_array(at)),
                    false,
                )
            }
        };
        self.rect.contains(p).then_some((p, model))
    }
}

/// A collaborator's pointer badge: whose, and the label it shows (their name, and what they
/// have open).
#[derive(Component)]
struct PointerBadge {
    id: String,
    label: String,
}

/// The badge's arrow (in 2D) and its dot (in 3D, where the point is on the model).
#[derive(Component)]
struct BadgeArrow;
#[derive(Component)]
struct BadgeDot;

/// The chip with a collaborator's initials at their camera: whose.
#[derive(Component)]
struct CameraChip(String);

/// Shows the badge's arrow in 2D and its dot in 3D.
fn shape_display(model: bool, arrow: bool) -> Display {
    if model != arrow {
        Display::Flex
    } else {
        Display::None
    }
}

fn badge(t: &Theme, id: &str, label: &str, model: bool) -> impl Bundle {
    let color = color_of(id);
    let short: String = id.chars().take(8).collect();
    let arrow = |offset: Vec2, tint: Color| {
        cadrs_ui::icon::icon_in(
            "cursor",
            ARROW,
            tint,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(offset.x),
                top: Val::Px(offset.y),
                ..default()
            },
        )
    };
    (
        Name::new(format!("collab-pointer-{short}")),
        PointerBadge {
            id: id.to_string(),
            label: label.to_string(),
        },
        Node {
            position_type: PositionType::Absolute,
            width: Val::Px(0.0),
            height: Val::Px(0.0),
            ..default()
        },
        GlobalZIndex(5),
        Pickable::IGNORE,
        children![
            (
                BadgeArrow,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(-ARROW_TIP.x),
                    top: Val::Px(-ARROW_TIP.y),
                    width: Val::Px(ARROW),
                    height: Val::Px(ARROW),
                    display: shape_display(model, true),
                    ..default()
                },
                Pickable::IGNORE,
                // A white edge (the arrow nudged a pixel each way) so it shows on any colour.
                children![
                    arrow(Vec2::new(-1.0, 0.0), Color::WHITE),
                    arrow(Vec2::new(1.0, 0.0), Color::WHITE),
                    arrow(Vec2::new(0.0, -1.0), Color::WHITE),
                    arrow(Vec2::new(0.0, 1.0), Color::WHITE),
                    arrow(Vec2::ZERO, color),
                ],
            ),
            (
                BadgeDot,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(-DOT / 2.0),
                    top: Val::Px(-DOT / 2.0),
                    width: Val::Px(DOT),
                    height: Val::Px(DOT),
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::MAX,
                    display: shape_display(model, false),
                    ..default()
                },
                BackgroundColor(color),
                BorderColor::all(Color::WHITE),
                Pickable::IGNORE,
            ),
            (
                Name::new(format!("collab-pointer-{short}-name")),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(ARROW - ARROW_TIP.x - 2.0),
                    top: Val::Px(ARROW - ARROW_TIP.y - 2.0),
                    padding: UiRect::axes(Val::Px(6.0), Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(t.radius)),
                    ..default()
                },
                BackgroundColor(color),
                Pickable::IGNORE,
                children![(
                    t.text(
                        label.to_string(),
                        t.font_sm,
                        FontWeight::MEDIUM,
                        Color::WHITE
                    ),
                    Pickable::IGNORE,
                )],
            ),
        ],
    )
}

fn camera_chip(t: &Theme, id: &str, name: &str) -> impl Bundle {
    let short: String = id.chars().take(8).collect();
    (
        Name::new(format!("collab-camera-{short}")),
        CameraChip(id.to_string()),
        Node {
            position_type: PositionType::Absolute,
            width: Val::Px(CHIP),
            height: Val::Px(CHIP),
            margin: UiRect::new(
                Val::Px(-CHIP / 2.0),
                Val::ZERO,
                Val::Px(-CHIP / 2.0),
                Val::ZERO,
            ),
            border: UiRect::all(Val::Px(1.5)),
            border_radius: BorderRadius::MAX,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(color_of(id)),
        BorderColor::all(Color::WHITE),
        GlobalZIndex(5),
        Pickable::IGNORE,
        children![(
            t.text(
                cadrs_ui::Avatar::initials(name),
                t.font_xs,
                FontWeight::BOLD,
                Color::WHITE
            ),
            Pickable::IGNORE,
        )],
    )
}

/// Someone's name, and what they have open (`Alex · Extrude 1`), once their presence says it
/// (a pointer can come first: the start of their cadrs ID until then).
fn label_of(people: &People, doc: Option<&ActiveDocument>, id: &str) -> String {
    let Some(p) = people.by_id.get(id) else {
        return id.chars().take(8).collect();
    };
    match doc.and_then(|d| super::presence::editing_name(d, &p.presence)) {
        Some(f) => format!("{} · {f}", p.presence.name),
        None => p.presence.name.clone(),
    }
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn sync_badges(
    pointers: Res<Pointers>,
    people: Res<People>,
    (doc, kind, eda, eda_ui, drawing): (
        Option<Res<ActiveDocument>>,
        Res<ActiveKind>,
        Res<Eda2d>,
        Res<EdaUi>,
        Option<Res<crate::drawing::DrawingUi>>,
    ),
    (view, rect, cache, following, time): (
        Res<ViewportView>,
        Res<ViewportRect>,
        Res<crate::parts::PartCache>,
        Option<Res<super::presence::Following>>,
        Res<Time<Real>>,
    ),
    area: Query<Entity, With<ViewportArea>>,
    mut badges: Query<(Entity, &PointerBadge, &mut Node, &Children)>,
    mut shapes: Query<
        (&mut Node, Has<BadgeArrow>),
        (
            Or<(With<BadgeArrow>, With<BadgeDot>)>,
            Without<PointerBadge>,
        ),
    >,
    mut chips: Query<
        (Entity, &CameraChip, &mut Node),
        (
            Without<PointerBadge>,
            Without<BadgeArrow>,
            Without<BadgeDot>,
        ),
    >,
    theme: Res<Theme>,
    mut commands: Commands,
) {
    let t = time.elapsed_secs_f64();
    let here = here_of(
        doc.as_deref(),
        *kind,
        &eda,
        &eda_ui,
        drawing.as_deref(),
        &view,
        &rect,
    );
    let Ok(area) = area.single() else {
        return;
    };

    // The pointers.
    let mut want: BTreeMap<&str, (Vec2, bool)> = BTreeMap::new();
    for (id, r) in &pointers.by_id {
        if !r.idle(t)
            && let Some(at) = r.shown
            && let Some(s) = here.screen(r.tab, at)
        {
            want.insert(id.as_str(), s);
        }
    }
    for (e, b, mut node, children) in &mut badges {
        let Some(&(p, model)) = want.get(b.id.as_str()) else {
            commands.entity(e).despawn();
            continue;
        };
        if b.label != label_of(&people, doc.as_deref(), &b.id) {
            // Made again below, with the new label.
            commands.entity(e).despawn();
            continue;
        }
        let local = p - rect.0.min;
        node.left = Val::Px(local.x);
        node.top = Val::Px(local.y);
        for c in children.iter() {
            if let Ok((mut n, arrow)) = shapes.get_mut(c) {
                let display = shape_display(model, arrow);
                if n.display != display {
                    n.display = display;
                }
            }
        }
        want.remove(b.id.as_str());
    }
    for (id, (p, model)) in want {
        let label = label_of(&people, doc.as_deref(), id);
        let local = p - rect.0.min;
        let e = commands.spawn(badge(&theme, id, &label, model)).id();
        commands.entity(e).entry::<Node>().and_modify(move |mut n| {
            n.left = Val::Px(local.x);
            n.top = Val::Px(local.y);
        });
        commands.entity(area).add_child(e);
    }

    // The chips at their cameras (in 3D, not whoever this user follows).
    let mut cams: BTreeMap<&str, Vec2> = BTreeMap::new();
    if here.tab.is_some() && three_d(here.kind) && here.eda.is_none_or(|e| e.1 == Mode::ThreeD) {
        for (id, person) in &people.by_id {
            if person.presence.tab != here.tab
                || following.as_ref().is_some_and(|f| f.leader == *id)
            {
                continue;
            }
            let Some(theirs) = person
                .presence
                .view
                .as_ref()
                .map(super::presence::view_state)
            else {
                continue;
            };
            let p = rect.0.center() + view.view.project(eye_of(&theirs, &cache));
            if rect.0.contains(p) {
                cams.insert(id.as_str(), p);
            }
        }
    }
    for (e, chip, mut node) in &mut chips {
        match cams.remove(chip.0.as_str()) {
            Some(p) => {
                let local = p - rect.0.min;
                node.left = Val::Px(local.x);
                node.top = Val::Px(local.y);
            }
            None => commands.entity(e).despawn(),
        }
    }
    for (id, p) in cams {
        let name = people
            .by_id
            .get(id)
            .map(|p| p.presence.name.clone())
            .unwrap_or_default();
        let local = p - rect.0.min;
        let e = commands.spawn(camera_chip(&theme, id, &name)).id();
        commands.entity(e).entry::<Node>().and_modify(move |mut n| {
            n.left = Val::Px(local.x);
            n.top = Val::Px(local.y);
        });
        commands.entity(area).add_child(e);
    }
}

fn here_of<'a>(
    doc: Option<&ActiveDocument>,
    kind: ActiveKind,
    eda: &Eda2d,
    eda_ui: &EdaUi,
    drawing: Option<&crate::drawing::DrawingUi>,
    view: &'a ViewportView,
    rect: &ViewportRect,
) -> Here<'a> {
    let tab = doc.and_then(|d| d.active);
    let eda_view = eda.0.and_then(|(el, subject, mode)| {
        let v = eda_ui.views.get(&(el, subject, mode))?.0;
        Some((subject, mode, v))
    });
    let sheet = (kind == ActiveKind::Drawing)
        .then(|| doc.zip(drawing))
        .flatten()
        .and_then(|(doc, ui)| {
            let ((_, id), v) = crate::drawing::current_view(doc, ui)?;
            Some((id.0.as_u128(), v, crate::drawing::sheet_area(rect, ui)))
        });
    Here {
        tab,
        kind,
        eda: eda_view,
        sheet,
        view: &view.view,
        rect: rect.0,
    }
}

/// In 3D: each collaborator's camera and the line from it to their pointer, strong where the
/// parts don't hide them and faint where they do.
#[allow(clippy::too_many_arguments)]
fn draw_gizmos(
    pointers: Res<Pointers>,
    people: Res<People>,
    (doc, kind, eda): (Option<Res<ActiveDocument>>, Res<ActiveKind>, Res<Eda2d>),
    view: Res<ViewportView>,
    cache: Res<crate::parts::PartCache>,
    following: Option<Res<super::presence::Following>>,
    time: Res<Time<Real>>,
    mut shown: Gizmos<CollabGizmos>,
    mut hidden: Gizmos<CollabHiddenGizmos>,
) {
    let tab = doc.as_deref().and_then(|d| d.active);
    if tab.is_none() || !three_d(*kind) || eda.0.is_some_and(|e| e.2 != Mode::ThreeD) {
        return;
    }
    let t = time.elapsed_secs_f64();
    let mine = &view.view;
    let px = |p: Vec3| mine.scale / mine.magnification(p);
    for (id, person) in &people.by_id {
        // Their camera, while they look at this tab in 3D (not whoever this user follows:
        // that camera is this view).
        if person.presence.tab != tab || following.as_ref().is_some_and(|f| f.leader == *id) {
            continue;
        }
        let Some(theirs) = person
            .presence
            .view
            .as_ref()
            .map(super::presence::view_state)
        else {
            continue;
        };
        let color = color_of(id);
        let faint = color.with_alpha(0.3);
        let eye = eye_of(&theirs, &cache);
        let pointer = pointers
            .by_id
            .get(id)
            .filter(|r| Some(r.tab) == tab && !r.idle(t))
            .and_then(|r| match r.shown {
                Some(PointerAt::Model(p)) => Some(Vec3::from_array(p)),
                _ => None,
            });
        let size = CAMERA_PX * px(eye);
        draw_camera(&mut shown, &theirs, eye, size, color);
        draw_camera(&mut hidden, &theirs, eye, size, faint);
        if let Some(p) = pointer {
            shown.line(eye, p, color.with_alpha(0.6));
            hidden.line(eye, p, color.with_alpha(0.15));
        }
    }
}

/// Where to draw someone's camera: their eye in perspective; in orthographic (no eye), on their
/// line of sight through the focus, just outside the shown parts, so it says where they look
/// from.
fn eye_of(v: &ViewState, cache: &crate::parts::PartCache) -> Vec3 {
    if v.perspective {
        return v.camera_position();
    }
    let reach = cache
        .shown()
        .filter_map(|p| p.solid.pick_index().bounds)
        .flat_map(|(lo, hi)| {
            let (lo, hi) = (
                Vec3::new(lo[0] as f32, lo[1] as f32, lo[2] as f32),
                Vec3::new(hi[0] as f32, hi[1] as f32, hi[2] as f32),
            );
            [lo, hi]
        })
        .map(|c| c.distance(v.focus))
        .fold(0.0_f32, f32::max);
    // About what they see across, without parts.
    let reach = if reach > 0.0 {
        reach * 1.3
    } else {
        v.scale * 300.0
    };
    v.focus + v.back() * reach
}

/// A camera: a pyramid from the eye along its line of sight, with a mark on its top edge.
fn draw_camera<G: GizmoConfigGroup>(
    g: &mut Gizmos<G>,
    v: &ViewState,
    eye: Vec3,
    size: f32,
    color: Color,
) {
    let (f, r, u) = (-v.back(), v.right(), v.up());
    let base = eye + f * size;
    let (w, h) = (size * 0.55, size * 0.38);
    let c = [
        base + r * w + u * h,
        base - r * w + u * h,
        base - r * w - u * h,
        base + r * w - u * h,
    ];
    for i in 0..4 {
        g.line(eye, c[i], color);
        g.line(c[i], c[(i + 1) % 4], color);
    }
    let top = base + u * (h * 1.7);
    g.line(c[0], top, color);
    g.line(c[1], top, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_slides_only_within_one_view() {
        let m = |x| Some(PointerAt::Model([x, 0.0, 0.0]));
        let d = |b, mode| {
            Some(PointerAt::Design {
                of: EdaOf::Board(b),
                mode,
                at: [0, 0],
            })
        };
        assert!(same_place(m(0.0), m(5.0)));
        assert!(same_place(d(1, 0), d(1, 0)));
        assert!(!same_place(d(1, 0), d(1, 1)));
        assert!(!same_place(d(1, 0), d(2, 0)));
        assert!(!same_place(m(0.0), d(1, 0)));
        assert!(!same_place(None, m(0.0)));
    }

    #[test]
    fn every_mode_has_its_own_code() {
        let modes = [
            Mode::Schematic,
            Mode::Layout,
            Mode::ThreeD,
            Mode::Symbol,
            Mode::Footprint,
        ];
        let codes: std::collections::HashSet<u8> = modes.iter().map(|m| mode_code(*m)).collect();
        assert_eq!(codes.len(), modes.len());
    }
}
