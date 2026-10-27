//! Two peers in one process, over real iroh with no relay: one shares a document, the other
//! fetches it by its link.

use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy_iroh::{Iroh, Relays};
use cadrs_core::Document;
use cadrs_core::collab::sync::Change;
use cadrs_core::collab::{Access, Permission, Snapshot};
use cadrs_net::{Ask, NetPlugin, Requests, ShareLink, Verdict, Welcome, transfer};

const PATIENCE: Duration = Duration::from_secs(30);
const TOPIC: [u8; 32] = [42; 32];

fn peer(name: &str) -> App {
    peer_starting(name, true)
}

fn peer_starting(name: &str, autostart: bool) -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        NetPlugin {
            identity: None,
            display_name: name.into(),
            relays: Relays::Disabled,
            autostart,
        },
    ));
    app
}

/// The owner's side: `doc` to anyone who knows the topic, refused otherwise.
#[derive(Resource)]
struct Shared(Document);

fn serve(mut requests: ResMut<Requests>, shared: Res<Shared>) {
    for r in requests.drain() {
        if r.document != shared.0.id || r.topic != TOPIC {
            r.refuse("Link sharing is off");
            continue;
        }
        match &r.ask {
            Ask::Join => {
                let doc = shared.0.clone();
                r.welcome(Ok(Welcome {
                    access: Access::owner(),
                    owner_name: "Owner".into(),
                    epoch: 9,
                    snapshot: Snapshot {
                        seq: 3,
                        document: doc,
                        blobs: Vec::new(),
                    },
                    comments: Default::default(),
                }))
            }
            // Numbered as 4 when it renames the document, refused otherwise.
            Ask::Propose {
                epoch: 9,
                base: 3,
                change,
                ..
            } if change.delta.head.is_some() => r.verdict(Verdict::Accepted(4)),
            _ => r.verdict(Verdict::Refused("No".into())),
        }
    }
}

fn wait<T>(owner: &mut App, joiner: &mut App, mut task: bevy_iroh::IrohTask<T>) -> T {
    let start = Instant::now();
    loop {
        owner.update();
        joiner.update();
        if let Some(r) = task.try_take() {
            eprintln!("answered in {:?}", start.elapsed());
            return r;
        }
        assert!(start.elapsed() < PATIENCE, "gave up waiting for the owner");
        std::thread::sleep(Duration::from_millis(16));
    }
}

fn fetch(owner: &mut App, joiner: &mut App, link: ShareLink) -> Result<Welcome, String> {
    let task = transfer::start_fetch(joiner.world().resource::<Iroh>(), link);
    wait(owner, joiner, task)
}

#[test]
fn a_document_and_edits_travel_by_its_link_and_a_wrong_topic_is_refused() {
    let mut owner = peer("Owner");
    let mut joiner = peer("Joiner");
    let mut doc = Document::new("Bracket");
    // Big enough to be well past bevy_iroh's one-megabyte messages, deflated or not.
    doc.name = (0..400_000).map(|i| format!("{i:x}")).collect::<String>();
    owner
        .insert_resource(Shared(doc.clone()))
        .add_systems(Update, serve);
    let addr = owner.world().resource::<Iroh>().addr().unwrap();
    let link = ShareLink {
        document: doc.id,
        name: doc.name.clone(),
        topic: TOPIC,
        owner: addr,
    };

    let welcome = fetch(&mut owner, &mut joiner, link.clone()).unwrap();
    assert_eq!(welcome.access.permission, Permission::Edit);
    assert_eq!(welcome.snapshot.seq, 3);
    assert_eq!(welcome.snapshot.document, doc);

    // An edit, proposed to the owner.
    let mut renamed = doc.clone();
    renamed.name = "Bracket v2".into();
    let change = Change::between(&doc, &renamed);
    let iroh = joiner.world().resource::<Iroh>();
    let task = transfer::start_propose(iroh, &link, 9, 1, 3, &change);
    assert_eq!(
        wait(&mut owner, &mut joiner, task),
        Ok(Verdict::Accepted(4))
    );
    let iroh = joiner.world().resource::<Iroh>();
    let task = transfer::start_propose(iroh, &link, 9, 2, 3, &Change::default());
    assert_eq!(
        wait(&mut owner, &mut joiner, task),
        Ok(Verdict::Refused("No".into()))
    );

    let wrong = ShareLink {
        topic: [0; 32],
        ..link
    };
    assert_eq!(
        fetch(&mut owner, &mut joiner, wrong),
        Err("Link sharing is off".into())
    );
}

#[test]
fn an_owner_off_the_network_until_asked_serves_after_each_start() {
    let mut owner = peer_starting("Owner", false);
    let mut joiner = peer("Joiner");
    let doc = Document::new("Bracket");
    owner
        .insert_resource(Shared(doc.clone()))
        .add_systems(Update, serve);
    owner.update();
    assert!(
        !owner.world().contains_resource::<Iroh>(),
        "offline until asked"
    );

    for _ in 0..2 {
        cadrs_net::start(owner.world_mut(), None, "Owner".into()).unwrap();
        let link = ShareLink {
            document: doc.id,
            name: doc.name.clone(),
            topic: TOPIC,
            owner: owner.world().resource::<Iroh>().addr().unwrap(),
        };
        let welcome = fetch(&mut owner, &mut joiner, link).unwrap();
        assert_eq!(welcome.snapshot.document, doc);
        cadrs_net::stop(owner.world_mut());
        assert!(!owner.world().contains_resource::<Iroh>());
    }
}
