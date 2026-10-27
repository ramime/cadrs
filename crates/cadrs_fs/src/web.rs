//! The file tree of a page: files and folders in memory, kept in the page's IndexedDB by the
//! page's script (`globalThis.cadrsFiles`, see `tools/web/files.js`). The script reads every
//! file before the app starts, and this module hands it each change to write behind; without
//! the script (a test page) the tree lives as long as the page.
//!
//! - Paths are absolute from `/`: a relative path is taken from `/`, and `.` and `..` are
//!   resolved. There are no links.
//! - The errors are `std::fs`'s on Linux, as far as the app tells them apart (`NotFound`,
//!   `AlreadyExists`, `IsADirectory`, `NotADirectory`, `DirectoryNotEmpty`).
//! - A file written in the [`set_downloads`] folder is also handed to the browser as a
//!   download: that is how an export leaves the page.
//! - The [`set_scratch`] folders' files are not kept: they last as long as the page.
//! - A [`File`] holds its contents while it is open and writes them back when it is flushed,
//!   synced or dropped.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use web_time::{SystemTime, UNIX_EPOCH};

/// The page's script (`globalThis.cadrsFiles`). In the crate's own tests (natively) there is
/// none: nothing is kept.
mod page {
    #[cfg(target_arch = "wasm32")]
    mod js {
        use wasm_bindgen::prelude::*;

        #[wasm_bindgen(inline_js = "
export function cadrs_files_take() {
  const f = globalThis.cadrsFiles;
  if (!f || !f.loaded) return [];
  const all = f.loaded;
  f.loaded = null;
  return all;
}
export function cadrs_files_put(path, data, mtime) {
  globalThis.cadrsFiles?.put(path, data === undefined ? null : data.slice(), mtime);
}
export function cadrs_files_remove(path) { globalThis.cadrsFiles?.remove(path); }
export function cadrs_files_download(name, data) { globalThis.cadrsFiles?.download(name, data.slice()); }
")]
        extern "C" {
            /// The files the page read from IndexedDB: `[path, Uint8Array or null (a folder),
            /// modified (ms since 1970)]` each. Empty after the first call.
            pub fn cadrs_files_take() -> js_sys::Array;
            /// Keeps a file (`data`, copied) or a folder (`None`).
            pub fn cadrs_files_put(path: &str, data: Option<&[u8]>, mtime: f64);
            pub fn cadrs_files_remove(path: &str);
            pub fn cadrs_files_download(name: &str, data: &[u8]);
        }
    }

    /// The files kept: path, contents (`None`: a folder), modified (ms since 1970).
    #[cfg(target_arch = "wasm32")]
    pub fn take() -> Vec<(String, Option<Vec<u8>>, f64)> {
        js::cadrs_files_take()
            .iter()
            .filter_map(|e| {
                let e = js_sys::Array::from(&e);
                let data = e.get(1);
                let data = (!data.is_null() && !data.is_undefined())
                    .then(|| js_sys::Uint8Array::new(&data).to_vec());
                Some((
                    e.get(0).as_string()?,
                    data,
                    e.get(2).as_f64().unwrap_or(0.0),
                ))
            })
            .collect()
    }
    #[cfg(target_arch = "wasm32")]
    pub use js::{
        cadrs_files_download as download, cadrs_files_put as put, cadrs_files_remove as remove,
    };

    #[cfg(not(target_arch = "wasm32"))]
    pub fn take() -> Vec<(String, Option<Vec<u8>>, f64)> {
        Vec::new()
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn put(_: &str, _: Option<&[u8]>, _: f64) {}
    #[cfg(not(target_arch = "wasm32"))]
    pub fn remove(_: &str) {}
    #[cfg(not(target_arch = "wasm32"))]
    pub fn download(_: &str, _: &[u8]) {}
}

#[derive(Clone)]
enum Node {
    File {
        data: Rc<Vec<u8>>,
        modified: SystemTime,
    },
    Dir {
        modified: SystemTime,
    },
}

impl Node {
    fn meta(&self) -> Metadata {
        match self {
            Node::File { data, modified } => Metadata {
                dir: false,
                len: data.len() as u64,
                modified: *modified,
            },
            Node::Dir { modified } => Metadata {
                dir: true,
                len: 0,
                modified: *modified,
            },
        }
    }
}

struct Tree {
    nodes: BTreeMap<PathBuf, Node>,
    downloads: Option<PathBuf>,
    scratch: Vec<PathBuf>,
}

thread_local! {
    static TREE: RefCell<Option<Tree>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Tree) -> R) -> R {
    TREE.with(|t| f(t.borrow_mut().get_or_insert_with(Tree::load)))
}

fn millis(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

fn root() -> PathBuf {
    PathBuf::from("/")
}

/// `path` from `/`, with `.` and `..` resolved.
fn norm(path: &Path) -> PathBuf {
    let mut out = root();
    for c in path.components() {
        match c {
            Component::RootDir | Component::Prefix(_) => out = root(),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
        }
    }
    out
}

fn error(kind: ErrorKind, what: &str, path: &Path) -> io::Error {
    io::Error::new(kind, format!("{what}: {}", path.display()))
}

fn not_found(path: &Path) -> io::Error {
    error(ErrorKind::NotFound, "No such file or directory", path)
}

impl Tree {
    fn load() -> Tree {
        let now = SystemTime::now();
        let mut nodes = BTreeMap::new();
        nodes.insert(root(), Node::Dir { modified: now });
        for (path, data, ms) in page::take() {
            let modified = UNIX_EPOCH + std::time::Duration::from_secs_f64(ms.max(0.0) / 1000.0);
            let node = match data {
                Some(data) => Node::File {
                    data: Rc::new(data),
                    modified,
                },
                None => Node::Dir { modified },
            };
            nodes.insert(norm(Path::new(&path)), node);
        }
        // Every file's folders, should the store have lost one.
        let missing: Vec<PathBuf> = nodes
            .keys()
            .flat_map(|p| p.ancestors().skip(1))
            .filter(|a| !nodes.contains_key(*a))
            .map(Path::to_path_buf)
            .collect();
        for dir in missing {
            nodes.insert(dir, Node::Dir { modified: now });
        }
        Tree {
            nodes,
            downloads: None,
            scratch: Vec::new(),
        }
    }

    fn get(&self, path: &Path) -> io::Result<&Node> {
        self.nodes.get(path).ok_or_else(|| not_found(path))
    }

    /// `Ok` when `path`'s folder is there.
    fn parent_is_dir(&self, path: &Path) -> io::Result<()> {
        let Some(parent) = path.parent() else {
            return Err(error(ErrorKind::AlreadyExists, "File exists", path));
        };
        match self.nodes.get(parent) {
            Some(Node::Dir { .. }) => Ok(()),
            Some(Node::File { .. }) => {
                Err(error(ErrorKind::NotADirectory, "Not a directory", path))
            }
            None => Err(not_found(path)),
        }
    }

    fn put_file(&mut self, path: &Path, data: Vec<u8>) -> io::Result<()> {
        self.parent_is_dir(path)?;
        if let Some(Node::Dir { .. }) = self.nodes.get(path) {
            return Err(error(ErrorKind::IsADirectory, "Is a directory", path));
        }
        let modified = SystemTime::now();
        self.keep(path, Some(&data), modified);
        self.offer_download(path, &data);
        self.nodes.insert(
            path.to_path_buf(),
            Node::File {
                data: Rc::new(data),
                modified,
            },
        );
        Ok(())
    }

    fn put_dir(&mut self, path: &Path) -> io::Result<()> {
        self.parent_is_dir(path)?;
        if self.nodes.contains_key(path) {
            return Err(error(ErrorKind::AlreadyExists, "File exists", path));
        }
        let modified = SystemTime::now();
        self.keep(path, None, modified);
        self.nodes
            .insert(path.to_path_buf(), Node::Dir { modified });
        Ok(())
    }

    fn remove(&mut self, path: &Path) -> Option<Node> {
        let node = self.nodes.remove(path)?;
        if self.kept(path) {
            page::remove(&path.to_string_lossy());
        }
        Some(node)
    }

    /// Whether the page keeps `path` (it isn't in a scratch folder).
    fn kept(&self, path: &Path) -> bool {
        !self.scratch.iter().any(|d| path.starts_with(d))
    }

    /// Hands the page a file's contents (or a folder: `None`) to keep, unless it is scratch.
    fn keep(&self, path: &Path, data: Option<&[u8]>, modified: SystemTime) {
        if self.kept(path) {
            page::put(&path.to_string_lossy(), data, millis(modified));
        }
    }

    /// `path` and everything in it, in path order (the folder first).
    fn subtree(&self, path: &Path) -> Vec<PathBuf> {
        self.nodes
            .range(path.to_path_buf()..)
            .take_while(|(p, _)| p.starts_with(path))
            .map(|(p, _)| p.clone())
            .collect()
    }

    fn offer_download(&self, path: &Path, data: &[u8]) {
        if let Some(dir) = &self.downloads
            && path.parent() == Some(dir.as_path())
            && let Some(name) = path.file_name()
        {
            page::download(&name.to_string_lossy(), data);
        }
    }
}

/// Files written straight into `dir` (not in its subfolders) are also handed to the browser as
/// downloads; `None` stops it.
pub fn set_downloads(dir: Option<PathBuf>) {
    with(|t| t.downloads = dir.map(|d| norm(&d)));
}

/// The folders whose files the page doesn't keep (in IndexedDB): they last as long as the page.
pub fn set_scratch(dirs: impl IntoIterator<Item = PathBuf>) {
    with(|t| t.scratch = dirs.into_iter().map(|d| norm(&d)).collect());
}

#[derive(Clone, Debug)]
pub struct Metadata {
    dir: bool,
    len: u64,
    modified: SystemTime,
}

#[allow(clippy::len_without_is_empty)]
impl Metadata {
    pub fn is_file(&self) -> bool {
        !self.dir
    }

    pub fn is_dir(&self) -> bool {
        self.dir
    }

    pub fn is_symlink(&self) -> bool {
        false
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn file_type(&self) -> FileType {
        FileType { dir: self.dir }
    }

    pub fn modified(&self) -> io::Result<SystemTime> {
        Ok(self.modified)
    }

    pub fn accessed(&self) -> io::Result<SystemTime> {
        Ok(self.modified)
    }

    pub fn created(&self) -> io::Result<SystemTime> {
        Ok(self.modified)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileType {
    dir: bool,
}

impl FileType {
    pub fn is_file(&self) -> bool {
        !self.dir
    }

    pub fn is_dir(&self) -> bool {
        self.dir
    }

    pub fn is_symlink(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct DirEntry {
    path: PathBuf,
    meta: Metadata,
}

impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn file_name(&self) -> OsString {
        self.path.file_name().unwrap_or_default().to_os_string()
    }

    pub fn file_type(&self) -> io::Result<FileType> {
        Ok(self.meta.file_type())
    }

    pub fn metadata(&self) -> io::Result<Metadata> {
        Ok(self.meta.clone())
    }
}

/// A folder's entries, in name order.
#[derive(Debug)]
pub struct ReadDir(std::vec::IntoIter<DirEntry>);

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(Ok)
    }
}

pub fn metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    let path = norm(path.as_ref());
    with(|t| t.get(&path).map(Node::meta))
}

pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let path = norm(path.as_ref());
    with(|t| t.get(&path).map(|_| path.clone()))
}

pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let path = norm(path.as_ref());
    with(|t| match t.get(&path)? {
        Node::File { data, .. } => Ok(data.to_vec()),
        Node::Dir { .. } => Err(error(ErrorKind::IsADirectory, "Is a directory", &path)),
    })
}

pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(path)?)
        .map_err(|_| io::Error::new(ErrorKind::InvalidData, "stream did not contain valid UTF-8"))
}

pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let path = norm(path.as_ref());
    with(|t| t.put_file(&path, contents.as_ref().to_vec()))
}

pub fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
    let path = norm(path.as_ref());
    with(|t| t.put_dir(&path))
}

pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = norm(path.as_ref());
    with(|t| {
        let mut missing: Vec<&Path> = Vec::new();
        for a in path.ancestors() {
            match t.nodes.get(a) {
                Some(Node::Dir { .. }) => break,
                Some(Node::File { .. }) => {
                    return Err(error(ErrorKind::AlreadyExists, "File exists", a));
                }
                None => missing.push(a),
            }
        }
        for dir in missing.into_iter().rev() {
            t.put_dir(dir)?;
        }
        Ok(())
    })
}

pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    let path = norm(path.as_ref());
    with(|t| match t.get(&path)? {
        Node::File { .. } => {
            t.remove(&path);
            Ok(())
        }
        Node::Dir { .. } => Err(error(ErrorKind::IsADirectory, "Is a directory", &path)),
    })
}

pub fn remove_dir(path: impl AsRef<Path>) -> io::Result<()> {
    let path = norm(path.as_ref());
    with(|t| match t.get(&path)? {
        Node::File { .. } => Err(error(ErrorKind::NotADirectory, "Not a directory", &path)),
        Node::Dir { .. } if t.subtree(&path).len() > 1 => Err(error(
            ErrorKind::DirectoryNotEmpty,
            "Directory not empty",
            &path,
        )),
        Node::Dir { .. } if path == root() => Err(error(
            ErrorKind::PermissionDenied,
            "Permission denied",
            &path,
        )),
        Node::Dir { .. } => {
            t.remove(&path);
            Ok(())
        }
    })
}

pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = norm(path.as_ref());
    with(|t| match t.get(&path)? {
        Node::File { .. } => Err(error(ErrorKind::NotADirectory, "Not a directory", &path)),
        Node::Dir { .. } => {
            for p in t.subtree(&path).into_iter().rev() {
                if p != root() {
                    t.remove(&p);
                }
            }
            Ok(())
        }
    })
}

pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let (from, to) = (norm(from.as_ref()), norm(to.as_ref()));
    with(|t| {
        let node = t.get(&from)?.clone();
        if from == to {
            return Ok(());
        }
        t.parent_is_dir(&to)?;
        match (&node, t.nodes.get(&to)) {
            (Node::File { .. }, Some(Node::Dir { .. })) => {
                return Err(error(ErrorKind::IsADirectory, "Is a directory", &to));
            }
            (Node::Dir { .. }, Some(Node::File { .. })) => {
                return Err(error(ErrorKind::NotADirectory, "Not a directory", &to));
            }
            (Node::Dir { .. }, Some(Node::Dir { .. })) if t.subtree(&to).len() > 1 => {
                return Err(error(
                    ErrorKind::DirectoryNotEmpty,
                    "Directory not empty",
                    &to,
                ));
            }
            (Node::Dir { .. }, _) if to.starts_with(&from) => {
                return Err(error(ErrorKind::InvalidInput, "Invalid argument", &to));
            }
            _ => {}
        }
        t.remove(&to);
        for old in t.subtree(&from) {
            // (`join("")` would end it in a separator.)
            let new = match old.strip_prefix(&from) {
                Ok(rest) if !rest.as_os_str().is_empty() => to.join(rest),
                _ => to.clone(),
            };
            let Some(node) = t.remove(&old) else { continue };
            match &node {
                Node::File { data, modified } => {
                    t.keep(&new, Some(data), *modified);
                    t.offer_download(&new, data);
                }
                Node::Dir { modified } => t.keep(&new, None, *modified),
            }
            t.nodes.insert(new, node);
        }
        Ok(())
    })
}

pub fn copy(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
    let data = read(from)?;
    let n = data.len() as u64;
    let to = norm(to.as_ref());
    with(|t| t.put_file(&to, data))?;
    Ok(n)
}

pub fn read_dir(path: impl AsRef<Path>) -> io::Result<ReadDir> {
    let path = norm(path.as_ref());
    with(|t| match t.get(&path)? {
        Node::File { .. } => Err(error(ErrorKind::NotADirectory, "Not a directory", &path)),
        Node::Dir { .. } => Ok(ReadDir(
            t.nodes
                .range(path.clone()..)
                .take_while(|(p, _)| p.starts_with(&path))
                .filter(|(p, _)| p.parent() == Some(path.as_path()))
                .map(|(p, n)| DirEntry {
                    path: p.clone(),
                    meta: n.meta(),
                })
                .collect::<Vec<_>>()
                .into_iter(),
        )),
    })
}

/// How to open a [`File`], as [`std::fs::OpenOptions`].
#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

impl OpenOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read(&mut self, on: bool) -> &mut Self {
        self.read = on;
        self
    }

    pub fn write(&mut self, on: bool) -> &mut Self {
        self.write = on;
        self
    }

    pub fn append(&mut self, on: bool) -> &mut Self {
        self.append = on;
        self
    }

    pub fn truncate(&mut self, on: bool) -> &mut Self {
        self.truncate = on;
        self
    }

    pub fn create(&mut self, on: bool) -> &mut Self {
        self.create = on;
        self
    }

    pub fn create_new(&mut self, on: bool) -> &mut Self {
        self.create_new = on;
        self
    }

    pub fn open(&self, path: impl AsRef<Path>) -> io::Result<File> {
        let path = norm(path.as_ref());
        let writes = self.write || self.append;
        let data = with(|t| -> io::Result<Vec<u8>> {
            match t.nodes.get(&path) {
                Some(Node::Dir { .. }) => {
                    Err(error(ErrorKind::IsADirectory, "Is a directory", &path))
                }
                Some(Node::File { .. }) if self.create_new => {
                    Err(error(ErrorKind::AlreadyExists, "File exists", &path))
                }
                Some(Node::File { .. }) if writes && self.truncate => Ok(Vec::new()),
                Some(Node::File { data, .. }) => Ok(data.to_vec()),
                None if writes && (self.create || self.create_new) => {
                    // There from the moment it is opened, as on a disk.
                    t.put_file(&path, Vec::new())?;
                    Ok(Vec::new())
                }
                None => Err(not_found(&path)),
            }
        })?;
        Ok(File {
            path,
            inner: RefCell::new(Open {
                pos: if self.append { data.len() } else { 0 },
                dirty: writes && self.truncate,
                data,
                writes,
                append: self.append,
            }),
        })
    }
}

/// An open file: its contents, written back to the tree when flushed, synced or dropped.
#[derive(Debug)]
pub struct File {
    path: PathBuf,
    inner: RefCell<Open>,
}

#[derive(Debug)]
struct Open {
    data: Vec<u8>,
    pos: usize,
    writes: bool,
    append: bool,
    dirty: bool,
}

impl File {
    pub fn open(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new().read(true).open(path)
    }

    pub fn create(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
    }

    pub fn create_new(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
    }

    pub fn options() -> OpenOptions {
        OpenOptions::new()
    }

    pub fn metadata(&self) -> io::Result<Metadata> {
        let modified = metadata(&self.path).map_or_else(|_| SystemTime::now(), |m| m.modified);
        Ok(Metadata {
            dir: false,
            len: self.inner.borrow().data.len() as u64,
            modified,
        })
    }

    pub fn set_len(&self, size: u64) -> io::Result<()> {
        let mut f = self.inner.borrow_mut();
        if !f.writes {
            return Err(error(
                ErrorKind::PermissionDenied,
                "Bad file descriptor",
                &self.path,
            ));
        }
        f.data.resize(size as usize, 0);
        f.dirty = true;
        Ok(())
    }

    pub fn sync_all(&self) -> io::Result<()> {
        self.commit()
    }

    pub fn sync_data(&self) -> io::Result<()> {
        self.commit()
    }

    /// Sets the file's modified time (after writing back what is pending).
    pub fn set_modified(&self, time: SystemTime) -> io::Result<()> {
        self.commit()?;
        with(|t| match t.nodes.get_mut(&self.path) {
            Some(Node::File { data, modified }) => {
                *modified = time;
                let data = data.clone();
                t.keep(&self.path, Some(&data), time);
                Ok(())
            }
            _ => Err(not_found(&self.path)),
        })
    }

    fn commit(&self) -> io::Result<()> {
        let mut f = self.inner.borrow_mut();
        if !f.dirty {
            return Ok(());
        }
        f.dirty = false;
        let data = f.data.clone();
        with(|t| t.put_file(&self.path, data))
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = self.commit();
    }
}

impl Read for &File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut f = self.inner.borrow_mut();
        let start = f.pos.min(f.data.len());
        let n = buf.len().min(f.data.len() - start);
        buf[..n].copy_from_slice(&f.data[start..start + n]);
        f.pos = start + n;
        Ok(n)
    }
}

impl Write for &File {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut f = self.inner.borrow_mut();
        if !f.writes {
            return Err(error(
                ErrorKind::PermissionDenied,
                "Bad file descriptor",
                &self.path,
            ));
        }
        if f.append {
            f.pos = f.data.len();
        }
        let end = f.pos + buf.len();
        if f.data.len() < end {
            f.data.resize(end, 0);
        }
        let pos = f.pos;
        f.data[pos..end].copy_from_slice(buf);
        f.pos = end;
        f.dirty = true;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.commit()
    }
}

impl Seek for &File {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let mut f = self.inner.borrow_mut();
        let pos = match to {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(d) => f.data.len() as i64 + d,
            SeekFrom::Current(d) => f.pos as i64 + d,
        };
        if pos < 0 {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "invalid seek to a negative position",
            ));
        }
        f.pos = pos as usize;
        Ok(pos as u64)
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self).flush()
    }
}

impl Seek for File {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        (&*self).seek(to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each test runs on a thread of its own, so with a tree of its own.

    #[test]
    fn files_and_folders() {
        create_dir_all("/a/b").unwrap();
        write("/a/b/c.txt", "hello").unwrap();
        assert_eq!(read_to_string("/a/b/c.txt").unwrap(), "hello");
        assert_eq!(read("a/./b/../b/c.txt").unwrap(), b"hello");
        assert!(metadata("/a/b").unwrap().is_dir());
        assert_eq!(metadata("/a/b/c.txt").unwrap().len(), 5);
        assert_eq!(
            write("/missing/c.txt", "x").unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert_eq!(
            write("/a/b", "x").unwrap_err().kind(),
            ErrorKind::IsADirectory
        );
        assert_eq!(
            create_dir("/a/b").unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );
        create_dir_all("/a/b").unwrap();
        assert!(create_dir_all("/a/b/c.txt/d").is_err());
        assert_eq!(
            remove_dir("/a").unwrap_err().kind(),
            ErrorKind::DirectoryNotEmpty
        );
        remove_file("/a/b/c.txt").unwrap();
        remove_dir("/a/b").unwrap();
        assert!(read_dir("/a").unwrap().next().is_none());
    }

    #[test]
    fn listing_is_one_level() {
        create_dir_all("/d/sub/deeper").unwrap();
        write("/d/x", "1").unwrap();
        write("/d/sub/y", "2").unwrap();
        // A sibling whose name starts with the folder's isn't in it.
        write("/dx", "3").unwrap();
        let names: Vec<_> = read_dir("/d")
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["sub", "x"]);
        let root: Vec<_> = read_dir("/").unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(root, [PathBuf::from("/d"), PathBuf::from("/dx")]);
    }

    #[test]
    fn rename_moves_folders() {
        create_dir_all("/s/in").unwrap();
        write("/s/in/f", "f").unwrap();
        write("/t", "t").unwrap();
        rename("/s", "/u").unwrap();
        assert!(metadata("/s").is_err());
        assert_eq!(read("/u/in/f").unwrap(), b"f");
        // A file replaces a file.
        rename("/t", "/u/in/f").unwrap();
        assert_eq!(read("/u/in/f").unwrap(), b"t");
        assert!(rename("/u", "/u/in/v").is_err());
        assert_eq!(
            rename("/u/in/f", "/u").unwrap_err().kind(),
            ErrorKind::IsADirectory
        );
        // Moved files are kept under their new names exactly.
        assert_eq!(
            read_dir("/u/in").unwrap().next().unwrap().unwrap().path(),
            Path::new("/u/in/f")
        );
        assert!(with(|t| t
            .nodes
            .keys()
            .all(|p| !p.to_string_lossy().ends_with('/') || *p == root())));
        remove_dir_all("/u").unwrap();
        assert!(metadata("/u/in/f").is_err());
        assert_eq!(
            copy("/missing", "/z").unwrap_err().kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn open_files_write_back() {
        create_dir("/f").unwrap();
        {
            let mut f = File::create("/f/a").unwrap();
            // There as soon as it is opened.
            assert!(metadata("/f/a").is_ok());
            f.write_all(b"hello world").unwrap();
            f.seek(SeekFrom::Start(6)).unwrap();
            f.write_all(b"there").unwrap();
        }
        assert_eq!(read("/f/a").unwrap(), b"hello there");
        let mut s = String::new();
        File::open("/f/a").unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "hello there");
        {
            let mut f = OpenOptions::new().append(true).open("/f/a").unwrap();
            f.write_all(b"!").unwrap();
        }
        assert_eq!(read("/f/a").unwrap(), b"hello there!");
        assert_eq!(
            File::create_new("/f/a").unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );
        assert!(File::open("/f/a").unwrap().write(b"x").is_err());
        assert_eq!(File::open("/f/b").unwrap_err().kind(), ErrorKind::NotFound);
        let then = UNIX_EPOCH + std::time::Duration::from_secs(1000);
        File::options()
            .write(true)
            .open("/f/a")
            .unwrap()
            .set_modified(then)
            .unwrap();
        assert_eq!(metadata("/f/a").unwrap().modified().unwrap(), then);
    }
}
