//! A shared document's room: the owner hosts it while the document is open, everyone it is
//! shared with joins it. Numbered edits ([`Sequenced`]) reach everyone over it.

use bevy::prelude::*;
use bevy_iroh::RoomTicket;
use bevy_iroh::TopicId;
use bevy_iroh::message::MessageAppExt;
use bevy_iroh::prelude::Room;
use cadrs_core::collab::comments::CommentOp;
use cadrs_core::collab::{self, Access, sync::Change};
use cadrs_core::{DocumentId, ElementId, FeatureId};
use serde::{Deserialize, Serialize};

use crate::link::ShareLink;

/// The most an edit may take, packed, to travel in the room (gossip carries a megabyte, less
/// its envelope). A bigger one is announced without it, and peers fetch the document.
pub const MAX_IN_ROOM: usize = 900 * 1024;

/// Edit `seq` of the owner's session `epoch`, as the owner applied it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sequenced {
    pub document: DocumentId,
    pub epoch: u64,
    pub seq: u64,
    /// Who made it: their cadrs ID and name.
    pub author: String,
    pub author_name: String,
    /// The id its author gave it.
    pub id: u64,
    /// The packed [`Change`]; `None` when it was too big for the room.
    pub change: Option<Vec<u8>>,
    /// [`cadrs_core::history_log::document_hash`] of the owner's document after it.
    pub hash: u64,
}

impl Sequenced {
    /// The message for `change`, without it if it is too big.
    pub fn new(
        document: DocumentId,
        epoch: u64,
        seq: u64,
        author: (&str, &str),
        id: u64,
        change: &Change,
        hash: u64,
    ) -> Self {
        let change = collab::pack(change).ok().filter(|b| b.len() <= MAX_IN_ROOM);
        Self {
            document,
            epoch,
            seq,
            author: author.0.to_string(),
            author_name: author.1.to_string(),
            id,
            change,
            hash,
        }
    }

    pub fn change(&self) -> Option<Change> {
        collab::unpack(self.change.as_deref()?).ok()
    }
}

/// Who may do what now, from the owner: each member's access and the link's. Sent to the
/// room whenever the owner changes the sharing, so everyone in it follows at once (and
/// someone removed knows to leave).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccessChanged {
    pub document: DocumentId,
    pub members: Vec<(String, Access)>,
    pub link: Option<Access>,
}

impl AccessChanged {
    /// What `peer` may do now, if anything.
    pub fn access(&self, peer: &str) -> Option<Access> {
        self.members
            .iter()
            .find(|(id, _)| id == peer)
            .map(|(_, a)| *a)
            .or(self.link)
    }
}

/// Where someone is in a shared document and what they look at, a few times a second while
/// it changes (and every few seconds regardless): Onshape's social cues and Follow mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    pub document: DocumentId,
    pub name: String,
    /// The tab they are in.
    pub tab: Option<ElementId>,
    /// The feature whose dialog (or sketch) they have open.
    pub editing: Option<(ElementId, FeatureId)>,
    /// Their 3D view, for whoever follows them.
    pub view: Option<View>,
    /// Who they follow (a cadrs ID).
    pub following: Option<String>,
}

/// Where someone's pointer is in the tab they are in: sent many times a second while it moves
/// and every few seconds while it rests, so the others see it live. Peers from before it ignore
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pointer {
    pub document: DocumentId,
    pub tab: ElementId,
    /// `None` while the pointer is off the tab's view (over a panel, out of the window).
    pub at: Option<PointerAt>,
}

/// A pointer position, in the units of what the tab shows.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PointerAt {
    /// In a 3D view: the model point (mm) on the part under it, else on the ground grid.
    Model([f32; 3]),
    /// On a 2D view of a PCB Studio: the board or component (`EdaOf`), the view's mode (the
    /// app's code for schematic, layout, symbol or footprint) and the design point (nm).
    Design { of: EdaOf, mode: u8, at: [i64; 2] },
    /// On a drawing's sheet (its id) at a sheet point (mm).
    Sheet { sheet: u128, at: [f32; 2] },
}

/// What a PCB Studio's 2D view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdaOf {
    Board(u64),
    Component(u64),
}

/// A 3D view as it travels: orientation, centre, zoom, projection and render mode.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub azimuth: f32,
    pub elevation: f32,
    pub roll: f32,
    pub focus: [f32; 3],
    pub scale: f32,
    pub perspective: bool,
    /// The render mode's slug ("shaded", …).
    pub render: u8,
}

/// A change to the comments, as the owner applied it: everyone applies it the same.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentChanged {
    pub document: DocumentId,
    /// Who made it (a cadrs ID).
    pub by: String,
    /// The packed [`CommentOp`] (postcard can't carry its optional fields).
    op: Vec<u8>,
}

impl CommentChanged {
    pub fn new(document: DocumentId, by: &str, op: &CommentOp) -> Option<Self> {
        Some(Self {
            document,
            by: by.to_string(),
            op: collab::pack(op).ok()?,
        })
    }

    pub fn op(&self) -> Option<CommentOp> {
        collab::unpack(&self.op).ok()
    }
}

/// A version the owner made of the shared document (on its own, or asked by someone it is
/// shared with): at edit `seq` of session `epoch`. Everyone makes the same version, with the
/// same id, of their copy at that edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionCreated {
    pub document: DocumentId,
    pub epoch: u64,
    pub seq: u64,
    pub id: uuid::Uuid,
    pub name: String,
    pub description: String,
    pub time: i64,
    /// Who asked for it.
    pub user: String,
}

/// The tabs someone has open for themselves ("Open tab, close it for other users", Onshape's
/// focus lock for tabs that can't be edited together): each tab with its holder's cadrs ID and
/// name. From the owner, whenever it changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocksChanged {
    pub document: DocumentId,
    pub locks: Vec<(ElementId, String, String)>,
}

pub(crate) fn register(app: &mut App) {
    app.add_net_message::<Sequenced>()
        .add_net_message::<AccessChanged>()
        .add_net_message::<Presence>()
        .add_net_message::<CommentChanged>()
        .add_net_message::<VersionCreated>()
        .add_net_message::<LocksChanged>()
        .add_net_message::<Pointer>();
}

/// The owner's room for a document shared in `topic`: always the same topic, so links keep
/// working (a ticket naming no one opens the topic and waits for others).
pub fn host_room(commands: &mut Commands, topic: [u8; 32], name: &str) -> Entity {
    let ticket = RoomTicket::new(TopicId::from_bytes(topic), name, Vec::new());
    commands.spawn(Room::join(ticket)).id()
}

/// The room a link names, joined through its owner.
pub fn join_room(commands: &mut Commands, link: &ShareLink) -> Entity {
    let ticket = RoomTicket::new(
        TopicId::from_bytes(link.topic),
        link.name.clone(),
        vec![link.owner.clone()],
    );
    commands.spawn(Room::join(ticket)).id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadrs_core::collab::comments::{Comment, Tag};
    use cadrs_core::collab::{Permission, Rights};

    /// bevy_iroh carries room messages as postcard, which can't skip a field: every message
    /// must come back whole, optional fields set or not.
    #[test]
    fn room_messages_survive_postcard() {
        let document = DocumentId::new();
        let comment = Comment {
            id: uuid::Uuid::new_v4(),
            parent: None,
            author: "a".into(),
            author_name: "Ann".into(),
            time: 5,
            text: "hi".into(),
            tag: Some(Tag::Element(ElementId::new())),
            assignee: None,
            resolved: false,
            edited: None,
        };
        let c = CommentChanged::new(document, "a", &CommentOp::Add(comment.clone())).unwrap();
        let back: CommentChanged = postcard::from_bytes(&postcard::to_stdvec(&c).unwrap()).unwrap();
        assert_eq!(back.op(), Some(CommentOp::Add(comment)));

        let p = Presence {
            document,
            name: "Ann".into(),
            tab: Some(ElementId::new()),
            editing: None,
            view: Some(View {
                azimuth: 1.0,
                elevation: 2.0,
                roll: 0.0,
                focus: [1.0, 2.0, 3.0],
                scale: 0.5,
                perspective: true,
                render: 2,
            }),
            following: None,
        };
        let back: Presence = postcard::from_bytes(&postcard::to_stdvec(&p).unwrap()).unwrap();
        assert_eq!(back, p);

        let a = AccessChanged {
            document,
            members: vec![(
                "b".into(),
                Access {
                    permission: Permission::View,
                    rights: Rights::of(Permission::View),
                },
            )],
            link: None,
        };
        let back: AccessChanged = postcard::from_bytes(&postcard::to_stdvec(&a).unwrap()).unwrap();
        assert_eq!(back, a);

        let s = Sequenced::new(document, 3, 4, ("a", "Ann"), 1, &Change::default(), 9);
        let back: Sequenced = postcard::from_bytes(&postcard::to_stdvec(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
