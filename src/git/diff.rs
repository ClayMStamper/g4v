//! Diffs of working files and committed revisions.

use super::{GitResult, Repo};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
    Hunk,
    Meta,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

/// Parsed information about a Git LFS pointer file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LfsPointer {
    pub oid: String,
    pub size: u64,
}

pub fn parse_lfs_pointer(text: &str) -> Option<LfsPointer> {
    if !text.starts_with("version https://git-lfs.github.com/spec/") {
        return None;
    }
    let mut oid = None;
    let mut size = None;
    for l in text.lines() {
        if let Some(v) = l.strip_prefix("oid sha256:") {
            oid = Some(v.trim().to_string());
        } else if let Some(v) = l.strip_prefix("size ") {
            size = v.trim().parse().ok();
        }
    }
    Some(LfsPointer { oid: oid?, size: size? })
}

#[derive(Clone, Debug, Default)]
pub struct FileDiff {
    pub lines: Vec<DiffLine>,
    pub binary: bool,
    pub truncated: bool,
    pub adds: usize,
    pub dels: usize,
    /// For LFS files: (old, new) pointers when available.
    pub lfs: Option<(Option<LfsPointer>, Option<LfsPointer>)>,
}

const MAX_LINES: usize = 20_000;

/// Parses unified diff output for a single file.
pub fn parse_unified(text: &str) -> FileDiff {
    let mut d = FileDiff::default();
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut in_hunk = false;
    let mut lfs_old: Option<String> = None;
    let mut lfs_new: Option<String> = None;
    for line in text.lines() {
        if d.lines.len() >= MAX_LINES {
            d.truncated = true;
            break;
        }
        if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            d.binary = true;
            d.lines.push(DiffLine { kind: LineKind::Meta, old_no: None, new_no: None, text: line.to_string() });
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@") {
            in_hunk = true;
            // @@ -a,b +c,d @@
            let mut it = rest.split_whitespace();
            if let Some(o) = it.next() {
                old_no = o.trim_start_matches('-').split(',').next().and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            if let Some(n) = it.next() {
                new_no = n.trim_start_matches('+').split(',').next().and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            d.lines.push(DiffLine { kind: LineKind::Hunk, old_no: None, new_no: None, text: line.to_string() });
            continue;
        }
        if !in_hunk {
            d.lines.push(DiffLine { kind: LineKind::Meta, old_no: None, new_no: None, text: line.to_string() });
            continue;
        }
        if line.starts_with("diff --git") {
            in_hunk = false;
            d.lines.push(DiffLine { kind: LineKind::Meta, old_no: None, new_no: None, text: line.to_string() });
            continue;
        }
        let (kind, body) = match line.chars().next() {
            Some('+') => (LineKind::Add, &line[1..]),
            Some('-') => (LineKind::Del, &line[1..]),
            Some(' ') => (LineKind::Context, &line[1..]),
            Some('\\') => (LineKind::Meta, line),
            _ => (LineKind::Context, line),
        };
        match kind {
            LineKind::Add => {
                d.adds += 1;
                lfs_new.get_or_insert_with(String::new).push_str(&format!("{body}\n"));
                d.lines.push(DiffLine { kind, old_no: None, new_no: Some(new_no), text: body.to_string() });
                new_no += 1;
            }
            LineKind::Del => {
                d.dels += 1;
                lfs_old.get_or_insert_with(String::new).push_str(&format!("{body}\n"));
                d.lines.push(DiffLine { kind, old_no: Some(old_no), new_no: None, text: body.to_string() });
                old_no += 1;
            }
            LineKind::Context => {
                lfs_old.get_or_insert_with(String::new).push_str(&format!("{body}\n"));
                lfs_new.get_or_insert_with(String::new).push_str(&format!("{body}\n"));
                d.lines.push(DiffLine { kind, old_no: Some(old_no), new_no: Some(new_no), text: body.to_string() });
                old_no += 1;
                new_no += 1;
            }
            _ => d.lines.push(DiffLine { kind, old_no: None, new_no: None, text: body.to_string() }),
        }
    }
    let old_ptr = lfs_old.as_deref().and_then(parse_lfs_pointer);
    let new_ptr = lfs_new.as_deref().and_then(parse_lfs_pointer);
    if old_ptr.is_some() || new_ptr.is_some() {
        d.lfs = Some((old_ptr, new_ptr));
    }
    d
}

/// Synthesises an "all added" diff for an untracked file.
pub fn added_file_diff(bytes: &[u8]) -> FileDiff {
    let mut d = FileDiff::default();
    if bytes.iter().take(8000).any(|b| *b == 0) {
        d.binary = true;
        d.lines.push(DiffLine { kind: LineKind::Meta, old_no: None, new_no: None, text: "New binary file".into() });
        return d;
    }
    let text = String::from_utf8_lossy(bytes);
    for (i, l) in text.lines().enumerate() {
        if i >= MAX_LINES {
            d.truncated = true;
            break;
        }
        d.adds += 1;
        d.lines.push(DiffLine { kind: LineKind::Add, old_no: None, new_no: Some(i as u32 + 1), text: l.to_string() });
    }
    d
}

pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

pub fn is_image_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".bmp", ".gif", ".tga", ".tif", ".tiff", ".webp"]
        .iter()
        .any(|e| lower.ends_with(e))
}

impl Repo {
    /// Diff of a working file against HEAD (staged + unstaged changes together).
    pub fn diff_worktree(&self, path: &str, orig: Option<&str>, untracked: bool, has_head: bool) -> GitResult<FileDiff> {
        if untracked {
            let bytes = std::fs::read(self.abs(path)).unwrap_or_default();
            return Ok(added_file_diff(&bytes));
        }
        let base = if has_head { "HEAD" } else { EMPTY_TREE };
        let mut cmd = self
            .git()
            .args(["diff", "--no-color", "--no-ext-diff", "--no-textconv", "-M", base, "--"])
            .arg(path);
        if let Some(o) = orig {
            cmd = cmd.arg(o);
        }
        Ok(parse_unified(&cmd.quiet().run()?.text()))
    }

    /// Diff of a file between two revisions (`old` may be None for root commits).
    pub fn diff_revs(&self, old: Option<&str>, new: &str, path: &str, old_path: Option<&str>) -> GitResult<FileDiff> {
        let old = old.unwrap_or(EMPTY_TREE);
        let mut cmd = self
            .git()
            .args(["diff", "--no-color", "--no-ext-diff", "--no-textconv", "-M"])
            .arg(old)
            .arg(new)
            .arg("--")
            .arg(path);
        if let Some(o) = old_path {
            cmd = cmd.arg(o);
        }
        Ok(parse_unified(&cmd.quiet().run()?.text()))
    }

    /// File contents at a revision with filters applied (LFS objects are smudged).
    pub fn blob_bytes(&self, rev: &str, path: &str) -> GitResult<Vec<u8>> {
        Ok(self.git().args(["cat-file", "--filters"]).arg(format!("{rev}:{path}")).quiet().run()?.stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hunks_and_numbers() {
        let text = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n";
        let d = parse_unified(text);
        assert_eq!(d.adds, 1);
        assert_eq!(d.dels, 1);
        let add = d.lines.iter().find(|l| l.kind == LineKind::Add).unwrap();
        assert_eq!(add.new_no, Some(2));
        assert_eq!(add.text, "B");
    }

    #[test]
    fn detects_lfs_pointer_changes() {
        let text = "diff --git a/a.psd b/a.psd\n--- a/a.psd\n+++ b/a.psd\n@@ -1,3 +1,3 @@\n version https://git-lfs.github.com/spec/v1\n-oid sha256:aaa\n-size 10\n+oid sha256:bbb\n+size 20\n";
        let d = parse_unified(text);
        let (o, n) = d.lfs.unwrap();
        assert_eq!(o.unwrap().size, 10);
        assert_eq!(n.unwrap().oid, "bbb");
    }

    #[test]
    fn binary_detection() {
        assert!(added_file_diff(&[1, 2, 0, 3]).binary);
        assert!(!added_file_diff(b"hello\nworld").binary);
        assert!(parse_unified("Binary files a/x and b/x differ\n").binary);
    }
}
