//! A dialog's ✓ keeps its feature only if the feature rebuilds. It asks the rebuild worker and
//! comes back when the answer is in, rather than waiting on the UI thread: a boolean with a
//! big imported STL took a minute, and the window stopped responding meanwhile (issue #11).

use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use cadrs_core::rebuild::{Build, Pending};
use cadrs_core::{Feature, FeatureId};

pub struct AcceptPlugin;

impl Plugin for AcceptPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, retry_when_built);
    }
}

/// A ✓ waiting for the rebuild of `features`; `retry` is the dialog's accept, called again
/// once it is done.
#[derive(Resource)]
struct Waiting {
    key: &'static str,
    features: Vec<Feature>,
    job: Pending,
    retry: fn(&mut World),
}

/// The finished rebuild a retried accept reads.
#[derive(Resource)]
struct Built {
    key: &'static str,
    features: Vec<Feature>,
    build: Arc<Build>,
}

/// Whether `feature` builds among `features` (the active ones): `Some` once known, `None`
/// while the worker builds them, `retry` being called again when it is done. A quick rebuild
/// answers at once. `key` names the dialog.
pub fn builds(
    world: &mut World,
    key: &'static str,
    mut features: Vec<Feature>,
    feature: FeatureId,
    retry: fn(&mut World),
) -> Option<bool> {
    // Only the features up to it: the ones after don't change whether it builds, and a feature
    // edited before the end would wait for all of them (its dialog shows the Part Studio rolled
    // back to it, so they may not be built yet). They rebuild in the view once it is kept.
    if let Some(i) = features.iter().position(|f| f.id == feature) {
        features.truncate(i + 1);
    }
    if let Some(b) = world.get_resource::<Built>()
        && b.key == key
        && b.features == features
    {
        return Some(b.build.error(feature).is_none());
    }
    if world
        .get_resource::<Waiting>()
        .is_some_and(|w| w.key == key && w.features == features)
    {
        return None;
    }
    // Anything older is dropped (a queued rebuild of it is cancelled), and the view's rebuild
    // running now (the dialog's preview, or one the dialog was opened during) stops rather than
    // finish first.
    world.remove_resource::<Waiting>();
    if let Some(cache) = world.get_resource::<crate::parts::PartCache>() {
        cache.stop_rebuild();
    }
    let mut job = cadrs_core::rebuild::request_persisted(features.clone());
    if let Some(build) = job.wait(Some(Duration::from_millis(50))) {
        return Some(build.error(feature).is_none());
    }
    world.insert_resource(Waiting {
        key,
        features,
        job,
        retry,
    });
    None
}

fn retry_when_built(world: &mut World) {
    let Some(mut w) = world.remove_resource::<Waiting>() else {
        return;
    };
    let Some(build) = w.job.poll() else {
        world.insert_resource(w);
        // Scripted steps wait for the ✓ too.
        if let Some(mut p) = world.get_resource_mut::<cadrs_ui::PendingWork>() {
            p.0 = true;
        }
        if let Some(mut why) = world.get_resource_mut::<cadrs_ui::PendingWhy>() {
            why.add("dialog check");
        }
        return;
    };
    world.insert_resource(Built {
        key: w.key,
        features: w.features,
        build,
    });
    (w.retry)(world);
    world.remove_resource::<Built>();
}
