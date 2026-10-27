//! A document's comments, as Onshape's Comments panel (`comments_on_workspaces.htm`).
//!
//! - A [`Comment`] is a thread's first message or a reply ([`Comment::parent`]). It may be
//!   tagged to a feature or a tab ([`Tag`]), mention people with `@name`, and be a **task**
//!   assigned to someone, who resolves it.
//! - Comments are not part of the document: they are kept beside it (`comments.ron` in its
//!   folder) and are not in its history, so undo, versions and restore leave them alone.
//! - Every change is a [`CommentOp`]. On a shared document the owner applies each in turn and
//!   passes it on, so every copy applies the same ops in the same order ([`Comments::apply`]);
//!   only a comment's author edits or deletes it, and a task is resolved by its author, its
//!   assignee or the document's owner.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ids::{ElementId, FeatureId};
use crate::library::Timestamp;

pub const COMMENTS_FILE: &str = "comments.ron";

/// What a comment is tagged to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tag {
    Element(ElementId),
    Feature(ElementId, FeatureId),
}

/// One message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: uuid::Uuid,
    /// The thread it replies to; `None` for a thread's first message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<uuid::Uuid>,
    /// Who wrote it: a cadrs ID (or the local user's id on an unshared document).
    pub author: String,
    pub author_name: String,
    pub time: Timestamp,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<Tag>,
    /// A task's assignee: their id and name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<(String, String)>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resolved: bool,
    /// When it was last edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited: Option<Timestamp>,
}

impl Comment {
    pub fn is_task(&self) -> bool {
        self.assignee.is_some()
    }

    /// The names it mentions (`@Name`, a word after the @).
    pub fn mentions(&self) -> Vec<String> {
        mentions(&self.text)
    }
}

/// The words after each `@` in `text`.
pub fn mentions(text: &str) -> Vec<String> {
    text.split('@')
        .skip(1)
        .filter_map(|rest| {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
                .collect();
            let name = name.trim_end_matches('.').to_string();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// A change to the comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommentOp {
    Add(Comment),
    Edit {
        id: uuid::Uuid,
        text: String,
        time: Timestamp,
    },
    Delete {
        id: uuid::Uuid,
    },
    Resolve {
        id: uuid::Uuid,
        resolved: bool,
    },
}

/// Why an op was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentError {
    NotFound,
    NotYours,
}

impl std::fmt::Display for CommentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CommentError::NotFound => "That comment is gone",
            CommentError::NotYours => "Only its author can change that comment",
        })
    }
}

/// A document's comments, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comments(pub Vec<Comment>);

impl Comments {
    pub fn load(dir: &Path) -> Self {
        cadrs_fs::read_to_string(dir.join(COMMENTS_FILE))
            .ok()
            .and_then(|t| ron::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        cadrs_fs::create_dir_all(dir)?;
        let path = dir.join(COMMENTS_FILE);
        if self.0.is_empty() {
            return match cadrs_fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let text =
            ron::ser::to_string_pretty(self, Default::default()).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("tmp");
        cadrs_fs::write(&tmp, text)?;
        cadrs_fs::rename(&tmp, path)
    }

    pub fn get(&self, id: uuid::Uuid) -> Option<&Comment> {
        self.0.iter().find(|c| c.id == id)
    }

    /// The threads' first messages, newest first.
    pub fn threads(&self) -> impl Iterator<Item = &Comment> {
        self.0.iter().rev().filter(|c| c.parent.is_none())
    }

    /// A thread's replies, oldest first.
    pub fn replies(&self, thread: uuid::Uuid) -> impl Iterator<Item = &Comment> {
        self.0.iter().filter(move |c| c.parent == Some(thread))
    }

    /// Applies `op`, made by `by`; `owner` is the document owner's id (who may resolve any
    /// task). Adding a comment that is already there replaces it (an op seen twice).
    pub fn apply(&mut self, op: &CommentOp, by: &str, owner: &str) -> Result<(), CommentError> {
        match op {
            CommentOp::Add(c) => {
                match self.0.iter_mut().find(|x| x.id == c.id) {
                    Some(x) if x.author != by => return Err(CommentError::NotYours),
                    Some(x) => *x = c.clone(),
                    None => self.0.push(c.clone()),
                }
                Ok(())
            }
            CommentOp::Edit { id, text, time } => {
                let c = self
                    .0
                    .iter_mut()
                    .find(|c| c.id == *id)
                    .ok_or(CommentError::NotFound)?;
                if c.author != by {
                    return Err(CommentError::NotYours);
                }
                c.text = text.clone();
                c.edited = Some(*time);
                Ok(())
            }
            CommentOp::Delete { id } => {
                let c = self.get(*id).ok_or(CommentError::NotFound)?;
                if c.author != by && by != owner {
                    return Err(CommentError::NotYours);
                }
                // A thread goes with its replies.
                self.0.retain(|c| c.id != *id && c.parent != Some(*id));
                Ok(())
            }
            CommentOp::Resolve { id, resolved } => {
                let c = self
                    .0
                    .iter_mut()
                    .find(|c| c.id == *id)
                    .ok_or(CommentError::NotFound)?;
                let assignee = c.assignee.as_ref().is_some_and(|(a, _)| a == by);
                if c.author != by && !assignee && by != owner {
                    return Err(CommentError::NotYours);
                }
                c.resolved = *resolved;
                Ok(())
            }
        }
    }

    /// Open tasks assigned to `user`.
    pub fn tasks_for(&self, user: &str) -> usize {
        self.0
            .iter()
            .filter(|c| !c.resolved && c.assignee.as_ref().is_some_and(|(a, _)| a == user))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(author: &str, text: &str) -> Comment {
        Comment {
            id: uuid::Uuid::new_v4(),
            parent: None,
            author: author.into(),
            author_name: author.to_uppercase(),
            time: 1,
            text: text.into(),
            tag: None,
            assignee: None,
            resolved: false,
            edited: None,
        }
    }

    #[test]
    fn mentions_are_the_words_after_at() {
        assert_eq!(mentions("@Ann can you check this, @bo.b?"), ["Ann", "bo.b"]);
        assert!(mentions("mail me at x @ y").is_empty());
    }

    #[test]
    fn only_the_author_edits_and_a_thread_goes_with_its_replies() {
        let mut cs = Comments::default();
        let a = comment("a", "first");
        let mut r = comment("b", "reply");
        r.parent = Some(a.id);
        cs.apply(&CommentOp::Add(a.clone()), "a", "o").unwrap();
        cs.apply(&CommentOp::Add(r.clone()), "b", "o").unwrap();
        // Seen twice: still one.
        cs.apply(&CommentOp::Add(r.clone()), "b", "o").unwrap();
        assert_eq!(cs.0.len(), 2);
        let edit = CommentOp::Edit {
            id: a.id,
            text: "changed".into(),
            time: 2,
        };
        assert_eq!(cs.apply(&edit, "b", "o"), Err(CommentError::NotYours));
        cs.apply(&edit, "a", "o").unwrap();
        assert_eq!(cs.get(a.id).unwrap().text, "changed");
        assert_eq!(cs.replies(a.id).count(), 1);
        // The owner may delete anything.
        cs.apply(&CommentOp::Delete { id: a.id }, "o", "o").unwrap();
        assert!(cs.0.is_empty());
    }

    #[test]
    fn a_task_is_resolved_by_its_assignee() {
        let mut cs = Comments::default();
        let mut t = comment("a", "fix the fillet @Bo");
        t.assignee = Some(("b".into(), "Bo".into()));
        cs.apply(&CommentOp::Add(t.clone()), "a", "o").unwrap();
        assert_eq!(cs.tasks_for("b"), 1);
        let resolve = CommentOp::Resolve {
            id: t.id,
            resolved: true,
        };
        assert_eq!(cs.apply(&resolve, "c", "o"), Err(CommentError::NotYours));
        cs.apply(&resolve, "b", "o").unwrap();
        assert_eq!(cs.tasks_for("b"), 0);
    }

    #[test]
    fn comments_round_trip_and_none_leaves_no_file() {
        let dir = std::env::temp_dir().join(format!("cadrs-comments-{}", uuid::Uuid::new_v4()));
        let mut cs = Comments::default();
        cs.0.push(comment("a", "hello"));
        cs.save(&dir).unwrap();
        assert_eq!(Comments::load(&dir), cs);
        Comments::default().save(&dir).unwrap();
        assert!(!dir.join(COMMENTS_FILE).exists());
        let _ = cadrs_fs::remove_dir_all(dir);
    }
}
