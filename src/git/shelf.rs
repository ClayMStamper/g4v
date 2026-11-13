//! Shelves: Perforce-style "save my changes on the side" built on plain git objects.
//!
//! A shelf is an ordinary commit object whose parent is the HEAD at shelving
//! time and whose tree is HEAD plus the shelved files. It is referenced by
//! `refs/g4v/shelves/<user>/<timestamp>` and never appears on any branch.
//!
//! Compared to `git stash`:
//! * any subset of files can be shelved (a whole changelist, or one file),
//! * shelving does not touch the working copy unless asked to,
//! * shelves can be pushed to the server so teammates can review/unshelve,
//! * Git LFS content is preserved (the clean filter runs while shelving and
//!   the LFS pre-push hook uploads the objects when sharing).

use super::history::{parse_name_status, ChangedFile};
use super::{GitError, GitResult, Repo};
use std::path::PathBuf;

pub const LOCAL_PREFIX: &str = "refs/g4v/shelves/";
pub const REMOTE_PREFIX: &str = "refs/g4v/remote-shelves/";
pub const BACKUP_TAG: &str = "[auto-backup] ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shelf {
    pub refname: String,
    /// Name below the prefix, e.g. `alice/20240101-120000-123`.
    pub name: String,
    pub id: String,
    pub base: Option<String>,
    pub author: String,
    pub time: i64,
    pub description: String,
    /// Set for shelves fetched from a remote (`origin`, ...).
    pub remote: Option<String>,
    /// A local shelf that also exists, identically, on the remote.
    pub shared: bool,
}

impl Shelf {
    pub fn is_backup(&self) -> bool {
        self.description.starts_with(BACKUP_TAG.trim_end())
    }
    pub fn display_name(&self) -> String {
        self.description.trim_start_matches(BACKUP_TAG).to_string()
    }
}

/// Makes a string safe for a ref path component.
pub fn slug(s: &str) -> String {
    let mut out: String =
        s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '-' }).collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "user".into()
    } else {
        out.chars().take(40).collect()
    }
}

struct TempIndex(PathBuf);
impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("lock"));
    }
}

impl Repo {
    /// Environment overrides so that shelving works even before the user has set up an identity.
    pub(crate) fn fallback_identity_env(&self) -> Vec<(&'static str, String)> {
        let mut env = Vec::new();
        if self.config_get("user.name").is_none() {
            env.push(("GIT_AUTHOR_NAME", "g4v user".to_string()));
            env.push(("GIT_COMMITTER_NAME", "g4v user".to_string()));
        }
        if self.config_get("user.email").is_none() {
            env.push(("GIT_AUTHOR_EMAIL", "g4v@localhost".to_string()));
            env.push(("GIT_COMMITTER_EMAIL", "g4v@localhost".to_string()));
        }
        env
    }

    /// Shelves the given paths (as they are on disk right now). The working copy is not modified.
    pub fn create_shelf(&self, paths: &[String], description: &str) -> GitResult<Shelf> {
        if paths.is_empty() {
            return Err(GitError {
                command: "shelve".into(),
                code: None,
                stderr: "There are no files to shelve.".into(),
                stdout: String::new(),
            });
        }
        std::fs::create_dir_all(self.meta_dir()).ok();
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let idx = TempIndex(self.meta_dir().join(format!("shelve-index-{}-{nanos}", std::process::id())));
        let idx_path = idx.0.as_os_str().to_os_string();

        let head = self.run(&["rev-parse", "--verify", "--quiet", "HEAD"]).ok().filter(|s| !s.is_empty());
        let mut read_tree = self.git().env("GIT_INDEX_FILE", &idx_path).arg("read-tree");
        read_tree = match &head {
            Some(h) => read_tree.arg(h),
            None => read_tree.arg("--empty"),
        };
        read_tree.run()?;

        self.git()
            .env("GIT_INDEX_FILE", &idx_path)
            .args(["add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"])
            .stdin_paths(paths)
            .run()?;
        let tree = self.git().env("GIT_INDEX_FILE", &idx_path).arg("write-tree").run()?.text().trim().to_string();

        let user = self.config_get("user.name").unwrap_or_else(|| "user".into());
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%3f").to_string();
        let name = format!("{}/{}", slug(&user), stamp);
        let message = if description.trim().is_empty() { "Shelved changes".to_string() } else { description.trim().to_string() };

        let mut ct = self.git().arg("commit-tree").arg(&tree);
        if let Some(h) = &head {
            ct = ct.arg("-p").arg(h);
        }
        for (k, v) in self.fallback_identity_env() {
            ct = ct.env(k, v);
        }
        let id = ct.args(["-F", "-"]).stdin(message.clone().into_bytes()).run()?.text().trim().to_string();
        let refname = format!("{LOCAL_PREFIX}{name}");
        self.git().args(["update-ref", "-m", "g4v: shelve"]).arg(&refname).arg(&id).run()?;
        Ok(Shelf {
            refname,
            name,
            id,
            base: head,
            author: user,
            time: chrono::Local::now().timestamp(),
            description: message,
            remote: None,
            shared: false,
        })
    }

    /// Lists local and fetched remote shelves, newest first.
    pub fn list_shelves(&self) -> GitResult<Vec<Shelf>> {
        let out = self
            .git()
            .args([
                "for-each-ref",
                "--format=%(refname)%1f%(objectname)%1f%(creatordate:unix)%1f%(authorname)%1f%(parent)%1f%(subject)",
                "refs/g4v/shelves",
                "refs/g4v/remote-shelves",
            ])
            .quiet()
            .run()?;
        let mut shelves: Vec<Shelf> = Vec::new();
        for line in out.text().lines() {
            let f: Vec<&str> = line.splitn(6, '\u{1f}').collect();
            if f.len() < 6 {
                continue;
            }
            let refname = f[0].to_string();
            let (name, remote) = if let Some(n) = refname.strip_prefix(LOCAL_PREFIX) {
                (n.to_string(), None)
            } else if let Some(rest) = refname.strip_prefix(REMOTE_PREFIX) {
                match rest.split_once('/') {
                    Some((r, n)) => (n.to_string(), Some(r.to_string())),
                    None => continue,
                }
            } else {
                continue;
            };
            shelves.push(Shelf {
                refname,
                name,
                id: f[1].to_string(),
                time: f[2].parse().unwrap_or(0),
                author: f[3].to_string(),
                base: f[4].split_whitespace().next().map(str::to_string),
                description: f[5].to_string(),
                remote,
                shared: false,
            });
        }
        // Mark local shelves that are mirrored on a remote, and hide the duplicate remote entry.
        let remote_copies: Vec<(String, String)> =
            shelves.iter().filter(|s| s.remote.is_some()).map(|s| (s.name.clone(), s.id.clone())).collect();
        for s in shelves.iter_mut().filter(|s| s.remote.is_none()) {
            s.shared = remote_copies.iter().any(|(n, id)| *n == s.name && *id == s.id);
        }
        let local_names: Vec<String> = shelves.iter().filter(|s| s.remote.is_none()).map(|s| s.name.clone()).collect();
        shelves.retain(|s| s.remote.is_none() || !local_names.contains(&s.name));
        shelves.sort_by(|a, b| b.time.cmp(&a.time).then(b.name.cmp(&a.name)));
        Ok(shelves)
    }

    pub fn shelf_files(&self, shelf: &Shelf) -> GitResult<Vec<ChangedFile>> {
        let out = self
            .git()
            .args(["diff-tree", "--no-commit-id", "-r", "-z", "--name-status", "--root", "--no-renames"])
            .arg(&shelf.id)
            .quiet()
            .run()?;
        Ok(parse_name_status(&out.stdout))
    }

    /// Writes shelved file versions into the working copy (overwriting local
    /// versions of those files). Returns the paths that were written/removed.
    /// Callers should back up overlapping local edits first.
    pub fn unshelve(&self, shelf: &Shelf, only: Option<&[String]>) -> GitResult<Vec<String>> {
        let files = self.shelf_files(shelf)?;
        let selected: Vec<&ChangedFile> = files.iter().filter(|f| only.is_none_or(|o| o.contains(&f.path))).collect();
        let restore: Vec<String> = selected.iter().filter(|f| f.status != 'D').map(|f| f.path.clone()).collect();
        let delete: Vec<String> = selected.iter().filter(|f| f.status == 'D').map(|f| f.path.clone()).collect();
        if !restore.is_empty() {
            self.git()
                .args(["restore", "--worktree", "--overlay", "--pathspec-from-file=-", "--pathspec-file-nul"])
                .arg(format!("--source={}", shelf.id))
                .stdin_paths(&restore)
                .run()?;
        }
        for p in &delete {
            let abs = self.abs(p);
            if abs.is_file() {
                std::fs::remove_file(&abs).map_err(|e| GitError {
                    command: format!("delete {p}"),
                    code: None,
                    stderr: format!("Could not delete {p}: {e}"),
                    stdout: String::new(),
                })?;
            }
        }
        let mut all = restore;
        all.extend(delete);
        Ok(all)
    }

    pub fn delete_shelf(&self, shelf: &Shelf) -> GitResult<()> {
        self.git().args(["update-ref", "-d"]).arg(&shelf.refname).run()?;
        Ok(())
    }

    /// Pushes a local shelf so teammates can see it.
    pub fn share_shelf(&self, shelf: &Shelf, remote: &str) -> GitResult<()> {
        self.git().arg("push").arg(remote).arg(format!("{}:{}{}", shelf.refname, LOCAL_PREFIX, shelf.name)).run()?;
        // Mirror locally so it shows as shared without a refetch.
        self.git().arg("update-ref").arg(format!("{REMOTE_PREFIX}{remote}/{}", shelf.name)).arg(&shelf.id).run()?;
        Ok(())
    }

    /// Removes a shelf from the server.
    pub fn unshare_shelf(&self, name: &str, remote: &str) -> GitResult<()> {
        self.git().arg("push").arg(remote).arg("--delete").arg(format!("{LOCAL_PREFIX}{name}")).run()?;
        let _ = self.git().args(["update-ref", "-d"]).arg(format!("{REMOTE_PREFIX}{remote}/{name}")).run();
        Ok(())
    }

    /// Copies a teammate's shelf into my local shelves.
    pub fn fetch_shelves(&self, remote: &str) -> GitResult<()> {
        self.git()
            .args(["fetch", "--prune", "--no-tags", "--quiet"])
            .arg(remote)
            .arg(format!("+{LOCAL_PREFIX}*:{REMOTE_PREFIX}{remote}/*"))
            .quiet()
            .run()?;
        Ok(())
    }

    /// Deletes automatic backup shelves older than `days`.
    pub fn prune_backups(&self, days: i64) -> GitResult<usize> {
        let cutoff = chrono::Local::now().timestamp() - days * 86_400;
        let mut n = 0;
        for s in self.list_shelves()? {
            if s.remote.is_none() && s.is_backup() && s.time < cutoff && !s.shared {
                self.delete_shelf(&s)?;
                n += 1;
            }
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slugs() {
        assert_eq!(slug("Jane Doe"), "jane-doe");
        assert_eq!(slug("  "), "user");
        assert_eq!(slug("a//b"), "a-b");
    }
}
