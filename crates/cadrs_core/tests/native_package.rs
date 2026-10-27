use cadrs_core::{Command, Document, DocumentMeta, FeatureId, Store, blobs, native};
use std::{
    io::{Read, Write},
    path::Path,
};

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn document() -> Document {
    let mut doc = Document::new("Portable assembly");
    cadrs_core::commands::AddSketch {
        element: doc.elements[0].id,
        feature: FeatureId::new(),
        plane: Some(cadrs_sketch::PlaneRef::Top),
    }
    .apply(&mut doc)
    .unwrap();
    cadrs_core::import::AddImport {
        element: doc.elements[0].id,
        feature: FeatureId::new(),
        file_name: "bracket.stl".into(),
        bytes: std::sync::Arc::new(include_bytes!("../../../fixtures/import_bracket.stl").to_vec()),
        y_axis_up: false,
        units: None,
    }
    .apply(&mut doc)
    .unwrap();
    doc
}
fn rewrite(source: &Path, dest: &Path, edit: impl Fn(&str, Vec<u8>) -> Option<Vec<u8>>) {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(source).unwrap()).unwrap();
    let mut out = zip::ZipWriter::new(std::fs::File::create(dest).unwrap());
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        let name = entry.name().unwrap().into_owned();
        if let Some(bytes) = edit(&name, bytes) {
            out.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            out.write_all(&bytes).unwrap();
        }
    }
    out.finish().unwrap();
}
#[test]
fn portable_round_trip_and_repeated_import_capture_current_state() {
    let _guard = TEST_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let source = Store::new(temp.path().join("source"));
    let mut doc = document();
    let meta = DocumentMeta::new("author", 100);
    source.create(&doc, &meta).unwrap();
    let thumbnail = std::fs::read(source.thumbnail_path(doc.id)).unwrap();
    doc.name = "Unsaved edit".into();
    cadrs_core::commands::AddSketch {
        element: doc.elements[0].id,
        feature: FeatureId::new(),
        plane: Some(cadrs_sketch::PlaneRef::Front),
    }
    .apply(&mut doc)
    .unwrap();
    let package = temp.path().join("test.cadrs");
    native::export(&package, &doc, &meta, &source).unwrap();
    let (hash, _) = blobs::used_by(&doc)[0].clone();
    let expected = blobs::get(&hash).unwrap();
    blobs::forget(&hash);
    native::export(&temp.path().join("disk-assets.cadrs"), &doc, &meta, &source).unwrap();
    std::fs::remove_dir_all(source.root()).unwrap();
    let target = Store::new(temp.path().join("target"));
    let first = native::import(&package, &target, "recipient", 200).unwrap();
    let second = native::import(&package, &target, "recipient", 201).unwrap();
    assert_ne!(first.document.id, doc.id);
    assert_ne!(first.document.id, second.document.id);
    let mut actual = first.document.clone();
    actual.id = doc.id;
    assert_eq!(actual, doc);
    assert_eq!(blobs::get(&hash).unwrap(), expected);
    assert_eq!(
        target.load(first.document.id).unwrap().document,
        first.document
    );
    assert!(target.thumbnail_path(first.document.id).exists());
    assert_eq!(
        std::fs::read(target.thumbnail_path(first.document.id)).unwrap(),
        thumbnail
    );
}
#[test]
fn invalid_packages_leave_no_partial_documents() {
    let _guard = TEST_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let source = Store::new(temp.path().join("source"));
    let doc = document();
    let original = temp.path().join("good.cadrs");
    native::export(&original, &doc, &DocumentMeta::new("a", 1), &source).unwrap();
    for mode in [
        "missing",
        "corrupt-asset",
        "future",
        "future-document",
        "future-kind",
        "missing-document",
    ] {
        let bad = temp.path().join(format!("{mode}.cadrs"));
        rewrite(&original, &bad, |name, bytes| match mode {
            "missing" if name.starts_with("blobs/") => None,
            "corrupt-asset" if name.starts_with("blobs/") => Some(b"corrupt".to_vec()),
            "future" if name == "manifest.ron" => Some(b"(format:\"cadrs\",version:999)".to_vec()),
            "future-document" if name == "document.ron" => {
                let mut file: cadrs_core::store::DocumentFile =
                    ron::de::from_bytes(&bytes).unwrap();
                file.version = 999;
                Some(ron::to_string(&file).unwrap().into_bytes())
            }
            // A tab kind a newer cadrs added within this schema version.
            "future-kind" if name == "document.ron" => Some(
                String::from_utf8(bytes)
                    .unwrap()
                    .replacen("PartStudio(", "FutureStudio(", 1)
                    .into_bytes(),
            ),
            "missing-document" if name == "document.ron" => None,
            _ => Some(bytes),
        });
        let target = Store::new(temp.path().join(mode));
        let e = native::import(&bad, &target, "a", 2).unwrap_err();
        // One saved by a newer cadrs says so, and to update.
        if mode.starts_with("future") {
            assert_eq!(e, cadrs_core::store::newer_reason(), "{mode}");
        }
        assert!(!target.root().exists());
    }
    let unsafe_path = temp.path().join("unsafe.cadrs");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&unsafe_path).unwrap());
    zip.start_file("../escape", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"unsafe").unwrap();
    zip.finish().unwrap();
    assert!(
        native::import(
            &unsafe_path,
            &Store::new(temp.path().join("unsafe")),
            "a",
            1
        )
        .unwrap_err()
        .contains("Unsafe")
    );
    let corrupt = temp.path().join("corrupt.cadrs");
    std::fs::write(&corrupt, b"PKcorrupt").unwrap();
    assert!(native::import(&corrupt, &Store::new(temp.path().join("corrupt")), "a", 1).is_err());
}
#[test]
fn legacy_text_and_write_failure() {
    let _guard = TEST_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let target = Store::new(temp.path().join("library"));
    let legacy = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/move_to_document_standin.cadrs"
    ));
    let imported = native::import(legacy, &target, "recipient", 100).unwrap();
    assert!(!imported.document.elements.is_empty());
    let output = temp.path().join("out.cadrs");
    std::fs::write(&output, b"keep existing file").unwrap();
    let doc = Document::new("Example");
    let meta = DocumentMeta::new("a", 1);
    assert!(native::export(&output, &doc, &meta, &target).is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"keep existing file");
    assert!(native::export(&temp.path().join("missing/out.cadrs"), &doc, &meta, &target).is_err());
}

#[test]
fn migrations_and_missing_export_asset() {
    let _guard = TEST_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path().join("library"));
    let doc = Document::new("Old schema");
    let file = cadrs_core::store::DocumentFile {
        version: 4,
        meta: DocumentMeta::new("a", 1),
        document: doc,
    };
    let legacy = temp.path().join("old.cadrs");
    std::fs::write(&legacy, ron::to_string(&file).unwrap()).unwrap();
    let imported = native::import(&legacy, &store, "b", 2).unwrap();
    assert_eq!(imported.version, cadrs_core::store::SCHEMA_VERSION);
    let doc = document();
    let (hash, _) = blobs::used_by(&doc)[0].clone();
    blobs::forget(&hash);
    let output = temp.path().join("missing-asset.cadrs");
    assert!(
        native::export(&output, &doc, &file.meta, &store)
            .unwrap_err()
            .contains("Missing required asset")
    );
    assert!(!output.exists());
}

#[test]
fn oversized_archive_entry_is_rejected_before_reading() {
    let _guard = TEST_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("oversized.cadrs");
    let mut out = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    out.start_file("large", zip::write::SimpleFileOptions::default())
        .unwrap();
    out.write_all(b"x").unwrap();
    let mut bytes = out.finish().unwrap().into_inner();
    let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    let declared = (512u32 * 1024 * 1024 + 1).to_le_bytes();
    bytes[central + 24..central + 28].copy_from_slice(&declared);
    bytes[22..26].copy_from_slice(&declared);
    std::fs::write(&path, bytes).unwrap();
    let store = Store::new(temp.path().join("library"));
    assert!(
        native::import(&path, &store, "a", 1)
            .unwrap_err()
            .contains("size limit")
    );
    assert!(!store.root().exists());
}
