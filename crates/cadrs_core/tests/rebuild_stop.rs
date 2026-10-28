//! A rebuild stopped early ([`rebuild::Pending::stop`], a newer request waiting): before the next
//! feature it would compute it gives the parts as far as it got, marked stopped; stopped before it
//! starts, nothing. Its own test binary: the rebuild worker and its progress are shared by a
//! binary's tests.
#![cfg(feature = "brep")]

use std::time::{Duration, Instant};

use cadrs_core::commands::{AddExtrude, AddSketch, EditSketch, SetExtrude};
use cadrs_core::document::{Document, ExtrudeFeature};
use cadrs_core::rebuild;
use cadrs_core::samples;
use cadrs_core::{ElementId, Feature, FeatureId, History};
use cadrs_sketch::{PlaneRef, SketchOp, Vec2};

/// `n` 10 mm squares side by side on Top, each extruded 5 mm as a part of its own.
fn blocks(n: usize) -> Vec<Feature> {
    let mut d = Document::new("Stop");
    let el: ElementId = d.elements[0].id;
    let mut h = History::default();
    for i in 0..n {
        let x = i as f64 * 20.0;
        let s = FeatureId::new();
        h.execute(
            &mut d,
            &AddSketch {
                element: el,
                feature: s,
                plane: Some(PlaneRef::Top),
            },
        )
        .unwrap();
        let v = Vec2::new;
        h.execute(
            &mut d,
            &EditSketch {
                element: el,
                feature: s,
                op: SketchOp::AddPolyline {
                    points: vec![v(x, 0.0), v(x + 10.0, 0.0), v(x + 10.0, 10.0), v(x, 10.0)],
                    closed: true,
                    construction: false,
                    label: "Add rectangle",
                },
            },
        )
        .unwrap();
        let g = d
            .element(el)
            .unwrap()
            .feature(s)
            .unwrap()
            .sketch()
            .unwrap()
            .geometry
            .clone();
        let regions = samples::region_refs(s, &g, &[v(x + 5.0, 5.0)]);
        let e = FeatureId::new();
        h.execute(
            &mut d,
            &AddExtrude {
                element: el,
                feature: e,
                extrude: ExtrudeFeature::default(),
            },
        )
        .unwrap();
        h.execute(
            &mut d,
            &SetExtrude {
                element: el,
                feature: e,
                extrude: ExtrudeFeature {
                    regions,
                    depth: 5.0,
                    depth_expr: "5 mm".into(),
                    ..ExtrudeFeature::default()
                },
                label: "Extrude".into(),
            },
        )
        .unwrap();
    }
    d.element(el).unwrap().features().to_vec()
}

#[test]
fn a_stopped_rebuild_gives_the_parts_as_far_as_it_got() {
    // Stopped before it starts (the worker busy with another job until then): nothing built.
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let busy = rebuild::run_on_worker(move |_| {
        let _ = wait.recv();
    });
    let features = blocks(3);
    let mut queued = rebuild::request(features.clone());
    queued.stop();
    go.send(()).unwrap();
    drop(busy);
    let b = queued.wait(None).expect("a stopped rebuild still answers");
    assert!(b.stopped, "stopped before it started");
    assert!(
        b.parts.is_empty() && b.computed == 0,
        "{} parts",
        b.parts.len()
    );

    // Stopped once it has started: it stops before its next feature, with the parts so far.
    let features = blocks(400);
    let mut running = rebuild::request(features.clone());
    let start = Instant::now();
    while running.progress().is_none_or(|(done, _)| done == 0) {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the rebuild never started"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    running.stop();
    let b = running.wait(None).expect("a stopped rebuild still answers");
    assert!(b.stopped, "stopped part way");
    assert!(
        !b.parts.is_empty() && b.parts.len() < 400,
        "the parts as far as it got: {}",
        b.parts.len()
    );

    // The features it got through are cached: the same list again builds them all (and is no
    // longer stopped), computing only the rest.
    let got = b.parts.len();
    let mut again = rebuild::request(features);
    let b = again.wait(None).expect("rebuilt");
    assert!(!b.stopped);
    assert_eq!(b.parts.len(), 400);
    assert!(
        b.computed <= 400 - got,
        "computed {} after {got} cached",
        b.computed
    );
}
