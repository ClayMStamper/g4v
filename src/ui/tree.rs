//! Workspace file tree model: builds a folder hierarchy from flat paths and
//! flattens the visible part into rows so huge repositories stay fast
//! (only the rows on screen are ever laid out).

use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub depth: usize,
    pub children: Vec<usize>,
    /// Exists on the server but not in my workspace yet.
    pub ghost: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TreeFilter {
    #[default]
    All,
    Changed,
    OutOfDate,
    Locked,
}

#[derive(Default)]
pub struct FileTree {
    pub nodes: Vec<Node>,
    pub expanded: HashSet<String>,
    pub rows: Vec<usize>,
    dirty: bool,
    pub search: String,
    pub filter: TreeFilter,
    last_search: String,
    last_filter: TreeFilter,
}

impl FileTree {
    /// Rebuilds from file lists. `ghosts` are files that only exist on the server.
    pub fn rebuild(&mut self, files: &[String], ghosts: &[String]) {
        let mut nodes = vec![Node { name: String::new(), path: String::new(), is_dir: true, depth: 0, children: vec![], ghost: false }];
        let mut dirs: HashMap<String, usize> = HashMap::new();
        dirs.insert(String::new(), 0);
        let all = files.iter().map(|f| (f, false)).chain(ghosts.iter().map(|g| (g, true)));
        for (path, ghost) in all {
            let mut parent = 0usize;
            let mut acc = String::new();
            let parts: Vec<&str> = path.split('/').collect();
            for (i, part) in parts.iter().enumerate() {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(part);
                let last = i + 1 == parts.len();
                if last {
                    let idx = nodes.len();
                    nodes.push(Node { name: part.to_string(), path: acc.clone(), is_dir: false, depth: i, children: vec![], ghost });
                    nodes[parent].children.push(idx);
                } else if let Some(&d) = dirs.get(&acc) {
                    parent = d;
                } else {
                    let idx = nodes.len();
                    nodes.push(Node { name: part.to_string(), path: acc.clone(), is_dir: true, depth: i, children: vec![], ghost: false });
                    nodes[parent].children.push(idx);
                    dirs.insert(acc.clone(), idx);
                    parent = idx;
                }
            }
        }
        // Sort: folders first, then case-insensitive name.
        let keys: Vec<(bool, String)> = nodes.iter().map(|n| (!n.is_dir, n.name.to_lowercase())).collect();
        for n in nodes.iter_mut() {
            n.children.sort_by(|a, b| keys[*a].cmp(&keys[*b]));
        }
        self.nodes = nodes;
        self.dirty = true;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn toggle(&mut self, path: &str) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_string());
        }
        self.dirty = true;
    }

    /// Expands all ancestors of `path` so it becomes visible.
    pub fn reveal(&mut self, path: &str) {
        let mut acc = String::new();
        let parts: Vec<&str> = path.split('/').collect();
        for p in &parts[..parts.len().saturating_sub(1)] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(p);
            self.expanded.insert(acc.clone());
        }
        self.dirty = true;
    }

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.dirty = true;
    }

    /// Recomputes visible rows if anything changed. `interesting` decides which files pass the filter.
    pub fn refresh_rows(&mut self, interesting: impl Fn(&str, TreeFilter) -> bool) {
        if !self.dirty && self.search == self.last_search && self.filter == self.last_filter {
            return;
        }
        self.dirty = false;
        self.last_search = self.search.clone();
        self.last_filter = self.filter;
        self.rows.clear();
        if self.nodes.is_empty() {
            return;
        }
        let needle = self.search.trim().to_lowercase();
        let filtering = !needle.is_empty() || self.filter != TreeFilter::All;
        if filtering {
            // Show matching files with their folders, fully expanded.
            let mut keep = vec![false; self.nodes.len()];
            self.mark_matches(0, &needle, &interesting, &mut keep);
            self.push_filtered(0, &keep);
        } else {
            self.push_rows(0);
        }
    }

    fn mark_matches(&self, idx: usize, needle: &str, f: &impl Fn(&str, TreeFilter) -> bool, keep: &mut [bool]) -> bool {
        let n = &self.nodes[idx];
        if n.is_dir {
            let mut any = false;
            for &c in &n.children {
                any |= self.mark_matches(c, needle, f, keep);
            }
            keep[idx] = any;
            any
        } else {
            let ok = (needle.is_empty() || n.path.to_lowercase().contains(needle)) && f(&n.path, self.filter);
            keep[idx] = ok;
            ok
        }
    }

    fn push_filtered(&mut self, idx: usize, keep: &[bool]) {
        let children = self.nodes[idx].children.clone();
        for c in children {
            if keep[c] {
                self.rows.push(c);
                if self.nodes[c].is_dir {
                    self.push_filtered(c, keep);
                }
            }
        }
    }

    fn push_rows(&mut self, idx: usize) {
        let children = self.nodes[idx].children.clone();
        for c in children {
            self.rows.push(c);
            if self.nodes[c].is_dir && self.expanded.contains(&self.nodes[c].path) {
                self.push_rows(c);
            }
        }
    }

    pub fn is_filtering(&self) -> bool {
        !self.search.trim().is_empty() || self.filter != TreeFilter::All
    }

    /// All file paths below a folder (or the file itself).
    pub fn files_under(&self, path: &str) -> Vec<String> {
        let Some(start) = self.nodes.iter().position(|n| n.path == path) else { return vec![] };
        let mut out = Vec::new();
        let mut stack = vec![start];
        while let Some(i) = stack.pop() {
            let n = &self.nodes[i];
            if n.is_dir {
                stack.extend(n.children.iter().copied());
            } else if !n.ghost {
                out.push(n.path.clone());
            }
        }
        out
    }
}

/// Every ancestor folder of the given paths ("a/b/c.txt" -> "a", "a/b").
pub fn ancestor_dirs<'a>(paths: impl IntoIterator<Item = &'a String>) -> HashSet<String> {
    let mut out = HashSet::new();
    for p in paths {
        let mut acc = String::new();
        let parts: Vec<&str> = p.split('/').collect();
        for part in &parts[..parts.len().saturating_sub(1)] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            out.insert(acc.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_flattens() {
        let mut t = FileTree::default();
        t.rebuild(&["b.txt".into(), "art/z.png".into(), "art/a.png".into(), "a.txt".into()], &["art/new.png".into()]);
        t.refresh_rows(|_, _| true);
        let names: Vec<_> = t.rows.iter().map(|&i| t.nodes[i].name.clone()).collect();
        assert_eq!(names, vec!["art", "a.txt", "b.txt"]);
        t.toggle("art");
        t.refresh_rows(|_, _| true);
        let names: Vec<_> = t.rows.iter().map(|&i| t.nodes[i].name.clone()).collect();
        assert_eq!(names, vec!["art", "a.png", "new.png", "z.png", "a.txt", "b.txt"]);
        assert!(t.nodes.iter().find(|n| n.name == "new.png").unwrap().ghost);
        assert_eq!(t.files_under("art").len(), 2);
    }

    #[test]
    fn search_expands_matches() {
        let mut t = FileTree::default();
        t.rebuild(&["x/y/hero.psd".into(), "x/other.txt".into()], &[]);
        t.search = "hero".into();
        t.refresh_rows(|_, _| true);
        let names: Vec<_> = t.rows.iter().map(|&i| t.nodes[i].name.clone()).collect();
        assert_eq!(names, vec!["x", "y", "hero.psd"]);
    }

    #[test]
    fn ancestors() {
        let v = vec!["a/b/c.txt".to_string()];
        let d = ancestor_dirs(&v);
        assert!(d.contains("a") && d.contains("a/b") && d.len() == 2);
    }
}
