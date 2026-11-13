//! Pending changelists: named groups of locally changed files.
//!
//! Git has no notion of changelists, so g4v keeps them as small, local
//! metadata (`.git/g4v/changelists.json`). They are never committed and never
//! affect what git itself does; they only decide which files get submitted or
//! shelved together. Files that are not in a numbered changelist belong to the
//! "default" changelist (id 0), exactly like Perforce.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

pub const DEFAULT_ID: u32 = 0;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Changelist {
    pub id: u32,
    pub description: String,
    pub files: BTreeSet<String>,
    #[serde(default)]
    pub created: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ChangelistStore {
    #[serde(default)]
    pub next_id: u32,
    #[serde(default)]
    pub lists: Vec<Changelist>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl ChangelistStore {
    pub fn file_path(meta_dir: &Path) -> PathBuf {
        meta_dir.join("changelists.json")
    }

    /// Loads the store; a missing or corrupt file yields an empty store
    /// (a corrupt file is kept aside as `.bak` so nothing is silently lost).
    pub fn load(meta_dir: &Path) -> Self {
        let path = Self::file_path(meta_dir);
        let mut store = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<ChangelistStore>(&text) {
                Ok(s) => s,
                Err(_) => {
                    let _ = std::fs::copy(&path, path.with_extension("json.bak"));
                    ChangelistStore::default()
                }
            },
            Err(_) => ChangelistStore::default(),
        };
        if store.next_id == 0 {
            store.next_id = store.lists.iter().map(|l| l.id).max().unwrap_or(0) + 1;
        }
        store.path = Some(path);
        store
    }

    /// Atomically writes the store (write temp file, then rename).
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    pub fn create(&mut self, description: &str) -> u32 {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        self.lists.push(Changelist {
            id,
            description: description.trim().to_string(),
            files: BTreeSet::new(),
            created: chrono::Local::now().timestamp(),
        });
        id
    }

    pub fn get(&self, id: u32) -> Option<&Changelist> {
        self.lists.iter().find(|l| l.id == id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Changelist> {
        self.lists.iter_mut().find(|l| l.id == id)
    }

    /// Deletes a changelist; its files fall back to the default changelist.
    pub fn delete(&mut self, id: u32) {
        self.lists.retain(|l| l.id != id);
    }

    /// Which changelist a file belongs to.
    pub fn list_of(&self, path: &str) -> u32 {
        self.lists
            .iter()
            .find(|l| l.files.contains(path))
            .map(|l| l.id)
            .unwrap_or(DEFAULT_ID)
    }

    /// Moves files into changelist `to` (0 = default).
    pub fn move_files<S: AsRef<str>>(&mut self, paths: &[S], to: u32) {
        for l in &mut self.lists {
            for p in paths {
                l.files.remove(p.as_ref());
            }
        }
        if to != DEFAULT_ID {
            if let Some(l) = self.get_mut(to) {
                for p in paths {
                    l.files.insert(p.as_ref().to_string());
                }
            }
        }
    }

    /// Drops files that are no longer pending. Returns true if anything changed.
    pub fn prune(&mut self, live: &HashSet<String>) -> bool {
        let mut changed = false;
        for l in &mut self.lists {
            let before = l.files.len();
            l.files.retain(|f| live.contains(f));
            changed |= l.files.len() != before;
        }
        changed
    }

    /// Files of changelist `id` among `pending` (default list = everything unassigned).
    pub fn files_in<'a>(&self, id: u32, pending: impl IntoIterator<Item = &'a String>) -> Vec<String> {
        pending
            .into_iter()
            .filter(|p| self.list_of(p) == id)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_and_prune() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = ChangelistStore::load(dir.path());
        let a = s.create("Hero textures");
        let b = s.create("Level tweaks");
        assert_ne!(a, b);
        s.move_files(&["x.png", "y.png"], a);
        assert_eq!(s.list_of("x.png"), a);
        s.move_files(&["x.png"], b);
        assert_eq!(s.list_of("x.png"), b);
        assert_eq!(s.list_of("y.png"), a);
        s.move_files(&["y.png"], DEFAULT_ID);
        assert_eq!(s.list_of("y.png"), DEFAULT_ID);
        let live: HashSet<String> = ["y.png".to_string()].into_iter().collect();
        assert!(s.prune(&live));
        assert!(s.get(b).unwrap().files.is_empty());
        s.save().unwrap();
        let s2 = ChangelistStore::load(dir.path());
        assert_eq!(s2.lists.len(), 2);
        assert!(s2.next_id > b);
    }

    #[test]
    fn corrupt_file_is_backed_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(ChangelistStore::file_path(dir.path()), "{not json").unwrap();
        let s = ChangelistStore::load(dir.path());
        assert!(s.lists.is_empty());
        assert!(dir.path().join("changelists.json.bak").exists());
    }
}
