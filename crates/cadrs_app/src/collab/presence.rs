//! Social cues and Follow mode, as Onshape's (`collaboration.htm`, `follow_mode.htm`).
//!
//! - Everyone in a shared document's room tells the others where they are ([`Presence`]): the
//!   tab, the feature whose dialog or sketch they have open, their 3D view and whom they
//!   follow; a few times a second while it changes, and every few seconds regardless.
//! - Each collaborator has a colour of their own ([`color_of`]). Their **avatars** sit in the
//!   top bar beside **Share**; a tab someone is in carries a dot in their colour (several for
//!   several people), and so does the feature someone is editing in the feature list.
//! - **Follow mode**: double-clicking an avatar follows that person: their tab and their view.
//!   The graphics area is outlined in their colour; they see the follower's avatar outlined. A
//!   click, a scroll or a key stops it, as does their leaving.
//! - With voice (the `voice` feature, `super::voice`): **Join voice** beside the avatars, then
//!   mute and leave; an avatar is outlined green while that person speaks.

use std::collections::BTreeMap;

use bevy::ecs::system::SystemState;
use bevy::input::keyboard::KeyboardInput;
use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy::text::FontWeight;
use cadrs_core::{DocumentId, ElementId, FeatureId};
use cadrs_net::bevy_iroh::Iroh;
use cadrs_net::bevy_iroh::iroh::EndpointId;
use cadrs_net::bevy_iroh::prelude::{NetSender, PeerLeft, Received};
use cadrs_net::{Presence, View};
use cadrs_ui::prelude::*;
use cadrs_ui::{Avatar, DoubleClick, DoubleClickable, IconButton};

use super::live::Live;
use crate::camera::{RenderMode, ViewState};
use crate::document::{FeatureRow, TabButton};
use crate::viewport::{ViewportArea, ViewportView};
use crate::{ActiveDocument, UserProfile};

/// How often presence goes out at most while it changes, and at least while it doesn't.
const SEND_EVERY: f64 = 0.1;
const HEARTBEAT: f64 = 4.0;
/// Someone not heard from this long is gone.
const FORGET_AFTER: f64 = 15.0;

/// Collaborator colours, as Onshape's social cues: strong enough to tell apart at a dot, and
/// none the app's own (its teal accent, the selection's orange), so a person never reads as a
/// control or a selection.
const PALETTE: [&str; 8] = [
    "#2f80ed", "#e53935", "#2e7d32", "#8e24aa", "#d81b60", "#3949ab", "#6d4c41", "#546e7a",
];

pub(super) fn plugin(app: &mut App) {
    app.add_message::<Received<Presence>>()
        .add_message::<PeerLeft>()
        .init_resource::<People>()
        .init_resource::<VoiceUi>()
        .add_systems(
            Update,
            (hear, tell, follow).chain().run_if(resource_exists::<Iroh>),
        )
        .add_systems(
            Update,
            (
                sync_people_bar,
                sync_markers,
                sync_follow_outline,
                colour_own_avatar,
            )
                .after(follow),
        )
        .add_observer(on_avatar_double_click)
        .add_observer(on_voice_button)
        .add_observer(stop_on_press);
}

/// A press anywhere (on any picked entity: the graphics area, a panel, a button) stops
/// following, once the double-click that started it is over.
fn stop_on_press(
    _press: On<Pointer<Press>>,
    following: Option<Res<Following>>,
    time: Res<Time<Real>>,
    mut commands: Commands,
) {
    if following.is_some_and(|f| time.elapsed_secs_f64() - f.since > 0.3) {
        commands.remove_resource::<Following>();
    }
}

/// A collaborator's colour (a cadrs ID's).
pub fn color_of(id: &str) -> Color {
    let hash = id
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    Color::Srgba(Srgba::hex(PALETTE[hash as usize % PALETTE.len()]).unwrap_or(Srgba::BLUE))
}

/// Someone else in the shared document open now.
#[derive(Debug, Clone)]
pub struct Person {
    pub id: EndpointId,
    pub presence: Presence,
    /// When they were last heard (seconds on the real clock).
    heard: f64,
}

impl Person {
    pub fn key(&self) -> String {
        self.id.to_string()
    }
}

/// The others in the shared document open now, by cadrs ID.
#[derive(Resource, Default)]
pub struct People {
    document: Option<DocumentId>,
    pub by_id: BTreeMap<String, Person>,
    /// When this document's room was first heard from: who is there by then was already
    /// there ("is here"), anyone after it joined.
    since: f64,
}

/// How long after opening a shared document the people heard were already there.
const ALREADY_THERE: f64 = 3.0;

/// Voice in the top bar (`super::voice` fills it when voice is built in): whether it is,
/// whether this user joined and is muted, who is speaking (cadrs IDs), and what the buttons
/// asked for.
#[derive(Resource, Default, Clone, PartialEq)]
pub struct VoiceUi {
    pub available: bool,
    pub joined: bool,
    pub muted: bool,
    pub speaking: Vec<String>,
    /// Join (true) or leave (false), asked by a button.
    pub want_join: Option<bool>,
    /// Mute or unmute, asked by a button.
    pub toggle_mute: bool,
}

fn on_voice_button(a: On<bevy::ui_widgets::Activate>, q: Query<&Name>, mut ui: ResMut<VoiceUi>) {
    match q.get(a.entity).map(Name::as_str) {
        Ok("collab-voice-join") => ui.want_join = Some(true),
        Ok("collab-voice-leave") => ui.want_join = Some(false),
        Ok("collab-voice-mute") => ui.toggle_mute = true,
        _ => {}
    }
}

/// Following someone: their cadrs ID, and since when (the click that started it doesn't stop
/// it).
#[derive(Resource, Debug, Clone)]
pub struct Following {
    pub leader: String,
    since: f64,
}

fn now(world: &World) -> f64 {
    world.resource::<Time<Real>>().elapsed_secs_f64()
}

fn hear(
    mut rx: MessageReader<Received<Presence>>,
    mut left: MessageReader<PeerLeft>,
    live: Option<Res<Live>>,
    mut people: ResMut<People>,
    time: Res<Time<Real>>,
    theme: Res<Theme>,
    mut commands: Commands,
) {
    let t = time.elapsed_secs_f64();
    let Some(live) = live else {
        if people.document.is_some() || !people.by_id.is_empty() {
            *people = People::default();
        }
        rx.clear();
        left.clear();
        return;
    };
    if people.document != Some(live.document) {
        *people = People {
            document: Some(live.document),
            by_id: BTreeMap::new(),
            since: t,
        };
    }
    let mut toasts = Vec::new();
    for m in rx.read() {
        if m.msg.document != live.document {
            continue;
        }
        if !people.by_id.contains_key(&m.from.to_string()) {
            let how = if t - people.since < ALREADY_THERE {
                "is here"
            } else {
                "joined"
            };
            toasts.push(format!("{} {how}", m.msg.name));
        }
        people.by_id.insert(
            m.from.to_string(),
            Person {
                id: m.from,
                presence: m.msg.clone(),
                heard: t,
            },
        );
    }
    for l in left.read() {
        if l.room == live.room()
            && let Some(p) = people.by_id.remove(&l.id.to_string())
        {
            toasts.push(format!("{} left", p.presence.name));
        }
    }
    let stale: Vec<String> = people
        .by_id
        .iter()
        .filter(|(_, p)| t - p.heard > FORGET_AFTER)
        .map(|(k, _)| k.clone())
        .collect();
    for k in stale {
        if let Some(p) = people.by_id.remove(&k) {
            toasts.push(format!("{} left", p.presence.name));
        }
    }
    for text in toasts {
        cadrs_ui::show_toast(&mut commands, &theme, text);
    }
}

/// The feature whose dialog or sketch is open.
fn editing_feature(world: &World) -> Option<(ElementId, FeatureId)> {
    if let Some(s) = world.get_resource::<crate::sketch::SketchSession>() {
        return Some((s.element, s.feature));
    }
    if let Some(s) = world.get_resource::<crate::extrude::ExtrudeSession>() {
        return Some((s.element, s.feature));
    }
    if let Some(s) = world.get_resource::<crate::applied::AppliedSession>() {
        return Some((s.element, s.feature));
    }
    if let Some(s) = world.get_resource::<crate::boolean::BooleanSession>() {
        return Some((s.element, s.feature));
    }
    None
}

fn view_of(v: &ViewState) -> View {
    View {
        azimuth: v.azimuth,
        elevation: v.elevation,
        roll: v.roll,
        focus: v.focus.to_array(),
        scale: v.scale,
        perspective: v.perspective,
        render: RenderMode::ALL
            .iter()
            .position(|r| *r == v.render)
            .unwrap_or(0) as u8,
    }
}

pub(super) fn view_state(v: &View) -> ViewState {
    ViewState {
        azimuth: v.azimuth,
        elevation: v.elevation,
        roll: v.roll,
        focus: Vec3::from_array(v.focus),
        scale: v.scale,
        perspective: v.perspective,
        render: RenderMode::ALL
            .get(v.render as usize)
            .copied()
            .unwrap_or_default(),
    }
}

/// This user's presence, to the room: when it changes (at most every [`SEND_EVERY`]) and
/// every [`HEARTBEAT`].
fn tell(world: &mut World, mut last: Local<Option<(Presence, f64)>>) {
    let Some((document, room)) = world.get_resource::<Live>().map(|l| (l.document, l.room()))
    else {
        *last = None;
        return;
    };
    let Some(d) = world.get_resource::<ActiveDocument>() else {
        return;
    };
    let tab = d.active;
    let view = world
        .get_resource::<ViewportView>()
        .filter(|v| v.element == tab)
        .map(|v| view_of(&v.view));
    let p = Presence {
        document,
        name: world.resource::<UserProfile>().display_name.clone(),
        tab,
        editing: editing_feature(world),
        view,
        following: world.get_resource::<Following>().map(|f| f.leader.clone()),
    };
    let t = now(world);
    let due = match &*last {
        None => true,
        Some((prev, at)) => (*prev != p && t - at >= SEND_EVERY) || t - at >= HEARTBEAT,
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
// Follow mode

/// Starts following `id`, or stops if already following them.
pub fn toggle_follow(world: &mut World, id: String) {
    let same = world
        .get_resource::<Following>()
        .is_some_and(|f| f.leader == id);
    if same {
        world.remove_resource::<Following>();
        return;
    }
    let since = now(world);
    world.insert_resource(Following { leader: id, since });
}

/// Follows whoever's cadrs ID starts with `prefix` (the `follow` script command).
pub fn follow_prefix(world: &mut World, prefix: &str) {
    let id = world
        .resource::<People>()
        .by_id
        .keys()
        .find(|k| k.starts_with(prefix))
        .cloned();
    match id {
        Some(id) => toggle_follow(world, id),
        None => warn!("follow: nobody here whose cadrs ID starts with {prefix}"),
    }
}

/// Keeps the leader's tab and view; any click, scroll or key of the follower's own stops it.
fn follow(world: &mut World) {
    let Some(f) = world.get_resource::<Following>().cloned() else {
        return;
    };
    let t = now(world);
    let leader = world
        .resource::<People>()
        .by_id
        .get(&f.leader)
        .map(|p| p.presence.clone());
    let Some(leader) = leader else {
        world.remove_resource::<Following>();
        crate::linked::toast(world, "Stopped following: they left");
        return;
    };
    if t - f.since > 0.3 {
        let pressed = world
            .resource::<ButtonInput<MouseButton>>()
            .get_just_pressed()
            .next()
            .is_some();
        let scrolled = !world.resource::<Messages<MouseWheel>>().is_empty();
        let typed = !world.resource::<Messages<KeyboardInput>>().is_empty();
        if pressed || scrolled || typed {
            world.remove_resource::<Following>();
            return;
        }
    }
    if let Some(tab) = leader.tab {
        let mut d = world.resource_mut::<ActiveDocument>();
        if d.active != Some(tab) && d.doc.element(tab).is_some() {
            d.set_active(tab);
        }
    }
    if let Some(v) = &leader.view {
        let mut vv = world.resource_mut::<ViewportView>();
        if vv.element == leader.tab {
            let to = view_state(v);
            if vv.view != to {
                vv.view = to;
                vv.animation = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The cues

/// The avatars' place in the top bar (`crate::document`).
#[derive(Component)]
pub struct PeopleBar;

/// An avatar: whose.
#[derive(Component)]
struct PersonAvatar(String);

/// What the avatars show, to rebuild them only when it changes: each person (id, name, where
/// they are, follows me, I follow them, speaking), the status, and voice (built in, joined,
/// muted).
#[derive(PartialEq, Clone, Default)]
struct BarState(
    Vec<(String, String, String, bool, bool, bool)>,
    Option<String>,
    (bool, bool, bool),
);

fn describe(doc: &ActiveDocument, p: &Presence) -> String {
    let mut text = p.name.clone();
    if let Some(e) = p.tab.and_then(|t| doc.doc.element(t)) {
        text.push_str(&format!(" · in {}", e.name));
    }
    if let Some(f) = editing_name(doc, p) {
        text.push_str(&format!(", editing {f}"));
    }
    text
}

/// The name of the feature someone has open (its dialog or sketch).
pub(super) fn editing_name(doc: &ActiveDocument, p: &Presence) -> Option<String> {
    let (e, f) = p.editing?;
    let f = doc.doc.element(e)?.features().iter().find(|x| x.id == f)?;
    Some(f.name.clone())
}

/// This user's own avatar in the top bar, while online: in the colour the others see them in
/// (their cadrs ID's), not the one picked from their name.
fn colour_own_avatar(
    iroh: Option<Res<Iroh>>,
    mut avatars: Query<(&Name, &mut BackgroundColor)>,
    added: Query<(), Added<Name>>,
    mut last: Local<Option<String>>,
    profile: Res<crate::UserProfile>,
) {
    let me = iroh.map(|i| i.id().to_string());
    if *last == me && added.is_empty() {
        return;
    }
    let want = me.as_deref().map_or_else(
        || cadrs_ui::Avatar::color_for(&profile.display_name),
        color_of,
    );
    for (name, mut bg) in &mut avatars {
        if name.as_str() == "identity-avatar" && bg.0 != want {
            bg.0 = want;
        }
    }
    *last = me;
}

#[allow(clippy::too_many_arguments)]
fn sync_people_bar(
    people: Res<People>,
    following: Option<Res<Following>>,
    voice: Res<VoiceUi>,
    doc: Option<Res<ActiveDocument>>,
    live: Option<Res<Live>>,
    iroh: Option<Res<Iroh>>,
    q_bar: Query<Entity, With<PeopleBar>>,
    theme: Res<Theme>,
    mut last: Local<BarState>,
    mut commands: Commands,
) {
    let Ok(bar) = q_bar.single() else {
        *last = BarState::default();
        return;
    };
    let Some(doc) = doc else { return };
    let me = iroh.map(|i| i.id().to_string()).unwrap_or_default();
    let leader = following.as_ref().map(|f| f.leader.clone());
    let rows: Vec<_> = people
        .by_id
        .values()
        .map(|p| {
            let key = p.key();
            let follows_me = p.presence.following.as_deref() == Some(me.as_str());
            let i_follow = leader.as_deref() == Some(key.as_str());
            let speaking = voice.speaking.contains(&key);
            (
                key,
                p.presence.name.clone(),
                describe(&doc, &p.presence),
                follows_me,
                i_follow,
                speaking,
            )
        })
        .collect();
    let status = live.as_ref().and_then(|l| l.status());
    let voice_state = (voice.available && live.is_some(), voice.joined, voice.muted);
    let state = BarState(rows, status, voice_state);
    if *last == state {
        return;
    }
    *last = state.clone();
    commands.entity(bar).despawn_children();
    let t = &*theme;
    commands.entity(bar).with_children(|b| {
        if let Some(status) = &state.1 {
            b.spawn((
                Name::new("collab-status"),
                t.text(status, t.font_sm, FontWeight::MEDIUM, t.muted_foreground),
                Node {
                    margin: UiRect::right(Val::Px(6.0)),
                    ..default()
                },
            ));
        }
        // Voice: join; once joined, mute and leave.
        let (available, joined, muted) = state.2;
        if available && !joined {
            b.spawn(
                IconButton::new("collab-voice-join", "volume-mute")
                    .small()
                    .tooltip("Join voice: talk with everyone in this document")
                    .build(t),
            );
        } else if available {
            b.spawn(
                IconButton::new(
                    "collab-voice-mute",
                    if muted { "volume-mute" } else { "volume" },
                )
                .small()
                .tooltip(if muted { "Unmute" } else { "Mute" })
                .build(t),
            );
            b.spawn(
                IconButton::new("collab-voice-leave", "close")
                    .small()
                    .tooltip("Leave voice")
                    .build(t),
            )
            .entry::<Node>()
            .and_modify(|mut n| n.margin = UiRect::right(Val::Px(4.0)));
        }
        for (key, name, tip, follows_me, i_follow, speaking) in &state.0 {
            let color = color_of(key);
            let outlined = *follows_me || *i_follow;
            let tip = if *i_follow {
                format!("{tip}. Following: click anywhere to stop")
            } else {
                format!("{tip}. Double-click to follow")
            };
            b.spawn((
                Name::new(format!("collab-avatar-{}", &key[..8.min(key.len())])),
                PersonAvatar(key.clone()),
                DoubleClickable,
                Node {
                    padding: UiRect::all(Val::Px(1.0)),
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(t.radius + 2.0)),
                    ..default()
                },
                BorderColor::all(if *speaking {
                    t.success
                } else if outlined {
                    color
                } else {
                    Color::NONE
                }),
                Tooltip::new(tip),
            ))
            .with_children(|a| {
                a.spawn((
                    Avatar::new(format!("collab-avatar-face-{key}"), name.clone())
                        .size(22.0)
                        .color(color)
                        .build(t),
                    Pickable::IGNORE,
                ));
            });
        }
    });
}

fn on_avatar_double_click(ev: On<DoubleClick>, q: Query<&PersonAvatar>, mut commands: Commands) {
    if let Ok(a) = q.get(ev.entity) {
        let id = a.0.clone();
        commands.queue(move |w: &mut World| toggle_follow(w, id));
    }
}

/// The dots on tabs and feature rows: whose colours they show.
#[derive(Component, PartialEq)]
struct Marker(Vec<String>);

fn marker(colors: Vec<String>) -> impl Bundle {
    (
        Marker(colors.clone()),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(3.0),
            right: Val::Px(4.0),
            column_gap: Val::Px(2.0),
            ..default()
        },
        Pickable::IGNORE,
        Children::spawn(SpawnIter(colors.into_iter().map(|id| {
            (
                Node {
                    width: Val::Px(7.0),
                    height: Val::Px(7.0),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(color_of(&id)),
                Pickable::IGNORE,
            )
        }))),
    )
}

/// Puts each collaborator's dot on the tab they are in and the feature they edit.
fn sync_markers(
    people: Res<People>,
    tabs: Query<(Entity, &TabButton)>,
    rows: Query<(Entity, &FeatureRow)>,
    markers: Query<(Entity, &Marker, &ChildOf)>,
    mut commands: Commands,
) {
    let mut want: BTreeMap<Entity, Vec<String>> = BTreeMap::new();
    for p in people.by_id.values() {
        if let Some(tab) = p.presence.tab
            && let Some((e, _)) = tabs.iter().find(|(_, b)| b.0 == tab)
        {
            want.entry(e).or_default().push(p.key());
        }
        if let Some((_, f)) = p.presence.editing
            && let Some((e, _)) = rows.iter().find(|(_, r)| r.0 == f)
        {
            want.entry(e).or_default().push(p.key());
        }
    }
    let mut have: BTreeMap<Entity, Entity> = BTreeMap::new();
    for (m, Marker(ids), parent) in &markers {
        match want.get(&parent.parent()) {
            Some(w) if w == ids => {
                have.insert(parent.parent(), m);
            }
            _ => commands.entity(m).despawn(),
        }
    }
    for (e, ids) in want {
        if !have.contains_key(&e) {
            commands.entity(e).with_child(marker(ids));
        }
    }
}

/// The follower's graphics area, outlined in the leader's colour.
#[derive(Component)]
struct FollowOutline(String);

fn sync_follow_outline(
    following: Option<Res<Following>>,
    people: Res<People>,
    area: Query<Entity, With<ViewportArea>>,
    outlines: Query<(Entity, &FollowOutline)>,
    theme: Res<Theme>,
    mut commands: Commands,
) {
    let want = following.as_ref().and_then(|f| {
        let name = people.by_id.get(&f.leader)?.presence.name.clone();
        Some((f.leader.clone(), name))
    });
    let current = outlines.iter().next();
    if current.map(|(_, o)| o.0.clone()) == want.as_ref().map(|w| w.0.clone()) {
        return;
    }
    if let Some((e, _)) = current {
        commands.entity(e).despawn();
    }
    let (Some((id, name)), Ok(area)) = (want, area.single()) else {
        return;
    };
    let color = color_of(&id);
    let t = &*theme;
    commands.entity(area).with_child((
        Name::new("follow-outline"),
        FollowOutline(id),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            top: Val::Px(0.0),
            bottom: Val::Px(0.0),
            border: UiRect::all(Val::Px(3.0)),
            justify_content: JustifyContent::Center,
            ..default()
        },
        BorderColor::all(color),
        Pickable::IGNORE,
        children![(
            Node {
                margin: UiRect::top(Val::Px(6.0)),
                padding: UiRect::axes(Val::Px(10.0), Val::Px(3.0)),
                border_radius: BorderRadius::all(Val::Px(t.radius)),
                height: Val::Px(22.0),
                ..default()
            },
            BackgroundColor(color),
            Pickable::IGNORE,
            children![(
                t.text(
                    format!("Following {name}: click anywhere to stop"),
                    t.font_sm,
                    FontWeight::MEDIUM,
                    Color::WHITE
                ),
                Pickable::IGNORE,
            )],
        )],
    ));
}
