//! Git LFS: exclusive file locks ("check out" in Perforce terms) and tracking.

use super::{GitError, GitResult, Repo};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

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
    Lock { id: r.id, path: r.path, owner: r.owner.map(|o| o.name).unwrap_or_default(), locked_at: r.locked_at, ours }
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

/// How many lock requests run at the same time. `git lfs lock` takes one file per call and each call is a round trip
/// to the server, so checking out a folder of a few hundred files one by one would take minutes.
const LOCK_PARALLELISM: usize = 8;

/// Runs `op` for every path on a few threads. Returns (succeeded, failures), both in the order of `paths`.
fn for_each_path(paths: &[String], op: impl Fn(&str) -> GitResult<()> + Sync) -> (Vec<String>, Vec<(String, String)>) {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<(), String>>>> = Mutex::new(vec![None; paths.len()]);
    std::thread::scope(|scope| {
        for _ in 0..LOCK_PARALLELISM.min(paths.len()) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(p) = paths.get(i) else { break };
                let r = op(p).map_err(|e| e.to_string());
                if let Ok(mut all) = results.lock() {
                    all[i] = Some(r);
                }
            });
        }
    });
    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for (p, r) in paths.iter().zip(results) {
        match r {
            Some(Ok(())) => ok.push(p.clone()),
            Some(Err(e)) => failed.push((p.clone(), e)),
            None => failed.push((p.clone(), "the request did not finish".to_string())),
        }
    }
    (ok, failed)
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
        // With `--json`, git-lfs prints an empty list and reports success even when the server could not be reached
        // or refused the request. An empty answer is therefore never taken at face value: it is confirmed with the
        // plain-text form of the same command, which does report errors. Otherwise "offline" would look exactly like
        // "nobody has anything checked out", and the guard against submitting files locked by someone else would
        // silently switch off.
        //
        // `--verify` tells us which locks are ours; not every server supports it.
        if let Ok(out) = self.git().args(["lfs", "locks", "--verify", "--json"]).quiet().run() {
            if let Some(v) = parse_verified_locks(&out.text()) {
                if !v.is_empty() || self.git().args(["lfs", "locks", "--verify"]).quiet().run().is_ok() {
                    return Ok(v);
                }
            }
        }
        let out = self.git().args(["lfs", "locks", "--json"]).quiet().run()?;
        let me = self.config_get("user.name");
        let locks = parse_plain_locks(&out.text(), me.as_deref()).ok_or_else(|| GitError {
            command: "git lfs locks --json".into(),
            code: None,
            stderr: "Could not read lock list from the server".into(),
            stdout: out.text(),
        })?;
        if locks.is_empty() {
            self.git().args(["lfs", "locks"]).quiet().run()?;
        }
        Ok(locks)
    }

    /// Locks files. Returns (locked, failures).
    pub fn lfs_lock(&self, paths: &[String]) -> (Vec<String>, Vec<(String, String)>) {
        for_each_path(paths, |p| self.git().args(["lfs", "lock", "--json", "--"]).arg(p).run().map(|_| ()))
    }

    /// Unlocks files. `force` breaks someone else's lock (admin).
    pub fn lfs_unlock(&self, paths: &[String], force: bool) -> (Vec<String>, Vec<(String, String)>) {
        for_each_path(paths, |p| {
            let mut cmd = self.git().args(["lfs", "unlock", "--json"]);
            if force {
                cmd = cmd.arg("--force");
            }
            cmd.arg("--").arg(p).run().map(|_| ())
        })
    }

    fn check_attr(&self, attr: &str, paths: &[String]) -> GitResult<HashMap<String, String>> {
        if paths.is_empty() {
            return Ok(HashMap::new());
        }
        let out = self.git().args(["check-attr", "-z", "--stdin", attr]).stdin_paths(paths).quiet().run()?;
        Ok(parse_check_attr(&out.stdout))
    }

    /// Paths marked `lockable` in .gitattributes.
    pub fn lockable_paths(&self, paths: &[String]) -> GitResult<HashSet<String>> {
        Ok(self.check_attr("lockable", paths)?.into_iter().filter(|(_, v)| v == "set" || v == "true").map(|(k, _)| k).collect())
    }

    /// Paths stored through Git LFS.
    pub fn lfs_tracked_paths(&self, paths: &[String]) -> GitResult<HashSet<String>> {
        Ok(self.check_attr("filter", paths)?.into_iter().filter(|(_, v)| v == "lfs").map(|(k, _)| k).collect())
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
    fn batches_keep_order_and_report_each_failure() {
        let paths: Vec<String> = (0..100).map(|i| format!("f{i}.psd")).collect();
        let (ok, failed) = for_each_path(&paths, |p| {
            if p.ends_with("7.psd") {
                Err(GitError { command: "lock".into(), code: Some(1), stderr: format!("{p} is locked"), stdout: String::new() })
            } else {
                Ok(())
            }
        });
        assert_eq!(ok.len(), 90);
        assert_eq!(failed.len(), 10);
        assert_eq!(ok[0], "f0.psd");
        assert_eq!(failed[0], ("f7.psd".to_string(), "f7.psd is locked".to_string()));
        assert_eq!(for_each_path(&[], |_| Ok(())), (vec![], vec![]));
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
