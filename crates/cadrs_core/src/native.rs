//! Portable, editable `.cadrs` ZIP packages. Library storage remains unchanged.
use crate::{
    Document, DocumentId, DocumentMeta, Store, blobs,
    store::{DocumentFile, SCHEMA_VERSION, StoreError},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::Path,
};

const MAX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 16_384;
#[derive(Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u32,
}

/// Writes current in-memory state, required source assets, and available thumbnails atomically.
pub fn export(
    path: &Path,
    doc: &Document,
    meta: &DocumentMeta,
    store: &Store,
) -> Result<(), String> {
    let mut entries = BTreeMap::new();
    entries.insert(
        "manifest.ron".to_string(),
        ron::to_string(&Manifest {
            format: "cadrs".into(),
            version: 1,
        })
        .map_err(|e| e.to_string())?
        .into_bytes(),
    );
    let file = DocumentFile {
        version: SCHEMA_VERSION,
        meta: meta.clone(),
        document: doc.clone(),
    };
    entries.insert(
        "document.ron".into(),
        ron::ser::to_string_pretty(&file, Default::default())
            .map_err(|e| e.to_string())?
            .into_bytes(),
    );
    for (hash, ext) in blobs::used_by(doc) {
        let bytes = blobs::bytes_for(&hash, &store.blobs_dir(doc.id))?;
        entries.insert(format!("blobs/{}", blobs::file_name(&hash, &ext)), bytes);
    }
    for name in ["thumbnail.png", "thumbnail-large.png"] {
        let p = store.doc_dir(doc.id).join(name);
        match std::fs::read(p) {
            Ok(bytes) => {
                entries.insert(name.into(), bytes);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if entries.len() > MAX_ENTRIES
        || entries.values().map(|b| b.len() as u64).sum::<u64>() > MAX_BYTES
    {
        return Err("Document exceeds native package limits".into());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    {
        let mut zip = zip::ZipWriter::new(temp.as_file_mut());
        for (name, bytes) in entries {
            zip.start_file(
                name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .map_err(|e| e.to_string())?;
            zip.write_all(&bytes).map_err(|e| e.to_string())?;
        }
        zip.finish().map_err(|e| e.to_string())?;
    }
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    temp.persist_noclobber(path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Validates a complete package, then installs a fresh library copy in one directory rename.
/// Legacy text documents are read with the existing schema migrations and adjacent assets.
pub fn import(
    path: &Path,
    store: &Store,
    user: &str,
    now: crate::library::Timestamp,
) -> Result<DocumentFile, String> {
    let mut source = File::open(path).map_err(|e| e.to_string())?;
    let mut magic = [0; 2];
    source.read_exact(&mut magic).map_err(|e| e.to_string())?;
    let (mut file, assets, thumbnails) = if &magic == b"PK" {
        let mut zip = zip::ZipArchive::new(File::open(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        if zip.len() > MAX_ENTRIES {
            return Err("Too many archive entries".into());
        }
        let mut entries = BTreeMap::new();
        let mut total = 0u64;
        for i in 0..zip.len() {
            let entry = zip.by_index(i).map_err(|e| e.to_string())?;
            let name = entry.name().map_err(|e| e.to_string())?.to_string();
            if name.contains('\\')
                || name.split('/').any(|s| s == ".." || s == ".")
                || entry.enclosed_name().is_none()
                || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
            {
                return Err("Unsafe archive path".into());
            }
            if entry.is_dir() {
                continue;
            }
            let remaining = MAX_BYTES
                .checked_sub(total)
                .ok_or("Archive exceeds size limit")?;
            if entry.size() > remaining {
                return Err("Archive exceeds size limit".into());
            }
            let mut bytes = Vec::new();
            entry
                .take(remaining + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            total += bytes.len() as u64;
            if total > MAX_BYTES {
                return Err("Archive exceeds size limit".into());
            }
            if entries.insert(name, bytes).is_some() {
                return Err("Duplicate archive entry".into());
            }
        }
        let manifest: Manifest =
            ron::de::from_bytes(entries.get("manifest.ron").ok_or("Missing manifest.ron")?)
                .map_err(|e| e.to_string())?;
        if manifest.format == "cadrs" && manifest.version > 1 {
            return Err(crate::store::newer_reason());
        }
        if manifest.format != "cadrs" || manifest.version != 1 {
            return Err("Unsupported cadrs archive version or format".into());
        }
        let text = std::str::from_utf8(entries.get("document.ron").ok_or("Missing document.ron")?)
            .map_err(|e| e.to_string())?;
        let file = Store::parse_document(text).map_err(reason)?;
        let mut assets = Vec::new();
        for (hash, ext) in blobs::used_by(&file.document) {
            let name = blobs::file_name(&hash, &ext);
            let bytes = entries
                .get(&format!("blobs/{name}"))
                .ok_or_else(|| format!("Missing required asset {name}"))?;
            if blobs::hash_of(bytes) != hash {
                return Err(format!("Asset {name} failed validation"));
            }
            assets.push((name, bytes.clone()));
        }
        let thumbnails = ["thumbnail.png", "thumbnail-large.png"]
            .into_iter()
            .filter_map(|n| entries.remove(n).map(|b| (n, b)))
            .collect::<Vec<_>>();
        (file, assets, thumbnails)
    } else {
        if source.metadata().map_err(|e| e.to_string())?.len() > MAX_BYTES {
            return Err("Document exceeds size limit".into());
        }
        let file = Store::load_path(path).map_err(reason)?;
        let mut assets = Vec::new();
        for (hash, ext) in blobs::used_by(&file.document) {
            let bytes =
                blobs::get(&hash).ok_or_else(|| format!("Missing required asset {hash}"))?;
            if blobs::hash_of(&bytes) != hash {
                return Err(format!("Asset {hash} failed validation"));
            }
            assets.push((blobs::file_name(&hash, &ext), bytes.to_vec()));
        }
        (file, assets, Vec::new())
    };
    file.document.id = DocumentId::new();
    file.meta = DocumentMeta::new(user, now);
    file.meta.last_opened = Some(now);
    std::fs::create_dir_all(store.root()).map_err(|e| e.to_string())?;
    let stage = tempfile::tempdir_in(store.root()).map_err(|e| e.to_string())?;
    let staging = Store::new(stage.path());
    // Persist source bytes before saving; Store::save may also find these in its cache.
    for (name, bytes) in &assets {
        let dir = staging.blobs_dir(file.document.id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(name), bytes).map_err(|e| e.to_string())?;
    }
    staging
        .create(&file.document, &file.meta)
        .map_err(|e| e.to_string())?;
    for (name, bytes) in thumbnails {
        std::fs::write(staging.doc_dir(file.document.id).join(name), bytes)
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(
        staging.doc_dir(file.document.id),
        store.doc_dir(file.document.id),
    )
    .map_err(|e| e.to_string())?;
    for (_, bytes) in assets {
        blobs::insert(bytes);
    }
    Ok(file)
}

/// A document's read error as the import reports it.
fn reason(e: StoreError) -> String {
    if e.is_newer() {
        crate::store::newer_reason()
    } else {
        e.to_string()
    }
}
