//! Commit history ("Submitted" changelists) and a simple lane graph.

use super::{GitResult, Repo};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub time: i64,
    /// Decorations: branch / tag names pointing at this commit.
    pub refs: Vec<String>,
    pub subject: String,
}

impl Commit {
    pub fn short_id(&self) -> &str {
        &self.id[..self.id.len().min(8)]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    /// Status letter: A, M, D, R, C, T.
    pub status: char,
    pub path: String,
    pub old_path: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CommitDetail {
    pub commit: Commit,
    pub committer: String,
    pub commit_time: i64,
    pub message: String,
    pub files: Vec<ChangedFile>,
}

/// What history to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryScope {
    /// Current branch plus its upstream (so incoming commits are visible).
    Current,
    /// Every branch and tag.
    All,
    /// A single file (follows renames).
    File(String),
}

const SEP: char = '\u{1f}';
const FORMAT: &str = "%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%s%x1e";

pub fn parse_log(text: &str) -> Vec<Commit> {
    text.split('\u{1e}')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches(['\n', '\r']);
            if rec.is_empty() {
                return None;
            }
            let f: Vec<&str> = rec.splitn(7, SEP).collect();
            if f.len() < 7 {
                return None;
            }
            Some(Commit {
                id: f[0].to_string(),
                parents: f[1].split_whitespace().map(str::to_string).collect(),
                author: f[2].to_string(),
                email: f[3].to_string(),
                time: f[4].parse().unwrap_or(0),
                refs: f[5].split(", ").map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
                subject: f[6].trim_end().to_string(),
            })
        })
        .collect()
}

/// Parses `--name-status -z` output (also used by diff-tree).
pub fn parse_name_status(bytes: &[u8]) -> Vec<ChangedFile> {
    let parts = super::split_nul(bytes);
    let mut out = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        let code = parts[i].chars().next().unwrap_or('M');
        if (code == 'R' || code == 'C') && i + 2 < parts.len() {
            out.push(ChangedFile { status: code, old_path: Some(parts[i + 1].clone()), path: parts[i + 2].clone() });
            i += 3;
        } else if i + 1 < parts.len() {
            out.push(ChangedFile { status: code, path: parts[i + 1].clone(), old_path: None });
            i += 2;
        } else {
            break;
        }
    }
    out
}

/// One row of the history graph.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphRow {
    /// Column of this commit's dot.
    pub node: usize,
    /// Line segments from the top edge (col) to the row middle (col).
    pub up: Vec<(usize, usize)>,
    /// Line segments from the row middle (col) to the bottom edge (col).
    pub down: Vec<(usize, usize)>,
    /// Number of columns in use for this row.
    pub width: usize,
}

/// Computes a straightforward lane layout for commits in topological order.
pub fn build_graph(commits: &[Commit]) -> Vec<GraphRow> {
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());
    for c in commits {
        let mut row = GraphRow::default();
        let before = lanes.clone();
        // Lane(s) that were waiting for this commit.
        let waiting: Vec<usize> = before.iter().enumerate().filter(|(_, l)| l.as_deref() == Some(c.id.as_str())).map(|(i, _)| i).collect();
        let node = match waiting.first() {
            Some(&i) => i,
            None => match lanes.iter().position(|l| l.is_none()) {
                Some(i) => i,
                None => {
                    lanes.push(None);
                    lanes.len() - 1
                }
            },
        };
        row.node = node;
        for (i, l) in before.iter().enumerate() {
            match l {
                Some(id) if *id == c.id => row.up.push((i, node)),
                Some(_) => row.up.push((i, i)),
                None => {}
            }
        }
        for &i in &waiting {
            lanes[i] = None;
        }
        // Continue lanes that pass through.
        for (i, l) in lanes.iter().enumerate() {
            if l.is_some() {
                row.down.push((i, i));
            }
        }
        // Parents.
        if let Some(first) = c.parents.first() {
            // Duplicated lanes are fine: they converge when the parent is reached.
            lanes[node] = Some(first.clone());
            row.down.push((node, node));
            for p in c.parents.iter().skip(1) {
                if let Some(j) = lanes.iter().position(|l| l.as_deref() == Some(p.as_str())) {
                    row.down.push((node, j));
                } else {
                    let k = match lanes.iter().position(|l| l.is_none()) {
                        Some(k) => k,
                        None => {
                            lanes.push(None);
                            lanes.len() - 1
                        }
                    };
                    lanes[k] = Some(p.clone());
                    row.down.push((node, k));
                }
            }
        }
        while lanes.last().is_some_and(|l| l.is_none()) {
            lanes.pop();
        }
        let max_col = row.up.iter().chain(row.down.iter()).flat_map(|(a, b)| [*a, *b]).chain([node]).max().unwrap_or(0);
        row.width = max_col + 1;
        rows.push(row);
    }
    rows
}

impl Repo {
    pub fn log(&self, scope: &HistoryScope, limit: usize, has_upstream: bool) -> GitResult<Vec<Commit>> {
        let mut cmd = self.git().arg("log").arg(format!("--format={FORMAT}")).arg(format!("-n{limit}")).arg("--date-order");
        match scope {
            HistoryScope::Current => {
                cmd = cmd.arg("HEAD");
                if has_upstream {
                    cmd = cmd.arg("@{upstream}");
                }
            }
            HistoryScope::All => {
                cmd = cmd.args(["--branches", "--remotes", "--tags", "HEAD"]);
            }
            HistoryScope::File(p) => {
                cmd = cmd.args(["--follow", "HEAD", "--"]).arg(p);
            }
        }
        let out = match cmd.quiet().run() {
            Ok(o) => o,
            // A brand-new repository has no commits yet.
            Err(e) if e.stderr.contains("does not have any commits") || e.stderr.contains("unknown revision") => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(parse_log(&out.text()))
    }

    pub fn commit_detail(&self, id: &str) -> GitResult<CommitDetail> {
        let fmt = "%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%cn%x1f%ct%x1f%B";
        let text = self.git().args(["show", "-s", "--no-show-signature"]).arg(format!("--format={fmt}")).arg(id).run()?.text();
        let f: Vec<&str> = text.splitn(9, SEP).collect();
        if f.len() < 9 {
            return Err(super::GitError {
                command: format!("git show {id}"),
                code: None,
                stderr: "Unexpected output from git show".into(),
                stdout: text.clone(),
            });
        }
        let message = f[8].trim_end().to_string();
        let commit = Commit {
            id: f[0].to_string(),
            parents: f[1].split_whitespace().map(str::to_string).collect(),
            author: f[2].to_string(),
            email: f[3].to_string(),
            time: f[4].parse().unwrap_or(0),
            refs: f[5].split(", ").filter(|s| !s.is_empty()).map(str::to_string).collect(),
            subject: message.lines().next().unwrap_or_default().to_string(),
        };
        let files = self.commit_files(&commit)?;
        Ok(CommitDetail { committer: f[6].to_string(), commit_time: f[7].parse().unwrap_or(0), commit, message, files })
    }

    pub fn commit_files(&self, c: &Commit) -> GitResult<Vec<ChangedFile>> {
        let out = if let Some(parent) = c.parents.first() {
            self.git().args(["diff", "--no-ext-diff", "-z", "--name-status", "-M"]).arg(parent).arg(&c.id).quiet().run()?
        } else {
            self.git().args(["diff-tree", "--no-commit-id", "-r", "-z", "--name-status", "--root", "-M"]).arg(&c.id).quiet().run()?
        };
        Ok(parse_name_status(&out.stdout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, parents: &[&str]) -> Commit {
        Commit {
            id: id.into(),
            parents: parents.iter().map(|s| s.to_string()).collect(),
            author: String::new(),
            email: String::new(),
            time: 0,
            refs: vec![],
            subject: String::new(),
        }
    }

    #[test]
    fn linear_graph_is_single_lane() {
        let commits = vec![c("c", &["b"]), c("b", &["a"]), c("a", &[])];
        let g = build_graph(&commits);
        assert!(g.iter().all(|r| r.node == 0 && r.width == 1));
    }

    #[test]
    fn merge_graph_uses_two_lanes() {
        // m merges x (side) into b
        let commits = vec![c("m", &["b", "x"]), c("x", &["a"]), c("b", &["a"]), c("a", &[])];
        let g = build_graph(&commits);
        assert_eq!(g[0].node, 0);
        assert!(g[0].down.contains(&(0, 1)));
        assert_eq!(g[1].node, 1);
        assert_eq!(g[2].node, 0);
        // x's lane converges into a.
        assert_eq!(g[3].node, 0);
        assert!(g[3].up.contains(&(1, 0)));
    }

    #[test]
    fn parses_log_records() {
        let text = "abc\u{1f}p1 p2\u{1f}Ann\u{1f}a@x\u{1f}100\u{1f}HEAD -> main, origin/main\u{1f}Hello\u{1e}\n";
        let v = parse_log(text);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].parents, vec!["p1", "p2"]);
        assert_eq!(v[0].refs, vec!["HEAD -> main", "origin/main"]);
        assert_eq!(v[0].subject, "Hello");
    }

    #[test]
    fn parses_name_status_with_renames() {
        let raw = b"M\0a.txt\0R100\0old.txt\0new.txt\0D\0gone\0";
        let v = parse_name_status(raw);
        assert_eq!(v.len(), 3);
        assert_eq!(v[1].status, 'R');
        assert_eq!(v[1].old_path.as_deref(), Some("old.txt"));
        assert_eq!(v[1].path, "new.txt");
        assert_eq!(v[2].status, 'D');
    }
}
