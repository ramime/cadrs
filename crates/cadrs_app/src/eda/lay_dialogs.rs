//! The Layout view's dialogs (GS13, GS14, GS18, GS19, GS21): Update PCB from schematic, Board
//! setup, zone properties, DRC, Plot, text, a footprint's and a shape's properties (E).
//!
//! Names: `eda-update-dialog` (`eda-update-replace`, `eda-update-delete`, `eda-update-run`,
//! `eda-update-log`), `eda-setup-dialog` (`eda-setup-copper`, `eda-setup-<rule>`,
//! `eda-setup-track`, `eda-setup-clearance`, `eda-setup-via`, `eda-setup-via-drill`,
//! `eda-setup-ok`), `eda-zone-dialog` (`eda-zone-net`, `eda-zone-layer`, `eda-zone-ok`),
//! `eda-drc-dialog` (`eda-drc-refill`, `eda-drc-run`, `eda-drc-summary`, `eda-drc-list`),
//! `eda-plot-dialog` (`eda-plot-dir`, `eda-plot-run`, `eda-plot-drill`, `eda-plot-log`),
//! `eda-btext-dialog` (`eda-btext-text`, `-height`, `-thickness`, `-align`, `-ok`),
//! `eda-fprops-dialog` (`eda-fprops-x`, `-y`, `-angle`, `-side`, `-model`, `-ok`),
//! `eda-sprops-dialog` (`eda-sprops-width`, `-filled`, `-net`, `-ok`).

use bevy::prelude::*;
use bevy::text::FontWeight;
use bevy::ui_widgets::{Activate, observe};
use cadrs_eda::expr::{Unit, eval};
use cadrs_eda::layer::Layer;
use cadrs_eda::units::{Nm, Pt, mm, to_mm};
use cadrs_ui::checkbox::CheckboxState;
use cadrs_ui::dialog_fields::{Select, SelectState};
use cadrs_ui::prelude::*;
use cadrs_ui::{Checkbox, Dialog};

use super::layout_tools::{LayoutState, Tool, set_tool};
use super::ui;
use crate::AppState;

#[derive(Component)]
pub struct LayoutDialog;

fn close_all(w: &mut World) {
    let mut q = w.query_filtered::<Entity, With<LayoutDialog>>();
    let es: Vec<Entity> = q.iter(w).collect();
    for e in es {
        w.entity_mut(e).despawn();
    }
}

fn spawn_dialog(w: &mut World, d: Dialog) {
    let theme = w.resource::<Theme>().clone();
    w.spawn((
        d.build(&theme),
        LayoutDialog,
        DespawnOnExit(AppState::Document),
    ));
}

fn row(p: &mut ChildSpawner, t: &Theme, label: &str, f: impl FnOnce(&mut ChildSpawner)) {
    p.spawn(Node {
        align_items: AlignItems::Center,
        column_gap: Val::Px(10.0),
        height: Val::Px(32.0),
        ..default()
    })
    .with_children(|r| {
        r.spawn((
            t.text(label, t.font_base, FontWeight::MEDIUM, t.muted_foreground),
            Node {
                width: Val::Px(190.0),
                ..default()
            },
        ));
        f(r);
    });
}

fn mm_row(p: &mut ChildSpawner, t: &Theme, label: &str, name: &'static str, v: Nm) {
    row(p, t, label, |r| {
        r.spawn(
            TextInput::new(name)
                .value(format!("{}", to_mm(v)))
                .width(Val::Px(120.0))
                .height(26.0)
                .build(t),
        );
        r.spawn(t.text("mm", t.font_sm, FontWeight::NORMAL, t.muted_foreground));
    });
}

fn button(
    f: &mut ChildSpawner,
    t: &Theme,
    name: &'static str,
    label: &str,
    primary: bool,
    run: fn(&mut World),
) {
    let mut b = cadrs_ui::Button::new(name).label(label);
    if primary {
        b = b.primary();
    }
    f.spawn((
        b.build(t),
        observe(move |_: On<Activate>, mut commands: Commands| {
            commands.queue(run);
        }),
    ));
}

fn checkbox(w: &mut World, name: &str) -> bool {
    let mut q = w.query::<(&Name, &CheckboxState)>();
    q.iter(w)
        .find(|(n, _)| n.as_str() == name)
        .is_some_and(|(_, s)| s.checked)
}

fn select_index(w: &mut World, name: &str) -> Option<usize> {
    let mut q = w.query::<(&Name, &SelectState)>();
    q.iter(w)
        .find(|(n, _)| n.as_str() == name)
        .map(|(_, s)| s.selected)
}

fn list_lines(w: &mut World, list: &str, lines: Vec<(String, bool)>) {
    let mut q = w.query::<(Entity, &Name)>();
    let Some(e) = q.iter(w).find(|(_, n)| n.as_str() == list).map(|(e, _)| e) else {
        return;
    };
    let t = w.resource::<Theme>().clone();
    let mut commands = w.commands();
    commands.entity(e).despawn_children();
    commands.entity(e).with_children(|l| {
        for (i, (s, bold)) in lines.into_iter().enumerate() {
            l.spawn((
                Name::new(format!("{list}-{i}")),
                t.text(
                    s,
                    t.font_sm,
                    if bold {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    },
                    t.foreground,
                ),
            ));
        }
    });
    w.flush();
}

/// A log or list that scrolls with the wheel when it runs past `h`.
fn list_node(name: &'static str, h: f32) -> impl Bundle {
    (
        Name::new(name),
        cadrs_ui::WheelScroll,
        Node {
            flex_direction: FlexDirection::Column,
            height: Val::Px(h),
            overflow: Overflow::scroll_y(),
            margin: UiRect::top(Val::Px(6.0)),
            ..default()
        },
    )
}

// ---------------------------------------------------------------------------------------------
// Update PCB from schematic (GS14)

pub fn open_update(w: &mut World) {
    let t = w.resource::<Theme>().clone();
    let tf = t.clone();
    spawn_dialog(
        w,
        Dialog::new("eda-update-dialog")
            .title("Update PCB from schematic")
            .width(640.0)
            .body(move |b| {
                b.spawn(
                    Checkbox::new("eda-update-relink")
                        .label("Re-link footprints to symbols by reference")
                        .checked(false)
                        .build(&t),
                );
                b.spawn(
                    Checkbox::new("eda-update-replace")
                        .label("Replace footprints with those the symbols name")
                        .checked(true)
                        .build(&t),
                );
                b.spawn(
                    Checkbox::new("eda-update-delete")
                        .label("Delete footprints with no symbol")
                        .checked(false)
                        .build(&t),
                );
                b.spawn(
                    Checkbox::new("eda-update-fields")
                        .label("Update footprint fields from symbols")
                        .checked(true)
                        .build(&t),
                );
                b.spawn(list_node("eda-update-log", 320.0));
            })
            .footer(move |f| {
                button(f, &tf, "eda-update-run", "Update PCB", true, run_update);
                button(f, &tf, "eda-update-close", "Close", false, close_all);
            }),
    );
}

fn run_update(w: &mut World) {
    let lib = ui::libraries(w);
    // New footprints go to the middle of the view.
    let at = {
        let ui_state = w.resource::<super::EdaUi>();
        let key = w.resource::<super::Eda2d>().0;
        key.and_then(|k| ui_state.views.get(&k))
            .map(|v| v.0.center)
            .map_or(cadrs_eda::units::Pt::ZERO, |c| {
                cadrs_eda::units::Pt::new(c[0] as i64, c[1] as i64)
            })
    };
    let opts = cadrs_eda::forward::Options {
        relink_by_reference: checkbox(w, "eda-update-relink"),
        replace_footprints: checkbox(w, "eda-update-replace"),
        delete_unused: checkbox(w, "eda-update-delete"),
        update_fields: checkbox(w, "eda-update-fields"),
        place_at: at,
    };
    let mut report = None;
    ui::commit(w, "Update PCB from schematic", |d| {
        report = Some(cadrs_eda::forward::update_pcb(d, &lib, &opts));
        Ok(())
    });
    if let Some(r) = report {
        let mut lines: Vec<(String, bool)> = r.messages.into_iter().map(|m| (m, false)).collect();
        lines.extend(
            r.warnings
                .into_iter()
                .map(|e| (format!("Warning: {e}"), true)),
        );
        lines.extend(r.errors.into_iter().map(|e| (format!("Error: {e}"), true)));
        list_lines(w, "eda-update-log", lines);
    }
}

// ---------------------------------------------------------------------------------------------
// Board setup (GS13)

const RULE_FIELDS: [(&str, &str); 8] = [
    ("Minimum clearance", "eda-setup-min-clearance"),
    ("Minimum track width", "eda-setup-min-track"),
    ("Minimum annular width", "eda-setup-annular"),
    ("Minimum via diameter", "eda-setup-min-via"),
    ("Copper to hole clearance", "eda-setup-hole-clearance"),
    ("Copper to edge clearance", "eda-setup-edge"),
    ("Minimum drill size", "eda-setup-drill"),
    ("Hole to hole clearance", "eda-setup-hole-hole"),
];

pub fn open_setup(w: &mut World) {
    let Some((_, _, d)) = ui::current(w) else {
        return;
    };
    let b = d.board.clone();
    let t = w.resource::<Theme>().clone();
    let tf = t.clone();
    spawn_dialog(
        w,
        Dialog::new("eda-setup-dialog")
            .title("Board setup")
            .width(560.0)
            .body(move |p| {
                p.spawn(t.text(
                    "Physical stackup",
                    t.font_base,
                    FontWeight::SEMIBOLD,
                    t.foreground,
                ));
                row(p, &t, "Copper layers", |r| {
                    let s = Select::new("eda-setup-copper")
                        .bordered()
                        .width(Val::Px(120.0))
                        .option("2", true)
                        .option("4", true)
                        .option("6", true);
                    r.spawn(
                        s.selected(((b.copper_layers.max(2) / 2) - 1) as usize)
                            .build(&t),
                    );
                });
                mm_row(p, &t, "Board thickness", "eda-setup-thickness", b.thickness);
                p.spawn((
                    t.text(
                        "Constraints",
                        t.font_base,
                        FontWeight::SEMIBOLD,
                        t.foreground,
                    ),
                    Node {
                        margin: UiRect::top(Val::Px(8.0)),
                        ..default()
                    },
                ));
                let r = &b.rules;
                let values = [
                    r.min_clearance,
                    r.min_track_width,
                    r.min_annular_ring,
                    r.min_via_diameter,
                    r.hole_clearance,
                    r.copper_edge_clearance,
                    r.min_drill,
                    r.hole_to_hole,
                ];
                for ((label, name), v) in RULE_FIELDS.iter().zip(values) {
                    mm_row(p, &t, label, name, v);
                }
                let c = r.class_of("").clone();
                p.spawn((
                    t.text(
                        "Net class Default",
                        t.font_base,
                        FontWeight::SEMIBOLD,
                        t.foreground,
                    ),
                    Node {
                        margin: UiRect::top(Val::Px(8.0)),
                        ..default()
                    },
                ));
                mm_row(p, &t, "Clearance", "eda-setup-clearance", c.clearance);
                mm_row(p, &t, "Track width", "eda-setup-track", c.track_width);
                mm_row(p, &t, "Via size", "eda-setup-via", c.via_diameter);
                mm_row(p, &t, "Via hole", "eda-setup-via-drill", c.via_drill);
            })
            .footer(move |f| {
                button(f, &tf, "eda-setup-ok", "OK", true, accept_setup);
                button(f, &tf, "eda-setup-cancel", "Cancel", false, close_all);
            }),
    );
}

fn read_mm(w: &mut World, name: &str) -> Result<Nm, String> {
    let s = ui::text_value(w, name);
    eval(&s, Unit::Mm).map_err(|e| format!("{s}: {e}"))
}

fn accept_setup(w: &mut World) {
    let copper = select_index(w, "eda-setup-copper").map(|i| (i as u8 + 1) * 2);
    let mut vals = vec![];
    for (_, name) in RULE_FIELDS {
        vals.push(read_mm(w, name));
    }
    let thickness = read_mm(w, "eda-setup-thickness");
    let class = [
        "eda-setup-clearance",
        "eda-setup-track",
        "eda-setup-via",
        "eda-setup-via-drill",
    ]
    .map(|n| read_mm(w, n));
    close_all(w);
    ui::commit(w, "Board setup", |d| {
        let b = &mut d.board;
        if let Some(c) = copper {
            b.copper_layers = c;
        }
        b.thickness = thickness?;
        b.stackup = cadrs_eda::board::Stackup::standard(b.copper_layers, b.thickness);
        let r = &mut b.rules;
        let v: Vec<Nm> = vals.into_iter().collect::<Result<_, _>>()?;
        (
            r.min_clearance,
            r.min_track_width,
            r.min_annular_ring,
            r.min_via_diameter,
            r.hole_clearance,
            r.copper_edge_clearance,
            r.min_drill,
            r.hole_to_hole,
        ) = (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]);
        let [cl, tw, vd, vh] = class;
        let c = r.class_mut("Default").ok_or("No Default net class")?;
        (c.clearance, c.track_width, c.via_diameter, c.via_drill) = (cl?, tw?, vd?, vh?);
        Ok(())
    });
}

// ---------------------------------------------------------------------------------------------
// Zone properties (GS18)

#[derive(Resource, Default)]
struct ZoneNets(Vec<String>);

/// A zone's properties (E or a double-click on it): its net (none: it takes the net of what is
/// wired to it) and its layer. One undo step; the zone is poured again.
pub fn open_zone(w: &mut World, id: uuid::Uuid) {
    let Some((_, _, d)) = ui::current(w) else {
        return;
    };
    let Some(z) = d.board.zones.iter().find(|z| z.id == id).cloned() else {
        return;
    };
    let mut nets: Vec<String> = vec![String::new()];
    nets.extend(d.board.nets.iter().filter(|n| !n.is_empty()).cloned());
    let net_i = nets.iter().position(|n| *n == z.net).unwrap_or(0);
    w.insert_resource(ZoneNets(nets.clone()));
    w.insert_resource(PropsOf(cadrs_eda::board_edit::BoardItem::Zone(id)));
    let bottom = z.layers.iter().any(|l| l == Layer::BottomCopper)
        && !z.layers.iter().any(|l| l == Layer::TopCopper);
    let t = w.resource::<Theme>().clone();
    let tf = t.clone();
    spawn_dialog(
        w,
        Dialog::new("eda-zone-dialog")
            .title("Copper zone properties")
            .width(440.0)
            .body(move |p| {
                row(p, &t, "Net", |r| {
                    let mut s = Select::new("eda-zone-net").bordered().width(Val::Px(240.0));
                    for n in &nets {
                        s = s.option(
                            if n.is_empty() {
                                "(from what is wired to it)".to_string()
                            } else {
                                n.clone()
                            },
                            true,
                        );
                    }
                    r.spawn(s.selected(net_i).build(&t));
                });
                row(p, &t, "Layer", |r| {
                    let s = Select::new("eda-zone-layer")
                        .bordered()
                        .width(Val::Px(240.0))
                        .option("Top copper", true)
                        .option("Bottom copper", true);
                    r.spawn(s.selected(usize::from(bottom)).build(&t));
                });
            })
            .footer(move |f| {
                button(f, &tf, "eda-zone-ok", "OK", true, accept_zone);
                button(f, &tf, "eda-zone-cancel", "Cancel", false, close_all);
            }),
    );
}

fn accept_zone(w: &mut World) {
    let Some(PropsOf(cadrs_eda::board_edit::BoardItem::Zone(id))) =
        w.get_resource::<PropsOf>().copied()
    else {
        return;
    };
    let nets = w.resource::<ZoneNets>().0.clone();
    let net = select_index(w, "eda-zone-net")
        .and_then(|i| nets.get(i).cloned())
        .unwrap_or_default();
    let layer = if select_index(w, "eda-zone-layer") == Some(1) {
        Layer::BottomCopper
    } else {
        Layer::TopCopper
    };
    close_all(w);
    ui::commit(w, "Zone properties", |d| {
        let z = d
            .board
            .zones
            .iter_mut()
            .find(|z| z.id == id)
            .ok_or("The zone is gone")?;
        z.net = net.clone();
        z.layers = cadrs_eda::layer::LayerSet::of(&[layer]);
        Ok(())
    });
}

// ---------------------------------------------------------------------------------------------
// Text and keep-outs

/// Text on the drawing layer: its words, height, stroke and alignment, then a click places it.
pub fn open_text(w: &mut World) {
    let layer = w.resource::<LayoutState>().draw_layer;
    let look = w.resource::<LayoutState>().text_style;
    let t = w.resource::<Theme>().clone();
    let tf = t.clone();
    spawn_dialog(
        w,
        Dialog::new("eda-btext-dialog")
            .title("Text")
            .width(440.0)
            .body(move |p| {
                row(p, &t, "Text", |r| {
                    r.spawn(
                        TextInput::new("eda-btext-text")
                            .width(Val::Px(220.0))
                            .height(26.0)
                            .build(&t),
                    );
                });
                mm_row(p, &t, "Height", "eda-btext-height", mm(look.height));
                mm_row(
                    p,
                    &t,
                    "Stroke width",
                    "eda-btext-thickness",
                    mm(look.thickness),
                );
                row(p, &t, "Alignment", |r| {
                    let i = match look.align {
                        cadrs_eda::graphics::HAlign::Left => 0,
                        cadrs_eda::graphics::HAlign::Center => 1,
                        cadrs_eda::graphics::HAlign::Right => 2,
                    };
                    r.spawn(
                        Select::new("eda-btext-align")
                            .bordered()
                            .width(Val::Px(120.0))
                            .option("Left", true)
                            .option("Centre", true)
                            .option("Right", true)
                            .selected(i)
                            .build(&t),
                    );
                });
                p.spawn(t.text(
                    format!(
                        "On {} (pick another layer in the layers panel).",
                        layer.name()
                    ),
                    t.font_sm,
                    FontWeight::NORMAL,
                    t.muted_foreground,
                ));
            })
            .footer(move |f| {
                button(f, &tf, "eda-btext-ok", "OK", true, accept_text);
                button(f, &tf, "eda-btext-cancel", "Cancel", false, close_all);
            }),
    );
}

fn accept_text(w: &mut World) {
    let text = ui::text_value(w, "eda-btext-text").trim().to_string();
    let (h, th) = (
        read_mm(w, "eda-btext-height"),
        read_mm(w, "eda-btext-thickness"),
    );
    let align = match select_index(w, "eda-btext-align") {
        Some(0) => cadrs_eda::graphics::HAlign::Left,
        Some(2) => cadrs_eda::graphics::HAlign::Right,
        _ => cadrs_eda::graphics::HAlign::Center,
    };
    close_all(w);
    let (Ok(h), Ok(th)) = (h, th) else {
        ui::toast(w, "Height and stroke width are lengths in mm");
        return;
    };
    w.resource_mut::<LayoutState>().text_style = super::layout_tools::TextLook {
        height: to_mm(h).max(0.1),
        thickness: to_mm(th).max(0.01),
        align,
    };
    if !text.is_empty() {
        set_tool(w, Tool::Text(text));
    }
}

/// A board text's properties (E or a double-click on it): its words, height and stroke width.
/// One undo step.
pub fn open_board_text(w: &mut World, id: uuid::Uuid) {
    let Some((_, _, d)) = ui::current(w) else {
        return;
    };
    let Some(bt) = d.board.texts.iter().find(|t| t.id == id).cloned() else {
        return;
    };
    w.insert_resource(PropsOf(cadrs_eda::board_edit::BoardItem::Text(id)));
    let t = w.resource::<Theme>().clone();
    let tf = t.clone();
    let thickness = bt.text.style.thickness.unwrap_or(bt.text.style.size.h / 8);
    spawn_dialog(
        w,
        Dialog::new("eda-tprops-dialog")
            .title(format!("Text on {}", bt.layer.name()))
            .width(440.0)
            .body(move |p| {
                row(p, &t, "Text", |r| {
                    r.spawn(
                        TextInput::new("eda-tprops-text")
                            .value(bt.text.text.clone())
                            .width(Val::Px(220.0))
                            .height(26.0)
                            .build(&t),
                    );
                });
                mm_row(p, &t, "Height", "eda-tprops-height", bt.text.style.size.h);
                mm_row(p, &t, "Stroke width", "eda-tprops-thickness", thickness);
                p.spawn(t.text(
                    "Drag the text to move it (Select: press on it once selected).",
                    t.font_sm,
                    FontWeight::NORMAL,
                    t.muted_foreground,
                ));
            })
            .footer(move |f| {
                button(f, &tf, "eda-tprops-ok", "OK", true, accept_board_text);
                button(f, &tf, "eda-tprops-cancel", "Cancel", false, close_all);
            }),
    );
}

fn accept_board_text(w: &mut World) {
    let Some(PropsOf(cadrs_eda::board_edit::BoardItem::Text(id))) =
        w.get_resource::<PropsOf>().copied()
    else {
        return;
    };
    let text = ui::text_value(w, "eda-tprops-text");
    let (h, th) = (
        read_mm(w, "eda-tprops-height"),
        read_mm(w, "eda-tprops-thickness"),
    );
    close_all(w);
    let (Ok(h), Ok(th)) = (h, th) else {
        ui::toast(w, "Height and stroke width are lengths in mm");
        return;
    };
    if text.trim().is_empty() {
        ui::toast(w, "The text is empty (Del removes it)");
        return;
    }
    ui::commit(w, "Text properties", |d| {
        let t = d
            .board
            .texts
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or("The text is gone")?;
        t.text.text = text.clone();
        let h = h.max(mm(0.1));
        // Its width scales with its height.
        let k = h as f64 / t.text.style.size.h.max(1) as f64;
        t.text.style.size = cadrs_eda::units::Size::new((t.text.style.size.w as f64 * k) as Nm, h);
        t.text.style.thickness = Some(th.max(mm(0.01)));
        Ok(())
    });
}

/// A keep-out (rule area): its layers and what it forbids, then its corners (double-click ends).
pub fn open_keepout(w: &mut World) {
    let t = w.resource::<Theme>().clone();
    let tf = t.clone();
    spawn_dialog(
        w,
        Dialog::new("eda-keepout-dialog")
            .title("Keep-out properties")
            .width(440.0)
            .body(move |p| {
                row(p, &t, "Layers", |r| {
                    let s = Select::new("eda-keepout-layers")
                        .bordered()
                        .width(Val::Px(200.0))
                        .option("Top copper", true)
                        .option("Bottom copper", true)
                        .option("Both copper layers", true);
                    r.spawn(s.selected(2).build(&t));
                });
                for (name, label, on) in [
                    ("eda-keepout-tracks", "No tracks", true),
                    ("eda-keepout-vias", "No vias", true),
                    ("eda-keepout-pads", "No pads", false),
                    ("eda-keepout-pour", "No copper pours", true),
                    ("eda-keepout-footprints", "No footprints", false),
                ] {
                    p.spawn(Checkbox::new(name).label(label).checked(on).build(&t));
                }
            })
            .footer(move |f| {
                button(f, &tf, "eda-keepout-ok", "OK", true, accept_keepout);
                button(f, &tf, "eda-keepout-cancel", "Cancel", false, close_all);
            }),
    );
}

fn accept_keepout(w: &mut World) {
    use cadrs_eda::layer::LayerSet;
    let layers = match select_index(w, "eda-keepout-layers") {
        Some(0) => LayerSet::of(&[Layer::TopCopper]),
        Some(1) => LayerSet::of(&[Layer::BottomCopper]),
        _ => LayerSet::of(&[Layer::TopCopper, Layer::BottomCopper]),
    };
    let rules = cadrs_eda::board::Keepout {
        tracks: checkbox(w, "eda-keepout-tracks"),
        vias: checkbox(w, "eda-keepout-vias"),
        pads: checkbox(w, "eda-keepout-pads"),
        copper_pour: checkbox(w, "eda-keepout-pour"),
        footprints: checkbox(w, "eda-keepout-footprints"),
        max_height: None,
    };
    close_all(w);
    set_tool(
        w,
        Tool::Keepout {
            layers,
            rules,
            pts: vec![],
        },
    );
}
// ---------------------------------------------------------------------------------------------
// DRC (GS19)

pub fn open_drc(w: &mut World) {
    let t = w.resource::<Theme>().clone();
    let tf = t.clone();
    spawn_dialog(
        w,
        Dialog::new("eda-drc-dialog")
            .title("Design rules checker")
            .width(680.0)
            .body(move |p| {
                p.spawn(
                    Checkbox::new("eda-drc-refill")
                        .label("Refill all zones before performing DRC")
                        .checked(true)
                        .build(&t),
                );
                p.spawn((
                    Name::new("eda-drc-summary"),
                    t.text("", t.font_base, FontWeight::MEDIUM, t.foreground),
                    Node {
                        margin: UiRect::top(Val::Px(6.0)),
                        ..default()
                    },
                ));
                p.spawn(list_node("eda-drc-list", 300.0));
            })
            .footer(move |f| {
                button(f, &tf, "eda-drc-run", "Run DRC", true, run_drc);
                button(f, &tf, "eda-drc-close", "Close", false, close_all);
            }),
    );
}

fn run_drc(w: &mut World) {
    if checkbox(w, "eda-drc-refill") {
        ui::commit(w, "Fill zones", |d| {
            cadrs_eda::zone::fill_all(&mut d.board);
            Ok(())
        });
    }
    let Some((_, _, d)) = ui::current(w) else {
        return;
    };
    let rep = cadrs_eda::drc::check(&d.board);
    let mechanical = mechanical_context(w);
    let errors = rep.errors().count() + mechanical.len();
    let total = rep.violations.len() + mechanical.len();
    let warnings = total - errors;
    let summary = format!(
        "Violations ({total}): {errors} errors, {warnings} warnings. Unconnected items: {}",
        rep.unconnected.len()
    );
    ui::set_label(w, "eda-drc-summary", &summary);
    let mut lines = vec![];
    for m in mechanical {
        lines.push((format!("Error: {m}"), true));
    }
    for v in &rep.violations {
        lines.push((
            format!(
                "{}: {}",
                if v.rule.is_warning() {
                    "Warning"
                } else {
                    "Error"
                },
                v.message
            ),
            true,
        ));
        lines.extend(v.items.iter().map(|i| (format!("    {i}"), false)));
    }
    for a in &rep.unconnected {
        lines.push((
            format!(
                "Unconnected: {} ({:.2}, {:.2}) – ({:.2}, {:.2})",
                a.net,
                to_mm(a.a.x),
                to_mm(a.a.y),
                to_mm(a.b.x),
                to_mm(a.b.y)
            ),
            false,
        ));
    }
    list_lines(w, "eda-drc-list", lines);
}

/// The board's components running into parts of the Part Studios it is derived into:
/// "J1 AC inlet runs into Tube in Enclosure (0.80 mm
/// in plan, 2.40 mm high)".
fn mechanical_context(w: &World) -> Vec<String> {
    let Some((pcb, board, _)) = w.resource::<super::Eda2d>().board() else {
        return vec![];
    };
    let doc = &w.resource::<crate::ActiveDocument>().doc;
    let mut out = vec![];
    for el in &doc.elements {
        let deriving: Vec<cadrs_core::FeatureId> = el
            .features()
            .iter()
            .filter(|f| matches!(&f.kind, cadrs_core::FeatureKind::Derived(x) if x.source.is_some_and(|r| r.element == pcb && r.document.is_none_or(|d| d == doc.id)) && x.board.as_ref().is_some_and(|b| b.board == board)))
            .map(|f| f.id)
            .collect();
        if deriving.is_empty() {
            continue;
        }
        let build = cadrs_core::rebuild::build(&el.part_features());
        let name = |p: cadrs_core::PartId| {
            build
                .part(p)
                .map(|x| cadrs_core::parts::display_name(x, el.part_props()).to_string())
                .unwrap_or_default()
        };
        for f in deriving {
            for c in build
                .derived
                .get(&f)
                .map(|o| o.collisions.as_slice())
                .unwrap_or_default()
            {
                out.push(cadrs_core::pcb::fit::collision_text(
                    &name(c.component),
                    &format!("{} in {}", name(c.other), el.name),
                    c,
                ));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Plot (GS21)

pub fn open_plot(w: &mut World) {
    let t = w.resource::<Theme>().clone();
    let tf = t.clone();
    spawn_dialog(
        w,
        Dialog::new("eda-plot-dialog")
            .title("Plot")
            .width(600.0)
            .body(move |p| {
                row(p, &t, "Output directory", |r| {
                    r.spawn(TextInput::new("eda-plot-dir").value("fab/").width(Val::Px(320.0)).height(26.0).build(&t));
                });
                p.spawn(t.text("Gerber X2: copper, board outline, solder mask, silkscreen and paste layers, with a job file. Drill files: Excellon, plated and not. Assembly: JLCPCB's BOM and placement (CPL) files. STEP: the board with its parts' 3D models.", t.font_sm, FontWeight::NORMAL, t.muted_foreground));
                p.spawn(list_node("eda-plot-log", 180.0));
            })
            .footer(move |f| {
                button(f, &tf, "eda-plot-step", "STEP", false, run_step);
                button(f, &tf, "eda-plot-assembly", "JLCPCB BOM and placement", false, run_assembly);
                button(f, &tf, "eda-plot-drill", "Generate drill files", false, run_drill);
                button(f, &tf, "eda-plot-run", "Plot", true, run_plot);
                button(f, &tf, "eda-plot-close", "Close", false, close_all);
            }),
    );
}

fn board_name(w: &World) -> String {
    let Some((el, b, _)) = w.resource::<super::Eda2d>().board() else {
        return "board".into();
    };
    w.resource::<crate::ActiveDocument>()
        .doc
        .element(el)
        .and_then(|e| e.pcb())
        .and_then(|s| s.board(b))
        .map(|b| cadrs_idf::safe_file_name(b.name()))
        .unwrap_or_else(|| "board".into())
}

/// Writes the Gerbers and the job file into `dir`; returns what was written.
pub fn plot_to(w: &mut World, dir: &std::path::Path) -> Result<Vec<String>, String> {
    let Some((_, _, d)) = ui::current(w) else {
        return Err("No board".into());
    };
    let name = board_name(w);
    cadrs_fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out = vec![];
    for (file, text) in cadrs_eda::fab::gerbers(&d.board, &name) {
        cadrs_fs::write(dir.join(&file), text).map_err(|e| format!("{file}: {e}"))?;
        out.push(format!("Plotted {file}"));
    }
    let job = format!("{name}-job.gbrjob");
    cadrs_fs::write(dir.join(&job), cadrs_eda::fab::job_file(&d.board, &name))
        .map_err(|e| e.to_string())?;
    out.push(format!("Created Gerber job file {job}"));
    Ok(out)
}

/// Writes the plated and non-plated Excellon files into `dir`.
pub fn drill_to(w: &mut World, dir: &std::path::Path) -> Result<Vec<String>, String> {
    let Some((_, _, d)) = ui::current(w) else {
        return Err("No board".into());
    };
    let name = board_name(w);
    cadrs_fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out = vec![];
    for (plated, suffix) in [(true, "PTH"), (false, "NPTH")] {
        let file = format!("{name}-{suffix}.drl");
        cadrs_fs::write(dir.join(&file), cadrs_eda::fab::excellon(&d.board, plated))
            .map_err(|e| format!("{file}: {e}"))?;
        out.push(format!("Created {file}"));
    }
    Ok(out)
}

fn plot_dir(w: &mut World) -> std::path::PathBuf {
    let s = ui::text_value(w, "eda-plot-dir");
    std::path::PathBuf::from(if s.trim().is_empty() {
        "fab/".into()
    } else {
        s
    })
}

fn run_plot(w: &mut World) {
    let dir = plot_dir(w);
    let lines = match plot_to(w, &dir) {
        Ok(v) => v.into_iter().map(|l| (l, false)).collect(),
        Err(e) => vec![(format!("Error: {e}"), true)],
    };
    list_lines(w, "eda-plot-log", lines);
}

fn run_drill(w: &mut World) {
    let dir = plot_dir(w);
    let lines = match drill_to(w, &dir) {
        Ok(v) => v.into_iter().map(|l| (l, false)).collect(),
        Err(e) => vec![(format!("Error: {e}"), true)],
    };
    list_lines(w, "eda-plot-log", lines);
}

/// Writes JLCPCB's assembly files into `dir`: `<board>-bom.csv` (from the schematic) and
/// `<board>-cpl.csv` (the placement of the surface-mount parts).
pub fn assembly_to(w: &mut World, dir: &std::path::Path) -> Result<Vec<String>, String> {
    let Some((_, _, d)) = ui::current(w) else {
        return Err("No board".into());
    };
    let name = board_name(w);
    cadrs_fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out = vec![];
    for (file, text) in [
        (
            format!("{name}-bom.csv"),
            cadrs_eda::bom::jlcpcb_csv(&d.schematic),
        ),
        (
            format!("{name}-cpl.csv"),
            cadrs_eda::fab::jlcpcb_cpl(&d.board),
        ),
    ] {
        cadrs_fs::write(dir.join(&file), text).map_err(|e| format!("{file}: {e}"))?;
        out.push(format!("Created {file}"));
    }
    Ok(out)
}

fn run_assembly(w: &mut World) {
    let dir = plot_dir(w);
    let lines = match assembly_to(w, &dir) {
        Ok(v) => v.into_iter().map(|l| (l, false)).collect(),
        Err(e) => vec![(format!("Error: {e}"), true)],
    };
    list_lines(w, "eda-plot-log", lines);
}

/// Writes `<board>.step` into `dir`: the board and its parts' 3D models.
pub fn step_to(w: &mut World, dir: &std::path::Path) -> Result<Vec<String>, String> {
    let Some((el, b, d)) = ui::current(w) else {
        return Err("No board".into());
    };
    let pcb = w
        .resource::<crate::ActiveDocument>()
        .doc
        .element(el)
        .and_then(|e| e.pcb())
        .and_then(|s| s.board(b))
        .map(|x| x.board.clone())
        .ok_or("No board")?;
    let name = board_name(w);
    cadrs_fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (bytes, warnings) = cadrs_pcb::step::native_board_step(&pcb, &d)?;
    let file = format!("{name}.step");
    cadrs_fs::write(dir.join(&file), bytes).map_err(|e| format!("{file}: {e}"))?;
    let mut out = vec![format!("Created {file}")];
    out.extend(warnings.into_iter().map(|w| format!("Warning: {w}")));
    Ok(out)
}

fn run_step(w: &mut World) {
    let dir = plot_dir(w);
    let lines = match step_to(w, &dir) {
        Ok(v) => v
            .into_iter()
            .map(|l| {
                let warn = l.starts_with("Warning");
                (l, warn)
            })
            .collect(),
        Err(e) => vec![(format!("Error: {e}"), true)],
    };
    list_lines(w, "eda-plot-log", lines);
}
pub fn register(_app: &mut App) {}

// ---------------------------------------------------------------------------------------------
// Footprint and shape properties (E)

/// The footprint or shape the properties dialog edits.
#[derive(Resource, Clone, Copy)]
struct PropsOf(cadrs_eda::board_edit::BoardItem);

/// A footprint's properties: where it is (exact position and angle), which side, and whether
/// its 3D model shows.
pub fn open_footprint(w: &mut World, id: uuid::Uuid) {
    let Some((_, _, d)) = ui::current(w) else {
        return;
    };
    let Some(f) = d.board.footprints.iter().find(|f| f.id == id).cloned() else {
        return;
    };
    w.insert_resource(PropsOf(cadrs_eda::board_edit::BoardItem::Footprint(id)));
    let t = w.resource::<Theme>().clone();
    let tf = t.clone();
    let shows = f.footprint.models.iter().any(|m| m.visible);
    let has_model = !f.footprint.models.is_empty();
    spawn_dialog(
        w,
        Dialog::new("eda-fprops-dialog")
            .title(format!("Footprint properties: {}", f.reference()))
            .width(460.0)
            .body(move |p| {
                p.spawn(t.text(
                    f.footprint.id.clone(),
                    t.font_sm,
                    FontWeight::NORMAL,
                    t.muted_foreground,
                ));
                mm_row(p, &t, "Position X", "eda-fprops-x", f.placement.at.x);
                mm_row(p, &t, "Position Y", "eda-fprops-y", f.placement.at.y);
                row(p, &t, "Rotation", |r| {
                    r.spawn(
                        TextInput::new("eda-fprops-angle")
                            .value(format!("{}", f.placement.angle))
                            .width(Val::Px(120.0))
                            .height(26.0)
                            .build(&t),
                    );
                    r.spawn(t.text("°", t.font_sm, FontWeight::NORMAL, t.muted_foreground));
                });
                row(p, &t, "Side", |r| {
                    let i = usize::from(f.placement.side == cadrs_eda::layer::Side::Bottom);
                    r.spawn(
                        Select::new("eda-fprops-side")
                            .bordered()
                            .width(Val::Px(120.0))
                            .option("Top", true)
                            .option("Bottom", true)
                            .selected(i)
                            .build(&t),
                    );
                });
                if has_model {
                    p.spawn(
                        Checkbox::new("eda-fprops-model")
                            .label("Show its 3D model")
                            .checked(shows)
                            .build(&t),
                    );
                }
            })
            .footer(move |f| {
                button(f, &tf, "eda-fprops-ok", "OK", true, accept_footprint);
                button(f, &tf, "eda-fprops-cancel", "Cancel", false, close_all);
            }),
    );
}

/// What Create PCB board makes its board from: the Part Studio, its sketch and regions, and the
/// PCB Studios it can go to (in the dialog's order, "New PCB Studio" last).
#[derive(Resource, Clone)]
struct CreateBoardOf {
    element: cadrs_core::ElementId,
    sketch: cadrs_core::FeatureId,
    regions: Vec<cadrs_core::document::RegionRef>,
    studios: Vec<cadrs_core::ElementId>,
}

/// Part Studio → a sketch's menu → **Create PCB board…**:
/// the board's name, the PCB Studio it goes to (or a new one), its thickness and which face lies
/// on the sketch. The board's outline is the regions' (circles in them are holes) and follows
/// the sketch.
pub fn open_create_board(
    w: &mut World,
    element: cadrs_core::ElementId,
    sketch: cadrs_core::FeatureId,
    regions: Vec<cadrs_core::document::RegionRef>,
) {
    let doc = &w.resource::<crate::ActiveDocument>().doc;
    let sketch_name = doc
        .element(element)
        .and_then(|e| e.feature(sketch))
        .map(|f| f.name.clone())
        .unwrap_or_default();
    let studios: Vec<(cadrs_core::ElementId, String)> = doc
        .elements
        .iter()
        .filter(|e| e.pcb().is_some())
        .map(|e| (e.id, e.name.clone()))
        .collect();
    w.insert_resource(CreateBoardOf {
        element,
        sketch,
        regions,
        studios: studios.iter().map(|s| s.0).collect(),
    });
    let t = w.resource::<Theme>().clone();
    let tf = t.clone();
    spawn_dialog(
        w,
        Dialog::new("board-create-dialog")
            .title(format!("Create PCB board from {sketch_name}"))
            .width(460.0)
            .body(move |p| {
                row(p, &t, "Name", |r| {
                    r.spawn(
                        TextInput::new("board-create-name")
                            .value(format!("Board from {sketch_name}"))
                            .width(Val::Px(220.0))
                            .height(26.0)
                            .build(&t),
                    );
                });
                row(p, &t, "PCB Studio", |r| {
                    let mut sel = Select::new("board-create-studio")
                        .bordered()
                        .width(Val::Px(220.0));
                    for (_, n) in &studios {
                        sel = sel.option(n.clone(), true);
                    }
                    r.spawn(sel.option("New PCB Studio", true).selected(0).build(&t));
                });
                mm_row(p, &t, "Thickness", "board-create-thickness", mm(1.6));
                row(p, &t, "Face on the sketch", |r| {
                    r.spawn(
                        Select::new("board-create-seat")
                            .bordered()
                            .width(Val::Px(220.0))
                            .option("Bottom face (it stands on it)", true)
                            .option("Top face (it hangs under it)", true)
                            .selected(0)
                            .build(&t),
                    );
                });
                row(p, &t, "Circles inside it", |r| {
                    r.spawn(
                        Select::new("board-create-circles")
                            .bordered()
                            .width(Val::Px(220.0))
                            .option("Cut-outs of the outline", true)
                            .option("Mounting holes (NPTH)", true)
                            .selected(0)
                            .build(&t),
                    );
                });
                p.spawn(t.text(
                    "The outline (and the holes) follow the sketch.",
                    t.font_sm,
                    FontWeight::NORMAL,
                    t.muted_foreground,
                ));
            })
            .footer(move |f| {
                button(
                    f,
                    &tf,
                    "board-create-ok",
                    "Create",
                    true,
                    accept_create_board,
                );
                button(f, &tf, "board-create-cancel", "Cancel", false, close_all);
            }),
    );
}

fn accept_create_board(w: &mut World) {
    let Some(of) = w.get_resource::<CreateBoardOf>().cloned() else {
        return;
    };
    let name = ui::text_value(w, "board-create-name");
    let thickness = read_mm(w, "board-create-thickness");
    let studio = select_index(w, "board-create-studio").unwrap_or(0);
    let seat = if select_index(w, "board-create-seat") == Some(1) {
        cadrs_core::pcb::board3d::BoardSeat::Top
    } else {
        cadrs_core::pcb::board3d::BoardSeat::Bottom
    };
    let circles_as_holes = select_index(w, "board-create-circles") == Some(1);
    let thickness = match thickness {
        Ok(t) if t > 0 => to_mm(t),
        _ => {
            ui::toast(w, "The thickness is a length above 0 (mm)");
            return;
        }
    };
    close_all(w);
    let cmd = cadrs_core::pcb::outline_link::CreateBoardFromSketch {
        element: of.element,
        sketch: of.sketch,
        regions: of.regions,
        pcb: of.studios.get(studio).copied(),
        new_pcb: cadrs_core::ElementId::new(),
        name: if name.trim().is_empty() {
            "Board".into()
        } else {
            name.trim().to_string()
        },
        thickness,
        seat,
        circles_as_holes,
        feature: cadrs_core::FeatureId::new(),
    };
    if let Err(e) = w.resource_mut::<crate::ActiveDocument>().execute(&cmd) {
        crate::linked::error_toast(w, e.to_string());
    }
}

/// The footprints Move by moves.
#[derive(Resource, Clone)]
struct MoveByOf(Vec<uuid::Uuid>);

/// Move by…: the selected footprints moved by a typed distance along x and y (a board fitted
/// into a Part Studio: "0.5 mm further from the wall").
pub fn open_move_by(w: &mut World, ids: Vec<uuid::Uuid>) {
    if ids.is_empty() {
        ui::toast(w, "Select the footprints to move");
        return;
    }
    let what = match ui::current(w).map(|(_, _, d)| {
        d.board
            .footprints
            .iter()
            .filter(|f| ids.contains(&f.id))
            .map(|f| f.reference().to_string())
            .collect::<Vec<_>>()
    }) {
        Some(v) if v.len() <= 4 => v.join(", "),
        Some(v) => format!("{} footprints", v.len()),
        None => return,
    };
    w.insert_resource(MoveByOf(ids));
    let t = w.resource::<Theme>().clone();
    let tf = t.clone();
    spawn_dialog(
        w,
        Dialog::new("eda-moveby-dialog")
            .title(format!("Move by: {what}"))
            .width(380.0)
            .body(move |p| {
                mm_row(p, &t, "Along X", "eda-moveby-x", 0);
                mm_row(p, &t, "Along Y", "eda-moveby-y", 0);
            })
            .footer(move |f| {
                button(f, &tf, "eda-moveby-ok", "OK", true, accept_move_by);
                button(f, &tf, "eda-moveby-cancel", "Cancel", false, close_all);
            }),
    );
}

fn accept_move_by(w: &mut World) {
    let Some(MoveByOf(ids)) = w.get_resource::<MoveByOf>().cloned() else {
        return;
    };
    let (x, y) = (read_mm(w, "eda-moveby-x"), read_mm(w, "eda-moveby-y"));
    close_all(w);
    let (Ok(x), Ok(y)) = (x, y) else {
        ui::toast(w, "Type the distances as lengths (mm)");
        return;
    };
    if x == 0 && y == 0 {
        return;
    }
    ui::commit(w, "Move by", |d| {
        use cadrs_eda::board_edit as be;
        for id in &ids {
            let i = be::footprint_by_id(&d.board, *id).ok_or("A footprint is gone")?;
            let to = d.board.footprints[i].placement.at + Pt::new(x, y);
            be::move_footprint(&mut d.board, i, to);
        }
        Ok(())
    });
}

fn read_deg(w: &mut World, name: &str) -> Result<f64, String> {
    ui::text_value(w, name)
        .trim()
        .trim_end_matches('°')
        .trim()
        .parse::<f64>()
        .map_err(|_| "not an angle".to_string())
}

fn accept_footprint(w: &mut World) {
    let Some(PropsOf(cadrs_eda::board_edit::BoardItem::Footprint(id))) =
        w.get_resource::<PropsOf>().copied()
    else {
        return;
    };
    let (x, y, angle) = (
        read_mm(w, "eda-fprops-x"),
        read_mm(w, "eda-fprops-y"),
        read_deg(w, "eda-fprops-angle"),
    );
    let bottom = select_index(w, "eda-fprops-side") == Some(1);
    let has_model = {
        let mut q = w.query::<&Name>();
        q.iter(w).any(|n| n.as_str() == "eda-fprops-model")
    };
    let show = checkbox(w, "eda-fprops-model");
    close_all(w);
    let (Ok(x), Ok(y), Ok(angle)) = (x, y, angle) else {
        ui::toast(w, "Position is a length (mm), rotation an angle (°)");
        return;
    };
    ui::commit(w, "Footprint properties", |d| {
        use cadrs_eda::board_edit as be;
        let i = be::footprint_by_id(&d.board, id).ok_or("The footprint is gone")?;
        if (d.board.footprints[i].placement.side == cadrs_eda::layer::Side::Bottom) != bottom {
            be::flip_footprint(&mut d.board, i);
        }
        let turn = angle - d.board.footprints[i].placement.angle;
        if turn.abs() > 1e-9 {
            be::rotate_footprint(&mut d.board, i, turn);
        }
        be::move_footprint(&mut d.board, i, Pt::new(x, y));
        if has_model {
            // The first model shows or not (others, such as a STEP kept for mechanical CAD,
            // stay as they are).
            if let Some(m) = d.board.footprints[i].footprint.models.first_mut() {
                m.visible = show;
            }
        }
        Ok(())
    });
}

/// A drawn shape's properties: its stroke, whether it's filled, and (on copper) its net.
pub fn open_shape(w: &mut World, id: uuid::Uuid) {
    let Some((_, _, d)) = ui::current(w) else {
        return;
    };
    let Some(s) = d.board.shapes.iter().find(|s| s.id == id).cloned() else {
        return;
    };
    w.insert_resource(PropsOf(cadrs_eda::board_edit::BoardItem::Shape(id)));
    let nets: Vec<String> = std::iter::once(String::new())
        .chain(d.board.nets.iter().filter(|n| !n.is_empty()).cloned())
        .collect();
    let t = w.resource::<Theme>().clone();
    let tf = t.clone();
    spawn_dialog(
        w,
        Dialog::new("eda-sprops-dialog")
            .title(format!("Shape on {}", s.layer.name()))
            .width(460.0)
            .body(move |p| {
                mm_row(
                    p,
                    &t,
                    "Line width",
                    "eda-sprops-width",
                    s.shape.stroke.width,
                );
                p.spawn(
                    Checkbox::new("eda-sprops-filled")
                        .label("Filled")
                        .checked(s.shape.fill != cadrs_eda::graphics::Fill::None)
                        .build(&t),
                );
                if s.layer.is_copper() {
                    row(p, &t, "Net", |r| {
                        let mut sel = Select::new("eda-sprops-net")
                            .bordered()
                            .width(Val::Px(200.0));
                        for n in &nets {
                            sel =
                                sel.option(if n.is_empty() { "(none)" } else { n.as_str() }, true);
                        }
                        let i = nets.iter().position(|n| *n == s.net).unwrap_or(0);
                        r.spawn(sel.selected(i).build(&t));
                    });
                }
            })
            .footer(move |f| {
                button(f, &tf, "eda-sprops-ok", "OK", true, accept_shape);
                button(f, &tf, "eda-sprops-cancel", "Cancel", false, close_all);
            }),
    );
}

fn accept_shape(w: &mut World) {
    let Some(PropsOf(cadrs_eda::board_edit::BoardItem::Shape(id))) =
        w.get_resource::<PropsOf>().copied()
    else {
        return;
    };
    let width = read_mm(w, "eda-sprops-width");
    let filled = checkbox(w, "eda-sprops-filled");
    let net_i = select_index(w, "eda-sprops-net");
    let nets: Vec<String> = ui::current(w)
        .map(|(_, _, d)| {
            std::iter::once(String::new())
                .chain(d.board.nets.iter().filter(|n| !n.is_empty()).cloned())
                .collect()
        })
        .unwrap_or_default();
    close_all(w);
    let Ok(width) = width else {
        ui::toast(w, "The line width is a length in mm");
        return;
    };
    ui::commit(w, "Shape properties", |d| {
        let s = d
            .board
            .shapes
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or("The shape is gone")?;
        s.shape.stroke.width = width;
        s.shape.fill = if filled {
            cadrs_eda::graphics::Fill::Outline
        } else {
            cadrs_eda::graphics::Fill::None
        };
        if let Some(n) = net_i.and_then(|i| nets.get(i)) {
            s.net = n.clone();
        }
        Ok(())
    });
}
