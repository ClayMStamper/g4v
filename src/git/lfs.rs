//! Git LFS: exclusive file locks ("check out" in Perforce terms) and tracking.

use super::{GitError, GitResult, Repo};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lock {
    pub id: String,
    pub path: String,
    pub owner: String,
    pub locked_at: String,
    /// Held by the current user.
    pub ours: bool,
}

#[derive(Deserialize)]
struct RawOwner {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct RawLock {
    #[serde(default)]
    id: String,
    path: String,
    #[serde(default)]
    owner: Option<RawOwner>,
    #[serde(default)]
    locked_at: String,
}

#[derive(Deserialize)]
struct Verified {
    #[serde(default)]
    ours: Vec<RawLock>,
    #[serde(default)]
    theirs: Vec<RawLock>,
}

fn convert(r: RawLock, ours: bool) -> Lock {
    Lock {
        id: r.id,
        path: r.path,
        owner: r.owner.map(|o| o.name).unwrap_or_default(),
        locked_at: r.locked_at,
        ours,
    }
}

/// Parses `git lfs locks --verify --json`.
pub fn parse_verified_locks(json: &str) -> Option<Vec<Lock>> {
    let v: Verified = serde_json::from_str(json).ok()?;
    let mut out: Vec<Lock> = v.ours.into_iter().map(|r| convert(r, true)).collect();
    out.extend(v.theirs.into_iter().map(|r| convert(r, false)));
    Some(out)
}

/// Parses `git lfs locks --json` (no ownership info; compared by name).
pub fn parse_plain_locks(json: &str, me: Option<&str>) -> Option<Vec<Lock>> {
    let v: Vec<RawLock> = serde_json::from_str(json).ok()?;
    Some(
        v.into_iter()
            .map(|r| {
                let ours = me.is_some_and(|m| r.owner.as_ref().is_some_and(|o| o.name.eq_ignore_ascii_case(m)));
                convert(r, ours)
            })
            .collect(),
    )
}

/// Parses `git check-attr -z` output into path -> value.
pub fn parse_check_attr(bytes: &[u8]) -> HashMap<String, String> {
    let parts = super::split_nul_keep_empty(bytes);
    let mut out = HashMap::new();
    for chunk in parts.chunks(3) {
        if chunk.len() == 3 {
            out.insert(chunk[0].clone(), chunk[2].clone());
        }
    }
    out
}

impl Repo {
    /// Whether this repository is configured for Git LFS at all.
    pub fn uses_lfs(&self) -> bool {
        let attrs = std::fs::read_to_string(self.root().join(".gitattributes")).unwrap_or_default();
        attrs.contains("filter=lfs") || attrs.contains("lockable")
    }

    /// Whether `git lfs` is installed.
    pub fn lfs_installed(&self) -> bool {
        self.git().args(["lfs", "version"]).quiet().run().is_ok()
    }

    /// Lists all locks on the server.
    pub fn lfs_locks(&self) -> GitResult<Vec<Lock>> {
        // `--verify` tells us which locks are ours; not every server supports it.
        if let Ok(out) = self.git().args(["lfs", "locks", "--verify", "--json"]).quiet().run() {
            if let Some(v) = parse_verified_locks(&out.text()) {
                return Ok(v);
            }
        }
        let out = self.git().args(["lfs", "locks", "--json"]).quiet().run()?;
        let me = self.config_get("user.name");
        parse_plain_locks(&out.text(), me.as_deref()).ok_or_else(|| GitError {
            command: "git lfs locks --json".into(),
            code: None,
            stderr: "Could not read lock list from the server".into(),
            stdout: out.text(),
        })
    }

    /// Locks files. Returns (locked, failures).
    pub fn lfs_lock(&self, paths: &[String]) -> (Vec<String>, Vec<(String, String)>) {
        let mut ok = Vec::new();
        let mut failed = Vec::new();
        for p in paths {
            match self.git().args(["lfs", "lock", "--json", "--"]).arg(p).run() {
                Ok(_) => ok.push(p.clone()),
                Err(e) => failed.push((p.clone(), e.to_string())),
            }
        }
        (ok, failed)
    }

    /// Unlocks files. `force` breaks someone else's lock (admin).
    pub fn lfs_unlock(&self, paths: &[String], force: bool) -> (Vec<String>, Vec<(String, String)>) {
        let mut ok = Vec::new();
        let mut failed = Vec::new();
        for p in paths {
            let mut cmd = self.git().args(["lfs", "unlock", "--json"]);
            if force {
                cmd = cmd.arg("--force");
            }
            match cmd.arg("--").arg(p).run() {
                Ok(_) => ok.push(p.clone()),
                Err(e) => failed.push((p.clone(), e.to_string())),
            }
        }
        (ok, failed)
    }

    fn check_attr(&self, attr: &str, paths: &[String]) -> GitResult<HashMap<String, String>> {
        if paths.is_empty() {
            return Ok(HashMap::new());
        }
        let out = self
            .git()
            .args(["check-attr", "-z", "--stdin", attr])
            .stdin_paths(paths)
            .quiet()
            .run()?;
        Ok(parse_check_attr(&out.stdout))
    }

    /// Paths marked `lockable` in .gitattributes.
    pub fn lockable_paths(&self, paths: &[String]) -> GitResult<HashSet<String>> {
        Ok(self
            .check_attr("lockable", paths)?
            .into_iter()
            .filter(|(_, v)| v == "set" || v == "true")
            .map(|(k, _)| k)
            .collect())
    }

    /// Paths stored through Git LFS.
    pub fn lfs_tracked_paths(&self, paths: &[String]) -> GitResult<HashSet<String>> {
        Ok(self
            .check_attr("filter", paths)?
            .into_iter()
            .filter(|(_, v)| v == "lfs")
            .map(|(k, _)| k)
            .collect())
    }

    /// `git lfs track <pattern>` (optionally lockable). Edits .gitattributes.
    pub fn lfs_track(&self, pattern: &str, lockable: bool) -> GitResult<()> {
        let mut cmd = self.git().args(["lfs", "track"]);
        if lockable {
            cmd = cmd.arg("--lockable");
        }
        cmd.arg(pattern).run()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_verified() {
        let j = r#"{"ours":[{"id":"1","path":"a.psd","owner":{"name":"me"},"locked_at":"2024"}],"theirs":[{"id":"2","path":"b.psd","owner":{"name":"bob"},"locked_at":"2024"}]}"#;
        let v = parse_verified_locks(j).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v[0].ours);
        assert!(!v[1].ours);
        assert_eq!(v[1].owner, "bob");
    }

    #[test]
    fn parses_plain() {
        let j = r#"[{"id":"1","path":"a.psd","owner":{"name":"Me"},"locked_at":"x"}]"#;
        let v = parse_plain_locks(j, Some("me")).unwrap();
        assert!(v[0].ours);
    }

    #[test]
    fn parses_check_attr() {
        let raw = b"a.psd\0lockable\0set\0b.txt\0lockable\0unspecified\0";
        let m = parse_check_attr(raw);
        assert_eq!(m.get("a.psd").map(String::as_str), Some("set"));
        assert_eq!(m.get("b.txt").map(String::as_str), Some("unspecified"));
    }
}
