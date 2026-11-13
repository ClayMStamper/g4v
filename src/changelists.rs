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
    /// Goes up whenever file membership changes, so views can cache what they derive from it.
    #[serde(skip)]
    rev: u64,
    #[serde(skip)]
    pub load_error: Option<String>,
}

impl ChangelistStore {
    pub fn file_path(meta_dir: &Path) -> PathBuf {
        meta_dir.join("changelists.json")
    }

    /// Loads the store; a missing or corrupt file yields an empty store
    /// (a corrupt file is kept aside as `.bak` so nothing is silently lost).
    pub fn load(meta_dir: &Path) -> Self {
        let path = Self::file_path(meta_dir);
        // A virus scanner or backup tool can hold the file for a moment; an empty store would then be written back
        // over the real one, so only "the file does not exist" counts as "there are no changelists".
        let mut text = std::fs::read_to_string(&path);
        for _ in 0..5 {
            match &text {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    std::thread::sleep(std::time::Duration::from_millis(60));
                    text = std::fs::read_to_string(&path);
                }
                _ => break,
            }
        }
        let mut load_error = None;
        let mut store = match text {
            Ok(text) => match serde_json::from_str::<ChangelistStore>(&text) {
                Ok(s) => s,
                Err(_) => {
                    if let Err(e) = Self::set_aside(&path) {
                        load_error = Some(format!("Could not preserve unreadable changelists at {}: {e}. Saving is disabled; recover the file and reopen this repository.", path.display()));
                    }
                    ChangelistStore::default()
                }
            },
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    load_error = Some(format!(
                        "Could not read {}: {e}. Saving is disabled; restore access and reopen this repository.",
                        path.display()
                    ));
                }
                ChangelistStore::default()
            }
        };
        if store.next_id == 0 {
            store.next_id = store.lists.iter().map(|l| l.id).max().unwrap_or(0) + 1;
        }
        store.path = Some(path);
        store.load_error = load_error;
        store
    }

    /// Keeps an unreadable store next to the real one, without replacing an earlier copy.
    fn set_aside(path: &Path) -> std::io::Result<()> {
        let parent = path.parent().ok_or_else(|| std::io::Error::other("Missing parent directory"))?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        std::io::copy(&mut std::fs::File::open(path)?, tmp.as_file_mut())?;
        tmp.as_file().sync_all()?;
        for n in 0.. {
            let bak = path.with_extension(if n == 0 { "json.bak".into() } else { format!("json.bak{n}") });
            match tmp.persist_noclobber(bak) {
                Ok(_) => return std::fs::remove_file(path),
                Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => tmp = e.file,
                Err(e) => return Err(e.error),
            }
        }
        unreachable!()
    }

    /// Atomically writes the store (write temp file, flush it to disk, then rename).
    pub fn save(&self) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(e) = &self.load_error {
            return Err(std::io::Error::other(e.clone()));
        }
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let mut f = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        f.write_all(text.as_bytes())?;
        // Without this a power cut right after the rename can leave an empty file behind.
        f.as_file().sync_all()?;
        f.persist(path).map_err(|e| e.error)?;
        Ok(())
    }

    /// Changes whenever a file moves between changelists or a changelist is added or removed.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn create(&mut self, description: &str) -> u32 {
        self.rev += 1;
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
        self.rev += 1;
        self.lists.retain(|l| l.id != id);
    }

    /// Which changelist a file belongs to.
    pub fn list_of(&self, path: &str) -> u32 {
        self.lists.iter().find(|l| l.files.contains(path)).map(|l| l.id).unwrap_or(DEFAULT_ID)
    }

    /// Moves files into changelist `to` (0 = default).
    pub fn move_files<S: AsRef<str>>(&mut self, paths: &[S], to: u32) {
        self.rev += 1;
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
        if changed {
            self.rev += 1;
        }
        changed
    }

    /// Splits `pending` (in order) into the files of each changelist; unassigned files go to [`DEFAULT_ID`].
    /// One pass over the files, however many changelists there are.
    pub fn group<'a>(&self, pending: impl IntoIterator<Item = &'a String>) -> std::collections::HashMap<u32, Vec<String>> {
        let mut owner: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        for l in &self.lists {
            for f in &l.files {
                owner.entry(f.as_str()).or_insert(l.id);
            }
        }
        let mut out: std::collections::HashMap<u32, Vec<String>> = std::collections::HashMap::new();
        for p in pending {
            out.entry(owner.get(p.as_str()).copied().unwrap_or(DEFAULT_ID)).or_default().push(p.clone());
        }
        out
    }

    /// Files of changelist `id` among `pending` (default list = everything unassigned).
    pub fn files_in<'a>(&self, id: u32, pending: impl IntoIterator<Item = &'a String>) -> Vec<String> {
        pending.into_iter().filter(|p| self.list_of(p) == id).cloned().collect()
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
        // A second corrupt file must not replace the first backup.
        std::fs::write(ChangelistStore::file_path(dir.path()), "{still not json").unwrap();
        ChangelistStore::load(dir.path());
        assert_eq!(std::fs::read_to_string(dir.path().join("changelists.json.bak")).unwrap(), "{not json");
        assert_eq!(std::fs::read_to_string(dir.path().join("changelists.json.bak1")).unwrap(), "{still not json");
    }

    #[test]
    fn unreadable_store_cannot_be_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = ChangelistStore::file_path(dir.path());
        std::fs::create_dir(&path).unwrap();
        let mut store = ChangelistStore::load(dir.path());
        assert!(store.load_error.is_some());
        store.create("must not replace unreadable metadata");
        assert!(store.save().is_err());
        assert!(path.is_dir());
    }

    #[test]
    fn recovery_never_replaces_old_backups_and_save_replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..105 {
            let suffix = if n == 0 { "json.bak".into() } else { format!("json.bak{n}") };
            std::fs::write(ChangelistStore::file_path(dir.path()).with_extension(suffix), "older backup").unwrap();
        }
        std::fs::write(ChangelistStore::file_path(dir.path()), "{broken}").unwrap();
        let mut store = ChangelistStore::load(dir.path());
        assert!(store.load_error.is_none());
        assert_eq!(std::fs::read_to_string(dir.path().join("changelists.json.bak104")).unwrap(), "older backup");
        assert_eq!(std::fs::read_to_string(dir.path().join("changelists.json.bak105")).unwrap(), "{broken}");
        store.create("one");
        store.save().unwrap();
        store.create("two");
        store.save().unwrap();
        assert_eq!(ChangelistStore::load(dir.path()).lists.len(), 2);
    }

    #[test]
    fn grouping_matches_list_of_and_tracks_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = ChangelistStore::load(dir.path());
        let a = s.create("A");
        let b = s.create("B");
        s.move_files(&["2.png", "4.png"], a);
        s.move_files(&["3.png"], b);
        let pending: Vec<String> = (1..=5).map(|i| format!("{i}.png")).collect();
        let g = s.group(&pending);
        assert_eq!(g[&DEFAULT_ID], vec!["1.png", "5.png"]);
        assert_eq!(g[&a], vec!["2.png", "4.png"]);
        assert_eq!(g[&b], vec!["3.png"]);
        for id in [DEFAULT_ID, a, b] {
            assert_eq!(g[&id], s.files_in(id, &pending));
        }
        let before = s.rev();
        s.move_files(&["1.png"], b);
        assert_ne!(s.rev(), before);
        let live: HashSet<String> = pending.iter().cloned().collect();
        let before = s.rev();
        assert!(!s.prune(&live));
        assert_eq!(s.rev(), before);
    }
}
