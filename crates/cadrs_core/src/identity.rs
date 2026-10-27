//! The identities kept on this computer: who the user is to collaborators. Each has a name, a
//! user id (what documents record as their owner and editor) and a key, whose public half is
//! its cadrs ID (`cadrs_net`). One is active; the first is the one cadrs always had, its key
//! the `identity.key` beside the documents, so an existing install keeps its cadrs ID.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The identities file in a store root.
pub const IDENTITIES_FILE: &str = "identities.ron";

/// The first identity's key, relative to the store root.
pub const DEFAULT_KEY: &str = "identity.key";

/// One identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// The user id documents record ("Owned by", "Modified by").
    pub id: String,
    /// What collaborators see.
    pub name: String,
    /// Its key file, relative to the store root.
    #[serde(default = "default_key")]
    pub key: String,
}

fn default_key() -> String {
    DEFAULT_KEY.into()
}

/// The identities on this computer, and which one is in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identities {
    pub list: Vec<Identity>,
    pub active: usize,
}

impl Identities {
    /// Just `first`, with the original key.
    pub fn new(first: Identity) -> Self {
        Self {
            list: vec![first],
            active: 0,
        }
    }

    /// The identities file in a store root.
    pub fn path(root: &Path) -> PathBuf {
        root.join(IDENTITIES_FILE)
    }

    /// Reads the identities from a store root; just `first` when there is no file or it can't
    /// be read.
    pub fn load(root: &Path, first: Identity) -> Self {
        cadrs_fs::read_to_string(Self::path(root))
            .ok()
            .and_then(|s| ron::from_str::<Self>(&s).ok())
            .filter(|i| !i.list.is_empty())
            .map(|mut i| {
                i.active = i.active.min(i.list.len() - 1);
                i
            })
            .unwrap_or_else(|| Self::new(first))
    }

    /// Writes the identities into a store root.
    pub fn save(&self, root: &Path) -> io::Result<()> {
        let text = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(io::Error::other)?;
        cadrs_fs::create_dir_all(root)?;
        let path = Self::path(root);
        let tmp = path.with_extension("tmp");
        cadrs_fs::write(&tmp, text)?;
        cadrs_fs::rename(&tmp, path)
    }

    pub fn active(&self) -> &Identity {
        &self.list[self.active]
    }

    /// The active identity's key file.
    pub fn key_path(&self, root: &Path) -> PathBuf {
        root.join(&self.active().key)
    }

    /// Adds an identity called `name` (its id made from the name, unlike any other) and
    /// returns its index. Its key is made the first time it goes online.
    pub fn add(&mut self, name: &str) -> usize {
        let base: String = name
            .trim()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let base = if base.is_empty() { "user".into() } else { base };
        let mut id = base.clone();
        let mut n = 2;
        while self.list.iter().any(|i| i.id == id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        self.list.push(Identity {
            key: format!("identities/{id}.key"),
            id,
            name: name.trim().to_string(),
        });
        self.list.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Identity {
        Identity {
            id: "rouan".into(),
            name: "Rouan".into(),
            key: DEFAULT_KEY.into(),
        }
    }

    #[test]
    fn a_new_identity_gets_its_own_id_and_key() {
        let mut ids = Identities::new(me());
        let a = ids.add("Studio Work");
        let b = ids.add("studio work!");
        assert_eq!(ids.list[a].id, "studio-work");
        assert_eq!(ids.list[b].id, "studio-work-2");
        assert_eq!(ids.list[a].key, "identities/studio-work.key");
        assert_eq!(ids.list[b].name, "studio work!");
        assert_eq!(ids.add("  "), 3);
        assert_eq!(ids.list[3].id, "user");
    }

    #[test]
    fn they_round_trip_through_the_store_root() {
        let dir = std::env::temp_dir().join(format!("cadrs-identities-{}", std::process::id()));
        let mut ids = Identities::new(me());
        ids.add("Work");
        ids.active = 1;
        ids.save(&dir).unwrap();
        let back = Identities::load(&dir, me());
        assert_eq!(back, ids);
        assert_eq!(back.key_path(&dir), dir.join("identities/work.key"));
        let _ = cadrs_fs::remove_dir_all(&dir);
        // No file: just the first, with the key cadrs always had.
        let none = Identities::load(&dir, me());
        assert_eq!(none.key_path(&dir), dir.join(DEFAULT_KEY));
    }
}
