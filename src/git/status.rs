//! Working copy status (`git status --porcelain=v2 -z`).

use super::{GitResult, Repo};

/// The user-facing kind of change, phrased the way Perforce users think of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChangeKind {
    /// New file (staged add or untracked).
    Add,
    Edit,
    Delete,
    /// Rename/move detected by git (only for staged renames).
    Move,
    /// File type changed (e.g. file <-> symlink).
    TypeChange,
    /// Unresolved merge conflict.
    Conflict,
}

impl ChangeKind {
    pub fn label(self) -> &'static str {
        match self {
            ChangeKind::Add => "add",
            ChangeKind::Edit => "edit",
            ChangeKind::Delete => "delete",
            ChangeKind::Move => "move",
            ChangeKind::TypeChange => "type change",
            ChangeKind::Conflict => "conflict",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatus {
    /// Repository relative path, `/` separated.
    pub path: String,
    /// Original path for renames.
    pub orig_path: Option<String>,
    pub kind: ChangeKind,
    /// Index status char (porcelain X).
    pub x: char,
    /// Worktree status char (porcelain Y).
    pub y: char,
    pub untracked: bool,
    pub submodule: bool,
}

impl FileStatus {
    /// For conflicts: does "our" side (HEAD) still have the file?
    pub fn conflict_ours_exists(&self) -> bool {
        matches!(self.x, 'A' | 'U')
    }
    /// For conflicts: does "their" side still have the file?
    pub fn conflict_theirs_exists(&self) -> bool {
        matches!(self.y, 'A' | 'U')
    }
    /// Whether the file is present in HEAD (i.e. reverting means restoring rather than deleting).
    pub fn in_head(&self) -> bool {
        !self.untracked && self.x != 'A' && self.kind != ChangeKind::Add
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchStatus {
    /// Current commit, `None` before the first commit.
    pub oid: Option<String>,
    /// Branch name, `None` when detached.
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RepoOp {
    #[default]
    None,
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

impl RepoOp {
    pub fn label(self) -> &'static str {
        match self {
            RepoOp::None => "",
            RepoOp::Merge => "merge",
            RepoOp::Rebase => "rebase",
            RepoOp::CherryPick => "cherry-pick",
            RepoOp::Revert => "revert",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct StatusSnapshot {
    pub branch: BranchStatus,
    pub files: Vec<FileStatus>,
    pub stash_count: u32,
    pub op: RepoOp,
    /// `.git/index.lock` exists (a crashed git process, or one running right now).
    pub index_lock: bool,
    /// The branch being rebased (HEAD is detached while a rebase is in progress).
    pub rebase_branch: Option<String>,
}

impl StatusSnapshot {
    pub fn conflicts(&self) -> impl Iterator<Item = &FileStatus> {
        self.files.iter().filter(|f| f.kind == ChangeKind::Conflict)
    }
    pub fn has_conflicts(&self) -> bool {
        self.conflicts().next().is_some()
    }
    pub fn find(&self, path: &str) -> Option<&FileStatus> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// Parses `git status --porcelain=v2 -z --branch --show-stash` output.
pub fn parse_porcelain_v2(bytes: &[u8]) -> (BranchStatus, Vec<FileStatus>, u32) {
    let mut branch = BranchStatus::default();
    let mut files = Vec::new();
    let mut stash = 0;
    let mut records = bytes.split(|b| *b == 0).filter(|r| !r.is_empty());
    while let Some(rec) = records.next() {
        let line = String::from_utf8_lossy(rec);
        let mut chars = line.chars();
        match chars.next() {
            Some('#') => {
                let rest = &line[2.min(line.len())..];
                if let Some(v) = rest.strip_prefix("branch.oid ") {
                    branch.oid = (v != "(initial)").then(|| v.to_string());
                } else if let Some(v) = rest.strip_prefix("branch.head ") {
                    branch.head = (v != "(detached)").then(|| v.to_string());
                } else if let Some(v) = rest.strip_prefix("branch.upstream ") {
                    branch.upstream = Some(v.to_string());
                } else if let Some(v) = rest.strip_prefix("branch.ab ") {
                    for part in v.split_whitespace() {
                        if let Some(n) = part.strip_prefix('+') {
                            branch.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            branch.behind = n.parse().unwrap_or(0);
                        }
                    }
                } else if let Some(v) = rest.strip_prefix("stash ") {
                    stash = v.trim().parse().unwrap_or(0);
                }
            }
            Some('1') => {
                // 1 XY sub mH mI mW hH hI path
                let parts: Vec<&str> = line.splitn(9, ' ').collect();
                if parts.len() == 9 {
                    files.push(ordinary(parts[1], parts[2], parts[8].to_string(), None));
                }
            }
            Some('2') => {
                // 2 XY sub mH mI mW hH hI Xscore path \0 origPath
                let parts: Vec<&str> = line.splitn(10, ' ').collect();
                let orig = records.next().map(|r| String::from_utf8_lossy(r).into_owned());
                if parts.len() == 10 {
                    let mut f = ordinary(parts[1], parts[2], parts[9].to_string(), orig);
                    if parts[8].starts_with('R') {
                        f.kind = ChangeKind::Move;
                    }
                    files.push(f);
                }
            }
            Some('u') => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                let parts: Vec<&str> = line.splitn(11, ' ').collect();
                if parts.len() == 11 {
                    let xy: Vec<char> = parts[1].chars().collect();
                    files.push(FileStatus {
                        path: parts[10].to_string(),
                        orig_path: None,
                        kind: ChangeKind::Conflict,
                        x: xy.first().copied().unwrap_or('U'),
                        y: xy.get(1).copied().unwrap_or('U'),
                        untracked: false,
                        submodule: parts[2].starts_with('S'),
                    });
                }
            }
            Some('?') => {
                let path = line[2.min(line.len())..].to_string();
                files.push(FileStatus { path, orig_path: None, kind: ChangeKind::Add, x: '?', y: '?', untracked: true, submodule: false });
            }
            _ => {}
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    (branch, files, stash)
}

fn ordinary(xy: &str, sub: &str, path: String, orig: Option<String>) -> FileStatus {
    let mut c = xy.chars();
    let x = c.next().unwrap_or('.');
    let y = c.next().unwrap_or('.');
    let kind = if y == 'D' && x == 'A' {
        // Added to the index then deleted from disk: effectively nothing; show as delete.
        ChangeKind::Delete
    } else if x == 'D' || y == 'D' {
        ChangeKind::Delete
    } else if x == 'A' {
        ChangeKind::Add
    } else if x == 'R' || x == 'C' {
        ChangeKind::Move
    } else if x == 'T' || y == 'T' {
        ChangeKind::TypeChange
    } else {
        ChangeKind::Edit
    };
    FileStatus { path, orig_path: orig, kind, x, y, untracked: false, submodule: sub.starts_with('S') }
}

impl Repo {
    pub fn status(&self) -> GitResult<StatusSnapshot> {
        let out = self
            .git()
            .args([
                "--no-optional-locks",
                "status",
                "--porcelain=v2",
                "-z",
                "--branch",
                "--show-stash",
                "--untracked-files=all",
                "--ignore-submodules=dirty",
            ])
            .env("GIT_OPTIONAL_LOCKS", "0")
            .quiet()
            .run()?;
        let (branch, files, stash_count) = parse_porcelain_v2(&out.stdout);
        Ok(StatusSnapshot {
            branch,
            files,
            stash_count,
            op: self.current_op(),
            index_lock: self.git_dir().join("index.lock").exists(),
            rebase_branch: self.rebase_branch(),
        })
    }

    fn rebase_branch(&self) -> Option<String> {
        ["rebase-merge", "rebase-apply"].iter().find_map(|d| {
            let s = std::fs::read_to_string(self.git_dir().join(d).join("head-name")).ok()?;
            Some(s.trim().trim_start_matches("refs/heads/").to_string())
        })
    }

    /// Detects an in-progress merge / rebase / cherry-pick.
    pub fn current_op(&self) -> RepoOp {
        let g = self.git_dir();
        if g.join("rebase-merge").exists() || g.join("rebase-apply").exists() {
            RepoOp::Rebase
        } else if g.join("MERGE_HEAD").exists() {
            RepoOp::Merge
        } else if g.join("CHERRY_PICK_HEAD").exists() {
            RepoOp::CherryPick
        } else if g.join("REVERT_HEAD").exists() {
            RepoOp::Revert
        } else {
            RepoOp::None
        }
    }

    /// All tracked files (for the workspace tree).
    pub fn tracked_files(&self) -> GitResult<Vec<String>> {
        let out = self.git().args(["ls-files", "-z", "--cached"]).quiet().run()?;
        let mut v = super::split_nul(&out.stdout);
        v.dedup();
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_and_entries() {
        let raw = b"# branch.oid 1234abcd\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -3\0# stash 1\0\
1 .M N... 100644 100644 100644 aaa bbb src/a b.txt\0\
2 R. N... 100644 100644 100644 aaa bbb R100 new.txt\0old.txt\0\
u UU N... 100644 100644 100644 100644 a b c conf.txt\0\
? art/new file.psd\0\
1 D. N... 100644 000000 000000 aaa 000 gone.txt\0";
        let (b, files, stash) = parse_porcelain_v2(raw);
        assert_eq!(b.head.as_deref(), Some("main"));
        assert_eq!(b.upstream.as_deref(), Some("origin/main"));
        assert_eq!((b.ahead, b.behind), (2, 3));
        assert_eq!(stash, 1);
        assert_eq!(files.len(), 5);
        let get = |p: &str| files.iter().find(|f| f.path == p).unwrap().clone();
        assert_eq!(get("src/a b.txt").kind, ChangeKind::Edit);
        let mv = get("new.txt");
        assert_eq!(mv.kind, ChangeKind::Move);
        assert_eq!(mv.orig_path.as_deref(), Some("old.txt"));
        assert_eq!(get("conf.txt").kind, ChangeKind::Conflict);
        assert!(get("art/new file.psd").untracked);
        assert_eq!(get("gone.txt").kind, ChangeKind::Delete);
    }

    #[test]
    fn parses_initial_and_detached() {
        let (b, _, _) = parse_porcelain_v2(b"# branch.oid (initial)\0# branch.head (detached)\0");
        assert_eq!(b.oid, None);
        assert_eq!(b.head, None);
    }
}
