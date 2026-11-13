//! State and behaviour of one open repository tab.

use super::tree::{ancestor_dirs, FileTree, TreeFilter};
use crate::changelists::{ChangelistStore, DEFAULT_ID};
use crate::git::branches::{BranchRef, Stash, Submodule};
use crate::git::diff::{is_image_path, FileDiff};
use crate::git::history::{build_graph, ChangedFile, Commit, CommitDetail, GraphRow, HistoryScope};
use crate::git::lfs::Lock;
use crate::git::ops::{PullMode, PushOutcome, Side, SyncOutcome};
use crate::git::shelf::{Shelf, BACKUP_TAG};
use crate::git::status::{ChangeKind, FileStatus, RepoOp, StatusSnapshot};
use crate::git::{GitError, Repo};
use crate::settings::Settings;
use crate::worker::Worker;
use eframe::egui;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

fn es(e: GitError) -> String {
    e.to_string()
}

// ---------------------------------------------------------------- messages

pub struct TreeData {
    pub tracked: Vec<String>,
    pub lockable: HashSet<String>,
    pub lfs: HashSet<String>,
    pub uses_lfs: bool,
    pub lfs_installed: bool,
}

pub struct FetchData {
    pub remote: Option<String>,
    pub incoming: Vec<(char, String)>,
}

pub struct RefsData {
    pub identity: crate::git::ops::Identity,
    pub default_remote: Option<String>,
    pub branches: Vec<BranchRef>,
    pub remotes: Vec<(String, String)>,
    pub submodules: Vec<Submodule>,
    pub stashes: Vec<Stash>,
}

pub struct DiffData {
    pub title: String,
    pub path: String,
    pub diff: FileDiff,
    pub old_image: Option<egui::ColorImage>,
    pub new_image: Option<egui::ColorImage>,
    pub old_size: Option<u64>,
    pub new_size: Option<u64>,
}

pub enum OpOk {
    Message(String),
    Sync(SyncOutcome),
    Submitted {
        commit: String,
        /// Changelist number of what landed on the branch (HEAD after any rebase during upload); 0 if unknown.
        cl: u64,
        cl_id: u32,
        push: Option<Result<String, String>>,
        unlocked: Vec<String>,
        unlock_failed: Vec<(String, String)>,
    },
    Shelved {
        shelf: Shelf,
        reverted: bool,
        share: Option<Result<(), String>>,
        cl_id: Option<u32>,
    },
    Unshelved {
        paths: Vec<String>,
        description: String,
        backup: Option<String>,
        /// Put the files in a new changelist named after the shelf (false = default changelist).
        new_changelist: bool,
    },
    Locks {
        verb: &'static str,
        ok: Vec<String>,
        failed: Vec<(String, String)>,
    },
    BlockedResolved {
        shelf: Shelf,
        sync: SyncOutcome,
    },
}

pub enum Msg {
    Status(Result<StatusSnapshot, String>),
    Tree(Result<TreeData, String>),
    Fetched(Result<FetchData, String>),
    /// Local-only recomputation of incoming files (errors are not network errors).
    Incoming(Result<Vec<(char, String)>, String>),
    Locks(Result<Vec<Lock>, String>),
    History(u64, Result<Vec<Commit>, String>),
    CommitDetail(Result<CommitDetail, String>),
    Diff(u64, Result<DiffData, String>),
    Shelves(Result<Vec<Shelf>, String>),
    ShelfFiles(String, Result<Vec<ChangedFile>, String>),
    Refs(Result<RefsData, String>),
    Done {
        title: String,
        result: Result<OpOk, String>,
        net: bool,
    },
}

// ---------------------------------------------------------------- UI enums

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CenterTab {
    Pending,
    History,
    Shelves,
    Locks,
    Log,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LeftTab {
    Workspace,
    Branches,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffTarget {
    Working { path: String },
    Commit { id: String, parent: Option<String>, file: ChangedFile },
    Shelf { id: String, base: Option<String>, file: ChangedFile },
    Incoming { path: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwitchTarget {
    Local(String),
    Remote(String),
}

#[derive(Clone, Debug)]
pub enum Action {
    Refresh,
    Fetch,
    GetLatest,
    ShelveOverlapAndSync(Vec<String>),
    Push,
    ShowDiff(DiffTarget),
    OpenSubmit(u32),
    OpenShelve(Vec<String>, Option<u32>),
    OpenRevert(Vec<String>),
    NewChangelist(Vec<String>),
    EditChangelist(u32),
    DeleteChangelist(u32),
    MoveFiles(Vec<String>, u32),
    Lock(Vec<String>),
    Unlock(Vec<String>, bool),
    OpenFile(String),
    RevealFile(String),
    FileHistory(String),
    RevealInTree(String),
    Resolve(String, Side),
    MarkResolved(String),
    MergeTool(String),
    ContinueOp,
    AbortOp,
    Switch(SwitchTarget),
    SwitchNow(SwitchTarget, bool),
    NewBranch(Option<String>),
    DeleteBranch(String, bool),
    DeleteRemoteBranch(String),
    MergeBranch(String),
    SelectCommit(String),
    RevertCommit(String, bool),
    CherryPick(String),
    UndoLastCommit,
    NewTag(String),
    DeleteTag(String),
    PushTag(String),
    GetRevision(String, String),
    SelectShelf(String),
    OpenUnshelve(Shelf, Vec<String>),
    ShareShelf(Shelf),
    UnshareShelf(Shelf),
    DeleteShelf(Shelf),
    StashApply(usize, bool),
    StashDrop(usize),
    OpenSubmodule(String),
    UpdateSubmodules,
    RemoveIndexLock,
    SetupIdentity,
    AddRemote,
    TrackLfs(String),
    Confirm { title: String, body: String, action: Box<Action> },
    // Confirmed / dialog-produced actions:
    DoSubmit { paths: Vec<String>, message: String, cl_id: u32, push: bool, unlock: Vec<String> },
    DoShelve { paths: Vec<String>, description: String, revert: bool, share: bool, cl_id: Option<u32> },
    DoUnshelve { shelf: Shelf, paths: Vec<String> },
    RestoreWip(Shelf),
    DoRevert(Vec<String>),
    DoDeleteShelf(Shelf),
    DoDeleteBranch(String, bool),
    DoDeleteRemoteBranch(String),
    DoRevertCommit(String, bool),
    DoUndoLastCommit,
    DoAbortOp,
    DoMerge(String),
    DoGetRevision(String, String),
    DoForceUnlock(Vec<String>),
}

// ---------------------------------------------------------------- dialogs

pub struct SubmitDlg {
    pub cl_id: u32,
    pub message: String,
    pub files: Vec<(String, bool)>,
    pub push: bool,
    pub unlock: bool,
}

pub struct ShelveDlg {
    pub cl_id: Option<u32>,
    pub description: String,
    pub files: Vec<(String, bool)>,
    pub revert: bool,
    pub share: bool,
}

pub struct UnshelveDlg {
    pub shelf: Shelf,
    pub files: Vec<(ChangedFile, bool)>,
}

pub enum Dialog {
    Submit(SubmitDlg),
    Shelve(ShelveDlg),
    Unshelve(UnshelveDlg),
    Revert(Vec<(String, bool)>),
    Changelist { id: Option<u32>, description: String, move_files: Vec<String> },
    NewBranch { name: String, start: Option<String>, switch: bool },
    SwitchDirty(SwitchTarget),
    SyncBlocked(Vec<String>),
    Confirm { title: String, body: String, action: Action },
    NewTag { rev: String, name: String, message: String, push: bool },
    AddRemote { name: String, url: String },
    Identity { name: String, email: String, global: bool },
}

/// A message shown at the bottom-right for a few seconds (errors stay until dismissed).
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub created: Instant,
}

// ---------------------------------------------------------------- tab state

pub struct RepoTab {
    pub repo: Repo,
    pub title: String,
    /// Path inside the parent repository when opened as a submodule tab.
    pub parent: Option<String>,
    local: Worker<Msg>,
    net: Worker<Msg>,

    pub status: Option<StatusSnapshot>,
    pub status_err: Option<String>,
    status_inflight: bool,
    last_status: Instant,
    pub status_by_path: HashMap<String, usize>,
    pub changed_dirs: HashSet<String>,

    pub changelists: ChangelistStore,

    pub tree: FileTree,
    pub tracked: Vec<String>,
    pub lockable: HashSet<String>,
    pub lfs_paths: HashSet<String>,
    pub uses_lfs: bool,
    pub lfs_installed: bool,
    tree_stale: bool,

    pub remote: Option<String>,
    pub incoming: HashMap<String, char>,
    pub incoming_dirs: HashSet<String>,
    pub last_fetch: Option<Instant>,
    pub fetch_err: Option<String>,
    fetch_inflight: bool,

    pub locks: Vec<Lock>,
    pub locks_by_path: HashMap<String, usize>,
    pub locks_err: Option<String>,
    pub locked_dirs: HashSet<String>,

    pub history: Vec<Commit>,
    pub graph: Vec<GraphRow>,
    pub history_scope: HistoryScope,
    pub history_filter: String,
    history_gen: u64,
    pub history_loading: bool,
    pub history_err: Option<String>,
    pub selected_commit: Option<String>,
    pub commit_detail: Option<CommitDetail>,

    pub shelves: Vec<Shelf>,
    pub selected_shelf: Option<String>,
    pub shelf_files: HashMap<String, Vec<ChangedFile>>,
    pub show_backups: bool,

    pub refs: Option<RefsData>,

    pub center: CenterTab,
    pub left: LeftTab,
    pub selection: BTreeSet<String>,
    pub sel_anchor: Option<String>,
    pub diff_target: Option<DiffTarget>,
    pub diff: Option<Result<DiffData, String>>,
    pub diff_gen: u64,
    pub diff_loading: bool,
    pub textures: (Option<egui::TextureHandle>, Option<egui::TextureHandle>),
    /// New diff data arrived; images must be (re-)uploaded.
    pub textures_stale: bool,

    pub dialog: Option<Dialog>,
    pub toasts: Vec<Toast>,
    /// Requests for the app (open a submodule tab, etc.).
    pub app_requests: Vec<AppRequest>,
    pub branch_filter: String,
    pub collapsed_cls: HashSet<u32>,
    /// Cached `user.name`/`user.email` (refreshed with the branch list).
    pub identity: crate::git::ops::Identity,
}

pub enum AppRequest {
    OpenRepo(std::path::PathBuf, Option<String>),
}

impl RepoTab {
    pub fn new(repo: Repo, ctx: &egui::Context, parent: Option<String>) -> Self {
        let c1 = ctx.clone();
        let c2 = ctx.clone();
        let local = Worker::new(repo.clone(), "local", move || c1.request_repaint());
        let net = Worker::new(repo.clone(), "net", move || c2.request_repaint());
        let changelists = ChangelistStore::load(&repo.meta_dir());
        let title = match &parent {
            Some(p) => format!("{} › {}", p, repo.name()),
            None => repo.name(),
        };
        let mut tab = RepoTab {
            title,
            parent,
            local,
            net,
            status: None,
            status_err: None,
            status_inflight: false,
            last_status: Instant::now(),
            status_by_path: HashMap::new(),
            changed_dirs: HashSet::new(),
            changelists,
            tree: FileTree::default(),
            tracked: Vec::new(),
            lockable: HashSet::new(),
            lfs_paths: HashSet::new(),
            uses_lfs: repo.uses_lfs(),
            lfs_installed: true,
            tree_stale: true,
            incoming: HashMap::new(),
            incoming_dirs: HashSet::new(),
            last_fetch: None,
            fetch_err: None,
            fetch_inflight: false,
            locks: Vec::new(),
            locks_by_path: HashMap::new(),
            locks_err: None,
            locked_dirs: HashSet::new(),
            history: Vec::new(),
            graph: Vec::new(),
            history_scope: HistoryScope::Current,
            history_filter: String::new(),
            history_gen: 0,
            history_loading: false,
            history_err: None,
            selected_commit: None,
            commit_detail: None,
            shelves: Vec::new(),
            selected_shelf: None,
            shelf_files: HashMap::new(),
            show_backups: false,
            refs: None,
            center: CenterTab::Pending,
            left: LeftTab::Workspace,
            selection: BTreeSet::new(),
            sel_anchor: None,
            diff_target: None,
            diff: None,
            diff_gen: 0,
            diff_loading: false,
            textures: (None, None),
            textures_stale: true,
            dialog: None,
            toasts: Vec::new(),
            app_requests: Vec::new(),
            branch_filter: String::new(),
            collapsed_cls: HashSet::new(),
            identity: repo.identity(),
            remote: repo.default_remote(),
            repo,
        };
        tab.refresh_all();
        tab.request_fetch();
        tab
    }

    // ------------------------------------------------------------ helpers

    /// Clears the diff pane (when its subject is no longer selected).
    pub fn clear_diff(&mut self) {
        self.diff_target = None;
        self.diff = None;
        self.diff_gen += 1;
        self.diff_loading = false;
        self.textures = (None, None);
        self.textures_stale = true;
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: false, created: Instant::now() });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: true, created: Instant::now() });
    }

    pub fn busy_label(&self) -> Option<String> {
        self.local.current().filter(|c| c != "status").or_else(|| self.net.current().filter(|c| c != "Checking server"))
    }

    pub fn is_busy(&self) -> bool {
        self.local.current().is_some_and(|c| c != "status") || self.local.queued() > 1
    }

    pub fn net_busy(&self) -> bool {
        self.net.busy()
    }

    pub fn files(&self) -> &[FileStatus] {
        self.status.as_ref().map(|s| s.files.as_slice()).unwrap_or(&[])
    }

    pub fn file(&self, path: &str) -> Option<&FileStatus> {
        let s = self.status.as_ref()?;
        self.status_by_path.get(path).and_then(|&i| s.files.get(i))
    }

    pub fn lock_of(&self, path: &str) -> Option<&Lock> {
        self.locks_by_path.get(path).and_then(|&i| self.locks.get(i))
    }

    pub fn locked_by_me(&self) -> Vec<String> {
        self.locks.iter().filter(|l| l.ours).map(|l| l.path.clone()).collect()
    }

    pub fn op(&self) -> RepoOp {
        self.status.as_ref().map(|s| s.op).unwrap_or_default()
    }

    pub fn has_conflicts(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.has_conflicts())
    }

    /// Paths shown in the Pending view: changed files plus files I have locked.
    pub fn pending_paths(&self) -> Vec<String> {
        let mut v: Vec<String> = self.files().iter().filter(|f| f.kind != ChangeKind::Conflict).map(|f| f.path.clone()).collect();
        for l in self.locks.iter().filter(|l| l.ours) {
            if !self.status_by_path.contains_key(&l.path) {
                v.push(l.path.clone());
            }
        }
        v.sort();
        v.dedup();
        v
    }

    /// Paths in changelist `id`.
    pub fn cl_files(&self, id: u32) -> Vec<String> {
        self.changelists.files_in(id, &self.pending_paths())
    }

    pub fn changelist_name(&self, id: u32) -> String {
        if id == DEFAULT_ID {
            "Default".into()
        } else {
            self.changelists.get(id).map(|c| first_line(&c.description)).unwrap_or_else(|| format!("#{id}"))
        }
    }

    fn save_changelists(&mut self) {
        if let Err(e) = self.changelists.save() {
            self.error(format!("Could not save changelists: {e}"));
        }
    }

    /// Files that need attention before submitting.
    pub fn locked_by_others<'a>(&self, paths: impl IntoIterator<Item = &'a String>) -> Vec<(String, String)> {
        paths.into_iter().filter_map(|p| self.lock_of(p).filter(|l| !l.ours).map(|l| (p.clone(), l.owner.clone()))).collect()
    }

    pub fn lockable_not_locked<'a>(&self, paths: impl IntoIterator<Item = &'a String>) -> Vec<String> {
        paths
            .into_iter()
            .filter(|p| self.lockable.contains(*p) && self.lock_of(p).is_none())
            .filter(|p| self.file(p).is_some_and(|f| f.kind != ChangeKind::Add))
            .cloned()
            .collect()
    }

    pub fn large_non_lfs(&self, paths: &[String], threshold_mb: u64) -> Vec<(String, u64)> {
        let limit = threshold_mb.saturating_mul(1024 * 1024);
        paths
            .iter()
            .filter(|p| !self.lfs_paths.contains(*p))
            .filter(|p| self.file(p).is_some_and(|f| matches!(f.kind, ChangeKind::Add | ChangeKind::Edit | ChangeKind::Move)))
            .filter_map(|p| self.repo.file_size(p).filter(|s| *s >= limit).map(|s| (p.clone(), s)))
            .collect()
    }

    // ------------------------------------------------------------ requests

    pub fn refresh_all(&mut self) {
        self.request_status(true);
        self.tree_stale = true;
        self.request_tree();
        self.request_refs();
        self.request_shelves();
        self.request_history();
        self.request_locks();
    }

    pub fn request_status(&mut self, force: bool) {
        if self.status_inflight && !force {
            return;
        }
        self.status_inflight = true;
        self.last_status = Instant::now();
        self.local.submit("status", |r| Msg::Status(r.status().map_err(es)));
    }

    fn request_tree(&mut self) {
        if !self.tree_stale {
            return;
        }
        self.tree_stale = false;
        self.local.submit("Reading files", |r| {
            Msg::Tree((|| {
                let tracked = r.tracked_files().map_err(es)?;
                let uses_lfs = r.uses_lfs();
                let (lockable, lfs) = if uses_lfs {
                    (r.lockable_paths(&tracked).unwrap_or_default(), r.lfs_tracked_paths(&tracked).unwrap_or_default())
                } else {
                    Default::default()
                };
                let lfs_installed = !uses_lfs || r.lfs_installed();
                Ok(TreeData { tracked, lockable, lfs, uses_lfs, lfs_installed })
            })())
        });
    }

    pub fn request_refs(&mut self) {
        self.local.submit("Reading branches", |r| {
            Msg::Refs(r.branches().map_err(es).map(|branches| RefsData {
                identity: r.identity(),
                default_remote: r.default_remote(),
                branches,
                remotes: r.remotes(),
                submodules: r.submodules(),
                stashes: r.stashes(),
            }))
        });
    }

    pub fn request_shelves(&mut self) {
        self.local.submit("Reading shelves", |r| Msg::Shelves(r.list_shelves().map_err(es)));
    }

    pub fn request_history(&mut self) {
        self.history_gen += 1;
        self.history_loading = true;
        let gen = self.history_gen;
        let scope = self.history_scope.clone();
        let has_up = self.status.as_ref().map(|s| s.branch.upstream.is_some()).unwrap_or(true);
        self.local.submit("Reading history", move |r| {
            let res = r.log(&scope, 2000, has_up);
            // Retry without the upstream if it vanished.
            let res = match res {
                Err(_) if has_up && scope == HistoryScope::Current => r.log(&scope, 2000, false),
                other => other,
            };
            Msg::History(gen, res.map_err(es))
        });
    }

    pub fn request_locks(&mut self) {
        if !self.uses_lfs {
            return;
        }
        self.net.submit("Checking locks", |r| Msg::Locks(r.lfs_locks().map_err(es)));
    }

    pub fn request_fetch(&mut self) {
        if self.fetch_inflight {
            return;
        }
        self.fetch_inflight = true;
        self.net.submit("Checking server", |r| {
            Msg::Fetched((|| {
                let remote = r.fetch().map_err(es)?;
                let incoming = r.incoming_changes().map_err(es)?;
                Ok(FetchData { remote, incoming })
            })())
        });
        self.request_locks();
    }

    pub fn request_diff(&mut self, target: DiffTarget) {
        self.diff_gen += 1;
        let gen = self.diff_gen;
        self.diff_target = Some(target.clone());
        self.diff_loading = true;
        let has_head = self.status.as_ref().is_some_and(|s| s.branch.oid.is_some());
        let file = match &target {
            DiffTarget::Working { path } => self.file(path).cloned(),
            _ => None,
        };
        self.local.submit("Loading diff", move |r| Msg::Diff(gen, load_diff(r, &target, file.as_ref(), has_head)));
    }

    // ------------------------------------------------------------ per-frame

    pub fn tick(&mut self, ctx: &egui::Context, settings: &Settings) {
        for msg in self.local.poll().into_iter().chain(self.net.poll()) {
            self.handle_msg(msg, settings);
        }
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        // Poll the working copy for changes made by other programs (Photoshop, Blender, ...).
        let interval = if focused { Duration::from_secs(2) } else { Duration::from_secs(10) };
        if self.last_status.elapsed() >= interval && !self.local.busy() {
            self.request_status(false);
        }
        if self.tree_stale && !self.local.busy() {
            self.request_tree();
        }
        if settings.fetch_interval_secs > 0 {
            let due = self.last_fetch.is_none_or(|t| t.elapsed() >= Duration::from_secs(settings.fetch_interval_secs));
            if due && !self.fetch_inflight && !self.local.busy() && self.last_fetch.is_some() {
                self.request_fetch();
            }
        }
        self.toasts.retain(|t| t.error || t.created.elapsed() < Duration::from_secs(6));
        ctx.request_repaint_after(Duration::from_millis(if focused { 1000 } else { 5000 }));
    }

    fn handle_msg(&mut self, msg: Msg, settings: &Settings) {
        match msg {
            Msg::Status(res) => {
                self.status_inflight = false;
                match res {
                    Ok(s) => self.apply_status(s),
                    Err(e) => self.status_err = Some(e),
                }
            }
            Msg::Tree(res) => match res {
                Ok(t) => {
                    self.tracked = t.tracked;
                    self.lockable = t.lockable;
                    self.lfs_paths = t.lfs;
                    self.uses_lfs = t.uses_lfs;
                    self.lfs_installed = t.lfs_installed;
                    self.rebuild_tree();
                }
                Err(e) => self.error(format!("Could not list files: {e}")),
            },
            Msg::Fetched(res) => {
                self.fetch_inflight = false;
                self.last_fetch = Some(Instant::now());
                match res {
                    Ok(f) => {
                        self.fetch_err = None;
                        self.remote = f.remote;
                        let new: HashMap<String, char> = f.incoming.into_iter().map(|(c, p)| (p, c)).collect();
                        let changed = new != self.incoming;
                        self.incoming = new;
                        self.incoming_dirs = ancestor_dirs(self.incoming.keys());
                        if changed {
                            self.rebuild_tree();
                            self.request_history();
                        }
                        self.request_status(true);
                        self.request_shelves();
                        self.request_refs();
                    }
                    Err(e) => self.fetch_err = Some(e),
                }
            }
            Msg::Incoming(res) => {
                self.fetch_inflight = false;
                if let Ok(list) = res {
                    let new: HashMap<String, char> = list.into_iter().map(|(c, p)| (p, c)).collect();
                    if new != self.incoming {
                        self.incoming = new;
                        self.incoming_dirs = ancestor_dirs(self.incoming.keys());
                        self.rebuild_tree();
                    }
                }
            }
            Msg::Locks(res) => match res {
                Ok(l) => {
                    self.locks = l;
                    self.locks_by_path = self.locks.iter().enumerate().map(|(i, l)| (l.path.clone(), i)).collect();
                    self.locked_dirs = ancestor_dirs(self.locks.iter().map(|l| &l.path));
                    self.locks_err = None;
                    self.tree.mark_dirty();
                }
                Err(e) => self.locks_err = Some(e),
            },
            Msg::History(gen, res) => {
                if gen == self.history_gen {
                    self.history_loading = false;
                    match res {
                        Ok(c) => {
                            self.graph = build_graph(&c);
                            self.history = c;
                            self.history_err = None;
                        }
                        Err(e) => self.history_err = Some(e),
                    }
                }
            }
            Msg::CommitDetail(res) => match res {
                Ok(d) => {
                    if self.selected_commit.as_deref() == Some(d.commit.id.as_str()) {
                        self.commit_detail = Some(d);
                    }
                }
                Err(e) => self.error(e),
            },
            Msg::Diff(gen, res) => {
                if gen == self.diff_gen {
                    self.diff_loading = false;
                    self.diff = Some(res);
                    self.textures_stale = true;
                }
            }
            Msg::Shelves(res) => match res {
                Ok(s) => {
                    self.shelves = s;
                    if let Some(sel) = &self.selected_shelf {
                        if !self.shelves.iter().any(|s| &s.refname == sel) {
                            self.selected_shelf = None;
                        }
                    }
                }
                Err(e) => self.error(format!("Could not list shelves: {e}")),
            },
            Msg::ShelfFiles(id, res) => match res {
                Ok(f) => {
                    self.shelf_files.insert(id, f);
                }
                Err(e) => self.error(e),
            },
            Msg::Refs(res) => match res {
                Ok(r) => {
                    self.identity = r.identity.clone();
                    if r.default_remote.is_some() {
                        self.remote = r.default_remote.clone();
                    }
                    self.refs = Some(r);
                }
                Err(e) => self.error(format!("Could not list branches: {e}")),
            },
            Msg::Done { title, result, net } => {
                self.on_done(title, result, settings);
                if net {
                    self.request_locks();
                }
                self.after_change();
            }
        }
    }

    fn apply_status(&mut self, s: StatusSnapshot) {
        let files_changed = self.status.as_ref().map(|o| o.files != s.files).unwrap_or(true);
        let head_changed = self.status.as_ref().map(|o| o.branch != s.branch).unwrap_or(true);
        self.status_by_path = s.files.iter().enumerate().map(|(i, f)| (f.path.clone(), i)).collect();
        self.changed_dirs = ancestor_dirs(s.files.iter().map(|f| &f.path));
        self.status = Some(s);
        self.status_err = None;
        if files_changed {
            // Forget changelist entries for files that are no longer pending.
            let live: HashSet<String> = self.pending_paths().into_iter().collect();
            if self.changelists.prune(&live) {
                self.save_changelists();
            }
            self.selection.retain(|p| live.contains(p));
            let untracked_added = self.files().iter().any(|f| f.untracked && !self.tracked.contains(&f.path));
            if untracked_added || self.tree.nodes.is_empty() {
                self.rebuild_tree();
            } else {
                self.tree.mark_dirty();
            }
            // Keep the diff of the selected working file current.
            if let Some(DiffTarget::Working { path }) = self.diff_target.clone() {
                if self.status_by_path.contains_key(&path) {
                    self.request_diff(DiffTarget::Working { path });
                } else {
                    self.clear_diff();
                }
            }
        }
        if head_changed {
            self.tree_stale = true;
            self.request_history();
            self.request_refs();
        }
    }

    fn rebuild_tree(&mut self) {
        let tracked: HashSet<&String> = self.tracked.iter().collect();
        let mut files: Vec<String> = self.tracked.clone();
        for f in self.files() {
            if !tracked.contains(&f.path) {
                files.push(f.path.clone());
            }
        }
        let have: HashSet<String> = files.iter().cloned().collect();
        let ghosts: Vec<String> = self.incoming.iter().filter(|(p, c)| **c != 'D' && !have.contains(*p)).map(|(p, _)| p.clone()).collect();
        self.tree.rebuild(&files, &ghosts);
    }

    /// Something changed in the repository: refresh everything that may be affected.
    fn after_change(&mut self) {
        self.request_status(true);
        self.tree_stale = true;
        self.request_refs();
        self.request_shelves();
        self.request_history();
        // Incoming files depend on HEAD; recompute without hitting the network.
        self.net.submit("Comparing with server", |r| {
            if r.current_op() != RepoOp::None {
                return Msg::Incoming(Err("operation in progress".into()));
            }
            Msg::Incoming(r.incoming_changes().map_err(es))
        });
        self.fetch_inflight = true;
    }

    fn on_done(&mut self, title: String, result: Result<OpOk, String>, settings: &Settings) {
        let ok = match result {
            Ok(ok) => ok,
            Err(e) => {
                self.error(format!("{title} failed:\n{e}"));
                return;
            }
        };
        match ok {
            OpOk::Message(m) => self.toast(m),
            OpOk::Sync(outcome) => self.on_sync(outcome),
            OpOk::Submitted { commit, cl, cl_id, push, unlocked, unlock_failed } => {
                if cl_id != DEFAULT_ID {
                    self.changelists.delete(cl_id);
                    self.save_changelists();
                }
                let short = crate::git::history::cl_label(cl, &commit);
                match push {
                    None => self.toast(format!("Committed {short} locally. Use Push to upload it to the server.")),
                    Some(Ok(m)) => self.toast(format!("Submitted {short}. {m}")),
                    Some(Err(e)) => self.error(format!(
                        "Your change was committed locally ({short}) but could not be uploaded:\n{e}\n\nNothing is lost. Press Push to try again."
                    )),
                }
                if !unlocked.is_empty() {
                    self.toast(format!("Released {} lock(s).", unlocked.len()));
                }
                for (p, e) in unlock_failed {
                    self.error(format!("Could not release lock on {p}: {e}"));
                }
            }
            OpOk::Shelved { shelf, reverted, share, cl_id } => {
                if reverted {
                    if let Some(id) = cl_id.filter(|i| *i != DEFAULT_ID) {
                        self.changelists.delete(id);
                        self.save_changelists();
                    }
                }
                let mut m = format!("Shelved \"{}\"", first_line(&shelf.description));
                if reverted {
                    m.push_str(" and reverted the files");
                }
                match share {
                    Some(Ok(())) => m.push_str(" — shared with the team"),
                    Some(Err(e)) => self.error(format!("Shelf saved locally but could not be shared: {e}")),
                    None => {}
                }
                self.toast(m);
            }
            OpOk::Unshelved { paths, description, backup, new_changelist } => {
                let mut m = if new_changelist {
                    let id = self.changelists.create(&description);
                    self.changelists.move_files(&paths, id);
                    self.save_changelists();
                    format!("Unshelved {} file(s) into changelist \"{}\".", paths.len(), first_line(&description))
                } else {
                    format!("Restored {} file(s).", paths.len())
                };
                if let Some(b) = backup {
                    m.push_str(&format!(" Your previous versions were backed up as shelf \"{b}\"."));
                }
                self.toast(m);
                self.center = CenterTab::Pending;
            }
            OpOk::Locks { verb, ok, failed } => {
                if !ok.is_empty() {
                    self.toast(format!("{verb} {} file(s).", ok.len()));
                }
                for (p, e) in failed {
                    self.error(format!("{p}: {e}"));
                }
            }
            OpOk::BlockedResolved { shelf, sync } => {
                self.toast(format!(
                    "Your edits were shelved as \"{}\". Unshelve them from the Shelves tab when you're ready.",
                    first_line(&shelf.description)
                ));
                self.on_sync(sync);
            }
        }
        let _ = settings;
    }

    fn on_sync(&mut self, outcome: SyncOutcome) {
        match outcome {
            SyncOutcome::UpToDate => self.toast("You already have the latest version."),
            SyncOutcome::Updated { commits } => self.toast(format!("Got latest: {commits} new change(s) from the server.")),
            SyncOutcome::NoUpstream => self.toast("This branch isn't on the server yet — submit or push to publish it."),
            SyncOutcome::Blocked { overlap } => self.dialog = Some(Dialog::SyncBlocked(overlap)),
            SyncOutcome::Conflicts => {
                self.center = CenterTab::Pending;
                self.toast(
                    "Some files were changed both by you and on the server. Resolve the conflicts in the Pending tab, then press Continue.",
                );
            }
        }
    }

    // ------------------------------------------------------------ actions

    pub fn handle(&mut self, action: Action, settings: &Settings) {
        match action {
            Action::Refresh => {
                self.refresh_all();
                self.request_fetch();
            }
            Action::Fetch => self.request_fetch(),
            Action::GetLatest => {
                if self.has_conflicts() || self.op() != RepoOp::None {
                    self.error("Finish resolving the current conflicts first (Pending tab).");
                    return;
                }
                let mode = settings.pull_mode;
                self.local.submit("Getting latest", move |r| {
                    let dirty: Vec<String> = r.status().map(|s| s.files.into_iter().map(|f| f.path).collect()).unwrap_or_default();
                    Msg::Done { title: "Get Latest".into(), result: r.sync(mode, &dirty).map(OpOk::Sync).map_err(es), net: false }
                });
            }
            Action::ShelveOverlapAndSync(paths) => {
                let mode = settings.pull_mode;
                let files: Vec<FileStatus> = self.files().iter().filter(|f| paths.contains(&f.path)).cloned().collect();
                let branch = self.branch_name();
                self.local.submit("Shelving and getting latest", move |r| {
                    let res = (|| {
                        let mut all = paths.clone();
                        all.extend(files.iter().filter_map(|f| f.orig_path.clone()));
                        let shelf = r.create_shelf(&all, &format!("My edits set aside to get latest on {branch}")).map_err(es)?;
                        r.revert_files(&files).map_err(es)?;
                        let dirty: Vec<String> = r.status().map(|s| s.files.into_iter().map(|f| f.path).collect()).unwrap_or_default();
                        let sync = r.sync(mode, &dirty).map_err(es)?;
                        Ok(OpOk::BlockedResolved { shelf, sync })
                    })();
                    Msg::Done { title: "Get Latest".into(), result: res, net: false }
                });
            }
            Action::Push => {
                let mode = settings.pull_mode;
                self.local.submit("Uploading", move |r| Msg::Done {
                    title: "Push".into(),
                    result: push_with_retry(r, mode).map(OpOk::Message),
                    net: false,
                });
            }
            Action::ShowDiff(t) => self.request_diff(t),
            Action::OpenSubmit(cl_id) => self.open_submit(cl_id, settings),
            Action::OpenShelve(paths, cl_id) => {
                let files: Vec<String> = paths.into_iter().filter(|p| self.file(p).is_some()).collect();
                if files.is_empty() {
                    self.error("There are no changed files to shelve.");
                    return;
                }
                let description = cl_id
                    .filter(|i| *i != DEFAULT_ID)
                    .and_then(|i| self.changelists.get(i))
                    .map(|c| c.description.clone())
                    .unwrap_or_default();
                self.dialog = Some(Dialog::Shelve(ShelveDlg {
                    cl_id,
                    description,
                    files: files.into_iter().map(|f| (f, true)).collect(),
                    revert: false,
                    share: false,
                }));
            }
            Action::OpenRevert(paths) => {
                let files: Vec<(String, bool)> =
                    paths.into_iter().filter(|p| self.file(p).is_some_and(|f| f.kind != ChangeKind::Conflict)).map(|p| (p, true)).collect();
                if files.is_empty() {
                    self.error("Nothing to revert: the selected files have no local changes.");
                    return;
                }
                self.dialog = Some(Dialog::Revert(files));
            }
            Action::NewChangelist(files) => {
                self.dialog = Some(Dialog::Changelist { id: None, description: String::new(), move_files: files })
            }
            Action::EditChangelist(id) => {
                if let Some(c) = self.changelists.get(id) {
                    self.dialog = Some(Dialog::Changelist { id: Some(id), description: c.description.clone(), move_files: vec![] });
                }
            }
            Action::DeleteChangelist(id) => {
                self.changelists.delete(id);
                self.save_changelists();
            }
            Action::MoveFiles(paths, to) => {
                self.changelists.move_files(&paths, to);
                self.save_changelists();
            }
            Action::Lock(paths) => {
                if !self.uses_lfs {
                    self.error("File locking needs Git LFS. Ask your repository admin to enable it.");
                    return;
                }
                let paths: Vec<String> = paths.into_iter().filter(|p| self.lock_of(p).is_none()).collect();
                if paths.is_empty() {
                    self.toast("Those files are already locked.");
                    return;
                }
                self.net.submit("Checking out (locking)", move |r| {
                    let (ok, failed) = r.lfs_lock(&paths);
                    Msg::Done { title: "Check out".into(), result: Ok(OpOk::Locks { verb: "Checked out", ok, failed }), net: true }
                });
            }
            Action::Unlock(paths, force) => {
                let mine: Vec<String> = paths.iter().filter(|p| self.lock_of(p).is_some_and(|l| l.ours)).cloned().collect();
                let theirs: Vec<String> = paths.iter().filter(|p| self.lock_of(p).is_some_and(|l| !l.ours)).cloned().collect();
                if !theirs.is_empty() && force {
                    let owners: Vec<String> = theirs
                        .iter()
                        .map(|p| format!("{p} (locked by {})", self.lock_of(p).map(|l| l.owner.clone()).unwrap_or_default()))
                        .collect();
                    self.dialog = Some(Dialog::Confirm {
                        title: "Break someone else's lock?".into(),
                        body: format!(
                            "These files are checked out by someone else. Breaking their lock may cause them to lose work:\n\n{}",
                            owners.join("\n")
                        ),
                        action: Action::DoForceUnlock(theirs),
                    });
                }
                if !mine.is_empty() {
                    let modified: Vec<&String> = mine.iter().filter(|p| self.file(p).is_some()).collect();
                    if !modified.is_empty() {
                        self.toast(format!(
                            "Note: {} of these files still have unsubmitted changes. Others may now edit them.",
                            modified.len()
                        ));
                    }
                    self.net.submit("Releasing locks", move |r| {
                        let (ok, failed) = r.lfs_unlock(&mine, false);
                        Msg::Done { title: "Release lock".into(), result: Ok(OpOk::Locks { verb: "Released", ok, failed }), net: true }
                    });
                }
            }
            Action::DoForceUnlock(paths) => {
                self.net.submit("Breaking locks", move |r| {
                    let (ok, failed) = r.lfs_unlock(&paths, true);
                    Msg::Done { title: "Break lock".into(), result: Ok(OpOk::Locks { verb: "Broke lock on", ok, failed }), net: true }
                });
            }
            Action::OpenFile(p) => crate::git::ops::open_path(&self.repo.abs(&p)),
            Action::RevealFile(p) => crate::git::ops::reveal_path(&self.repo.abs(&p)),
            Action::FileHistory(p) => {
                self.history_scope = HistoryScope::File(p);
                self.center = CenterTab::History;
                self.request_history();
            }
            Action::RevealInTree(p) => {
                self.left = LeftTab::Workspace;
                self.tree.search.clear();
                self.tree.filter = TreeFilter::All;
                self.tree.reveal(&p);
                self.selection = [p].into_iter().collect();
            }
            Action::Resolve(path, side) => {
                let Some(f) = self.file(&path).cloned() else { return };
                let op = self.op();
                self.local.submit("Resolving", move |r| Msg::Done {
                    title: "Resolve".into(),
                    result: r.resolve_take(&f, side, op).map(|_| OpOk::Message(format!("Resolved {}", f.path))).map_err(es),
                    net: false,
                });
            }
            Action::MarkResolved(path) => {
                self.local.submit("Resolving", move |r| Msg::Done {
                    title: "Mark resolved".into(),
                    result: r.mark_resolved(&path).map(|_| OpOk::Message(format!("Marked {path} as resolved"))).map_err(es),
                    net: false,
                });
            }
            Action::MergeTool(path) => {
                self.local.submit("Waiting for merge tool", move |r| Msg::Done {
                    title: "Merge tool".into(),
                    result: r.run_mergetool(&path).map(|_| OpOk::Message(format!("Merged {path}"))).map_err(es),
                    net: false,
                });
            }
            Action::ContinueOp => {
                let op = self.op();
                if self.has_conflicts() {
                    self.error("Resolve every conflicted file first.");
                    return;
                }
                self.local.submit("Continuing", move |r| {
                    let res = r.continue_op(op).map_err(es).and_then(|_| {
                        let st = r.status().map_err(es)?;
                        if st.has_conflicts() {
                            Ok(OpOk::Sync(SyncOutcome::Conflicts))
                        } else {
                            Ok(OpOk::Message(format!("Finished the {}.", op.label())))
                        }
                    });
                    Msg::Done { title: "Continue".into(), result: res, net: false }
                });
            }
            Action::AbortOp => {
                let op = self.op();
                self.dialog = Some(Dialog::Confirm {
                    title: format!("Abort the {}?", op.label()),
                    body: "This puts everything back the way it was before you started getting latest / merging. Your own committed and uncommitted work is kept.".into(),
                    action: Action::DoAbortOp,
                });
            }
            Action::DoAbortOp => {
                let op = self.op();
                self.local.submit("Aborting", move |r| Msg::Done {
                    title: "Abort".into(),
                    result: r.abort_op(op).map(|_| OpOk::Message("Back to where you were.".into())).map_err(es),
                    net: false,
                });
            }
            Action::Switch(target) => {
                let dirty = self.files().iter().any(|f| !f.submodule);
                if dirty {
                    self.dialog = Some(Dialog::SwitchDirty(target));
                } else {
                    self.handle(Action::SwitchNow(target, false), settings);
                }
            }
            Action::SwitchNow(target, shelve_first) => {
                let files: Vec<FileStatus> = self.files().iter().filter(|f| !f.submodule).cloned().collect();
                let from = self.branch_name();
                self.local.submit("Switching branch", move |r| {
                    let res = (|| {
                        if shelve_first && !files.is_empty() {
                            let mut paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
                            paths.extend(files.iter().filter_map(|f| f.orig_path.clone()));
                            r.create_shelf(&paths, &wip_description(&from)).map_err(es)?;
                            r.revert_files(&files).map_err(es)?;
                        }
                        match &target {
                            SwitchTarget::Local(b) => r.switch_branch(b).map_err(es)?,
                            SwitchTarget::Remote(b) => r.checkout_remote_branch(b).map_err(es)?,
                        }
                        let name = match &target {
                            SwitchTarget::Local(b) | SwitchTarget::Remote(b) => b.clone(),
                        };
                        Ok(OpOk::Message(if shelve_first {
                            format!("Switched to {name}. Your changes were shelved and will be offered back when you return to {from}.")
                        } else {
                            format!("Switched to {name}.")
                        }))
                    })();
                    Msg::Done { title: "Switch branch".into(), result: res, net: false }
                });
            }
            Action::NewBranch(start) => {
                self.dialog = Some(Dialog::NewBranch { name: String::new(), start, switch: true });
            }
            Action::DeleteBranch(name, _) => {
                self.dialog = Some(Dialog::Confirm {
                    title: format!("Delete branch {name}?"),
                    body: "The branch is only deleted if all of its work has been merged somewhere else.".into(),
                    action: Action::DoDeleteBranch(name, false),
                });
            }
            Action::DoDeleteBranch(name, force) => {
                self.local.submit("Deleting branch", move |r| {
                    let res = r.delete_branch(&name, force).map(|_| OpOk::Message(format!("Deleted branch {name}"))).map_err(|e| {
                        if e.stderr.contains("not fully merged") {
                            format!("{name} has work that isn't merged anywhere. Merge it first, or delete it from the branch's menu with 'Delete (discard unmerged work)'.")
                        } else {
                            es(e)
                        }
                    });
                    Msg::Done { title: "Delete branch".into(), result: res, net: false }
                });
            }
            Action::DeleteRemoteBranch(name) => {
                self.dialog = Some(Dialog::Confirm {
                    title: format!("Delete {name} from the server?"),
                    body: "This removes the branch for everyone on the team.".into(),
                    action: Action::DoDeleteRemoteBranch(name),
                });
            }
            Action::DoDeleteRemoteBranch(name) => {
                self.local.submit("Deleting remote branch", move |r| Msg::Done {
                    title: "Delete remote branch".into(),
                    result: r.delete_remote_branch(&name).map(|_| OpOk::Message(format!("Deleted {name} from the server"))).map_err(es),
                    net: false,
                });
            }
            Action::MergeBranch(name) => {
                let cur = self.branch_name();
                self.dialog = Some(Dialog::Confirm {
                    title: format!("Merge {name} into {cur}?"),
                    body: format!("All changes from {name} will be brought into {cur}. If the same files were changed on both, you'll be asked to resolve them."),
                    action: Action::DoMerge(name),
                });
            }
            Action::DoMerge(name) => {
                self.local.submit("Merging", move |r| {
                    let res = match r.merge_into_current(&name) {
                        Ok(()) => Ok(OpOk::Message(format!("Merged {name}."))),
                        Err(e) => match r.status() {
                            Ok(s) if s.has_conflicts() => Ok(OpOk::Sync(SyncOutcome::Conflicts)),
                            _ => Err(es(e)),
                        },
                    };
                    Msg::Done { title: "Merge".into(), result: res, net: false }
                });
            }
            Action::Confirm { action, .. } => self.handle(*action, settings),
            Action::SelectCommit(id) => {
                if self.selected_commit.as_ref() != Some(&id) {
                    self.clear_diff();
                }
                self.selected_commit = Some(id.clone());
                self.commit_detail = None;
                self.local.submit("Loading change", move |r| Msg::CommitDetail(r.commit_detail(&id).map_err(es)));
            }
            Action::RevertCommit(id, merge) => {
                self.dialog = Some(Dialog::Confirm {
                    title: "Back out this change?".into(),
                    body: "Creates a new change that undoes it. History is kept, so this is safe even after it was submitted.".into(),
                    action: Action::DoRevertCommit(id, merge),
                });
            }
            Action::DoRevertCommit(id, merge) => {
                self.local.submit("Backing out change", move |r| {
                    let res = match r.revert_commit(&id, merge) {
                        Ok(()) => Ok(OpOk::Message("Created a change that backs it out. Push to share it.".into())),
                        Err(e) => match r.status() {
                            Ok(s) if s.has_conflicts() => Ok(OpOk::Sync(SyncOutcome::Conflicts)),
                            _ => Err(es(e)),
                        },
                    };
                    Msg::Done { title: "Back out".into(), result: res, net: false }
                });
            }
            Action::CherryPick(id) => {
                self.local.submit("Copying change", move |r| {
                    let res = match r.cherry_pick(&id) {
                        Ok(()) => Ok(OpOk::Message("Copied the change onto this branch.".into())),
                        Err(e) => match r.status() {
                            Ok(s) if s.has_conflicts() => Ok(OpOk::Sync(SyncOutcome::Conflicts)),
                            _ => Err(es(e)),
                        },
                    };
                    Msg::Done { title: "Cherry-pick".into(), result: res, net: false }
                });
            }
            Action::UndoLastCommit => {
                self.dialog = Some(Dialog::Confirm {
                    title: "Undo last commit?".into(),
                    body: "The commit is removed but all of its changes stay in your workspace, ready to fix and submit again.".into(),
                    action: Action::DoUndoLastCommit,
                });
            }
            Action::DoUndoLastCommit => {
                self.local.submit("Undoing commit", |r| Msg::Done {
                    title: "Undo commit".into(),
                    result: r
                        .undo_last_commit()
                        .map(|_| OpOk::Message("Commit undone; its changes are back in your workspace.".into()))
                        .map_err(es),
                    net: false,
                });
            }
            Action::NewTag(rev) => {
                self.dialog = Some(Dialog::NewTag { rev, name: String::new(), message: String::new(), push: true });
            }
            Action::DeleteTag(name) => {
                self.local.submit("Deleting tag", move |r| Msg::Done {
                    title: "Delete tag".into(),
                    result: r.delete_tag(&name).map(|_| OpOk::Message(format!("Deleted tag {name} locally"))).map_err(es),
                    net: false,
                });
            }
            Action::PushTag(name) => {
                let Some(remote) = self.remote.clone().or_else(|| self.repo.default_remote()) else {
                    self.error("No server configured.");
                    return;
                };
                self.local.submit("Pushing tag", move |r| Msg::Done {
                    title: "Push tag".into(),
                    result: r.push_tag(&remote, &name).map(|_| OpOk::Message(format!("Pushed tag {name}"))).map_err(es),
                    net: false,
                });
            }
            Action::GetRevision(rev, path) => {
                self.dialog = Some(Dialog::Confirm {
                    title: format!("Get this revision of {path}?"),
                    body: "Your workspace copy will be replaced with this older version. If you had local edits, they are backed up to a shelf first.".into(),
                    action: Action::DoGetRevision(rev, path),
                });
            }
            Action::DoGetRevision(rev, path) => {
                let dirty = self.file(&path).is_some();
                self.local.submit("Getting revision", move |r| {
                    let res = (|| {
                        let mut backup = None;
                        if dirty {
                            backup = Some(
                                r.create_shelf(
                                    std::slice::from_ref(&path),
                                    &format!("{BACKUP_TAG}before getting older revision of {path}"),
                                )
                                .map_err(es)?,
                            );
                        }
                        r.restore_file_from(&rev, &path).map_err(es)?;
                        Ok(OpOk::Message(match backup {
                            Some(_) => format!("Restored {path}. Your previous edits are in the Shelves tab (backups)."),
                            None => {
                                let label =
                                    r.cl_of(&rev).map(|(id, n)| crate::git::history::cl_label(n, &id)).unwrap_or_else(|_| rev.clone());
                                format!("Restored {path} from {label}.")
                            }
                        }))
                    })();
                    Msg::Done { title: "Get revision".into(), result: res, net: false }
                });
            }
            Action::SelectShelf(refname) => {
                if self.selected_shelf.as_ref() != Some(&refname) {
                    self.clear_diff();
                }
                self.selected_shelf = Some(refname.clone());
                if let Some(s) = self.shelves.iter().find(|s| s.refname == refname).cloned() {
                    if !self.shelf_files.contains_key(&s.id) {
                        self.local.submit("Loading shelf", move |r| Msg::ShelfFiles(s.id.clone(), r.shelf_files(&s).map_err(es)));
                    }
                }
            }
            Action::OpenUnshelve(shelf, preselect) => {
                let Some(files) = self.shelf_files.get(&shelf.id).cloned() else {
                    self.error("Still loading the shelf's files, try again in a moment.");
                    return;
                };
                let files = files
                    .into_iter()
                    .map(|f| {
                        let on = preselect.is_empty() || preselect.contains(&f.path);
                        (f, on)
                    })
                    .collect();
                self.dialog = Some(Dialog::Unshelve(UnshelveDlg { shelf, files }));
            }
            Action::ShareShelf(shelf) => {
                let Some(remote) = self.remote.clone().or_else(|| self.repo.default_remote()) else {
                    self.error("This repository has no server to share with.");
                    return;
                };
                self.local.submit("Sharing shelf", move |r| Msg::Done {
                    title: "Share shelf".into(),
                    result: r
                        .share_shelf(&shelf, &remote)
                        .map(|_| OpOk::Message("Shelf shared. Teammates will see it after their next refresh.".into()))
                        .map_err(es),
                    net: false,
                });
            }
            Action::UnshareShelf(shelf) => {
                let Some(remote) = shelf.remote.clone().or_else(|| self.remote.clone()).or_else(|| self.repo.default_remote()) else {
                    return;
                };
                self.local.submit("Removing shelf from server", move |r| Msg::Done {
                    title: "Unshare shelf".into(),
                    result: r
                        .unshare_shelf(&shelf.name, &remote)
                        .map(|_| OpOk::Message("Shelf removed from the server.".into()))
                        .map_err(es),
                    net: false,
                });
            }
            Action::DeleteShelf(shelf) => {
                let extra = if shelf.shared { "\n\nIt will also be removed from the server." } else { "" };
                self.dialog = Some(Dialog::Confirm {
                    title: "Delete shelf?".into(),
                    body: format!("\"{}\" will be deleted permanently.{extra}", first_line(&shelf.description)),
                    action: Action::DoDeleteShelf(shelf),
                });
            }
            Action::DoDeleteShelf(shelf) => {
                let remote = self.remote.clone().or_else(|| self.repo.default_remote());
                self.local.submit("Deleting shelf", move |r| {
                    let res = (|| {
                        if shelf.remote.is_some() {
                            // Someone else's shared shelf: just forget our copy.
                            r.delete_shelf(&shelf).map_err(es)?;
                            return Ok(OpOk::Message("Removed the shelf from your list.".into()));
                        }
                        if shelf.shared {
                            if let Some(rm) = &remote {
                                r.unshare_shelf(&shelf.name, rm).map_err(es)?;
                            }
                        }
                        r.delete_shelf(&shelf).map_err(es)?;
                        Ok(OpOk::Message("Shelf deleted.".into()))
                    })();
                    Msg::Done { title: "Delete shelf".into(), result: res, net: false }
                });
            }
            Action::StashApply(i, pop) => {
                self.local.submit("Applying stash", move |r| Msg::Done {
                    title: "Apply stash".into(),
                    result: r.stash_apply(i, pop).map(|_| OpOk::Message("Stash applied.".into())).map_err(es),
                    net: false,
                });
            }
            Action::StashDrop(i) => {
                self.local.submit("Dropping stash", move |r| Msg::Done {
                    title: "Drop stash".into(),
                    result: r.stash_drop(i).map(|_| OpOk::Message("Stash dropped.".into())).map_err(es),
                    net: false,
                });
            }
            Action::OpenSubmodule(path) => {
                self.app_requests.push(AppRequest::OpenRepo(self.repo.abs(&path), Some(self.repo.name())));
            }
            Action::UpdateSubmodules => {
                self.local.submit("Updating submodules", |r| Msg::Done {
                    title: "Update submodules".into(),
                    result: r.update_submodules_if_any().map(|_| OpOk::Message("Submodules updated.".into())).map_err(es),
                    net: false,
                });
            }
            Action::RemoveIndexLock => {
                match self.repo.remove_index_lock() {
                    Ok(()) => self.toast("Removed the stale lock file."),
                    Err(e) => self.error(format!("Could not remove the lock: {e}")),
                }
                self.request_status(true);
            }
            Action::SetupIdentity => {
                let id = self.repo.identity();
                self.dialog =
                    Some(Dialog::Identity { name: id.name.unwrap_or_default(), email: id.email.unwrap_or_default(), global: true });
            }
            Action::AddRemote => {
                self.dialog = Some(Dialog::AddRemote { name: "origin".into(), url: String::new() });
            }
            Action::TrackLfs(pattern) => {
                self.local.submit("Tracking with LFS", move |r| Msg::Done {
                    title: "Track with LFS".into(),
                    result: r
                        .lfs_track(&pattern, false)
                        .map(|_| {
                            OpOk::Message(format!("{pattern} files are now stored with Git LFS. Remember to submit .gitattributes too."))
                        })
                        .map_err(es),
                    net: false,
                });
            }
            Action::DoSubmit { paths, message, cl_id, push, unlock } => {
                let mode = settings.pull_mode;
                self.local.submit("Submitting", move |r| {
                    let res = (|| {
                        let commit = r.commit_paths(&paths, &message).map_err(es)?;
                        let push_res = push.then(|| push_with_retry(r, mode));
                        let (unlocked, unlock_failed) = match &push_res {
                            Some(Ok(_)) if !unlock.is_empty() => r.lfs_unlock(&unlock, false),
                            _ => (vec![], vec![]),
                        };
                        // Uploading may have rebased the commit onto newer work, so number what's at HEAD now.
                        let (commit, cl) = r.cl_of("HEAD").unwrap_or((commit, 0));
                        Ok(OpOk::Submitted { commit, cl, cl_id, push: push_res, unlocked, unlock_failed })
                    })();
                    Msg::Done { title: "Submit".into(), result: res, net: true }
                });
                self.selection.clear();
            }
            Action::DoShelve { paths, description, revert, share, cl_id } => {
                let files: Vec<FileStatus> = self.files().iter().filter(|f| paths.contains(&f.path)).cloned().collect();
                let remote = self.remote.clone().or_else(|| self.repo.default_remote());
                self.local.submit("Shelving", move |r| {
                    let res = (|| {
                        let mut all = paths.clone();
                        all.extend(files.iter().filter_map(|f| f.orig_path.clone()));
                        let shelf = r.create_shelf(&all, &description).map_err(es)?;
                        if revert {
                            r.revert_files(&files).map_err(es)?;
                        }
                        let share = share.then(|| match &remote {
                            Some(rm) => r.share_shelf(&shelf, rm).map_err(es),
                            None => Err("no server configured".to_string()),
                        });
                        Ok(OpOk::Shelved { shelf, reverted: revert, share, cl_id })
                    })();
                    Msg::Done { title: "Shelve".into(), result: res, net: false }
                });
            }
            Action::RestoreWip(shelf) => {
                // Bring back work shelved when leaving this branch, then drop the shelf.
                self.local.submit("Restoring your work", move |r| {
                    let res = (|| {
                        let written = r.unshelve(&shelf, None).map_err(es)?;
                        r.delete_shelf(&shelf).map_err(es)?;
                        Ok(OpOk::Unshelved { paths: written, description: shelf.display_name(), backup: None, new_changelist: false })
                    })();
                    Msg::Done { title: "Restore".into(), result: res, net: false }
                });
            }
            Action::DoUnshelve { shelf, paths } => {
                self.local.submit("Unshelving", move |r| {
                    let res = (|| {
                        let dirty: Vec<String> =
                            r.status().map_err(es)?.files.into_iter().filter(|f| paths.contains(&f.path)).map(|f| f.path).collect();
                        let backup = if dirty.is_empty() {
                            None
                        } else {
                            let b = r
                                .create_shelf(&dirty, &format!("{BACKUP_TAG}before unshelving \"{}\"", first_line(&shelf.description)))
                                .map_err(es)?;
                            Some(b.display_name())
                        };
                        let written = r.unshelve(&shelf, Some(&paths)).map_err(es)?;
                        Ok(OpOk::Unshelved { paths: written, description: shelf.display_name(), backup, new_changelist: true })
                    })();
                    Msg::Done { title: "Unshelve".into(), result: res, net: false }
                });
            }
            Action::DoRevert(paths) => {
                let files: Vec<FileStatus> = self.files().iter().filter(|f| paths.contains(&f.path)).cloned().collect();
                let n = files.len();
                self.local.submit("Reverting", move |r| {
                    let res = (|| {
                        let mut all: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
                        all.extend(files.iter().filter_map(|f| f.orig_path.clone()));
                        let non_sub: Vec<String> = files.iter().filter(|f| !f.submodule).map(|f| f.path.clone()).collect();
                        let backup = if non_sub.is_empty() {
                            None
                        } else {
                            Some(r.create_shelf(&all, &format!("{BACKUP_TAG}before reverting {n} file(s)")).map_err(es)?)
                        };
                        r.revert_files(&files).map_err(es)?;
                        Ok(OpOk::Message(match backup {
                            Some(_) => format!("Reverted {n} file(s). A backup was saved in Shelves › Backups in case you need it."),
                            None => format!("Reverted {n} file(s)."),
                        }))
                    })();
                    Msg::Done { title: "Revert".into(), result: res, net: false }
                });
                self.selection.clear();
            }
        }
    }

    /// Runs a simple background operation that reports a message.
    pub fn run_simple(&mut self, label: &str, f: impl FnOnce(&Repo) -> crate::git::GitResult<String> + Send + 'static) {
        let title = label.to_string();
        self.local.submit(label, move |r| Msg::Done { title, result: f(r).map(OpOk::Message).map_err(es), net: false });
    }

    pub fn branch_name(&self) -> String {
        self.status.as_ref().and_then(|s| s.branch.head.clone()).unwrap_or_else(|| "(no branch)".into())
    }

    fn open_submit(&mut self, cl_id: u32, settings: &Settings) {
        if !self.identity.complete() {
            self.identity = self.repo.identity();
        }
        if !self.identity.complete() {
            self.handle(Action::SetupIdentity, settings);
            self.toast("Tell git who you are first — this is shown on every change you submit.");
            return;
        }
        if self.status.as_ref().is_some_and(|s| s.branch.head.is_none() && s.branch.oid.is_some()) {
            self.error("You're not on a branch (you are viewing an old version). Create a branch first so your work isn't lost.");
            self.handle(Action::NewBranch(None), settings);
            return;
        }
        if self.op() != RepoOp::None {
            self.error("Finish resolving the current conflicts first (Pending tab), then press Continue.");
            return;
        }
        let files: Vec<String> = self.cl_files(cl_id).into_iter().filter(|p| self.file(p).is_some()).collect();
        if files.is_empty() {
            self.error("This changelist has no changed files to submit.");
            return;
        }
        let message = if cl_id == DEFAULT_ID {
            String::new()
        } else {
            self.changelists.get(cl_id).map(|c| c.description.clone()).unwrap_or_default()
        };
        let has_remote = self.remote.is_some();
        self.dialog = Some(Dialog::Submit(SubmitDlg {
            cl_id,
            message,
            files: files.into_iter().map(|f| (f, true)).collect(),
            push: settings.push_on_submit && has_remote,
            unlock: settings.unlock_after_submit,
        }));
    }

    /// Shelves left behind when switching away from `branch`.
    pub fn wip_shelf_for_current(&self) -> Option<&Shelf> {
        let b = self.status.as_ref()?.branch.head.clone()?;
        let want = wip_description(&b);
        self.shelves.iter().find(|s| s.remote.is_none() && s.description == want)
    }
}

pub fn wip_description(branch: &str) -> String {
    format!("Work in progress on branch '{branch}'")
}

pub fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().trim().to_string()
}

/// Push; if the server has newer commits, get latest and push again once.
fn push_with_retry(r: &Repo, mode: PullMode) -> Result<String, String> {
    match r.push_current().map_err(es)? {
        PushOutcome::Pushed => Ok("Uploaded to the server.".into()),
        PushOutcome::Rejected(_) => {
            let dirty: Vec<String> = r.status().map(|s| s.files.into_iter().map(|f| f.path).collect()).unwrap_or_default();
            match r.sync(mode, &dirty).map_err(es)? {
                SyncOutcome::Updated { .. } | SyncOutcome::UpToDate => match r.push_current().map_err(es)? {
                    PushOutcome::Pushed => Ok("The server had newer changes; got latest and uploaded.".into()),
                    PushOutcome::Rejected(e) => Err(format!("The server keeps rejecting the upload: {e}")),
                },
                SyncOutcome::Conflicts => {
                    Err("Someone else changed the same files. Resolve the conflicts in the Pending tab, press Continue, then Push.".into())
                }
                SyncOutcome::Blocked { overlap } => Err(format!(
                    "The server has newer versions of files you are still editing ({}). Get Latest, then Push.",
                    overlap.join(", ")
                )),
                SyncOutcome::NoUpstream => Err("This branch has no upstream on the server.".into()),
            }
        }
    }
}

const MAX_IMAGE_SIDE: u32 = 2048;

fn decode_image(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let img =
        if img.width() > MAX_IMAGE_SIDE || img.height() > MAX_IMAGE_SIDE { img.thumbnail(MAX_IMAGE_SIDE, MAX_IMAGE_SIDE) } else { img };
    let rgba = img.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

#[allow(clippy::type_complexity)]
fn load_diff(r: &Repo, target: &DiffTarget, file: Option<&FileStatus>, has_head: bool) -> Result<DiffData, String> {
    // (old rev, old path, new rev or None for working copy, new path)
    let (title, diff, old, new): (String, FileDiff, Option<(String, String)>, Option<(Option<String>, String)>) = match target {
        DiffTarget::Working { path } => {
            let untracked = file.is_some_and(|f| f.untracked);
            let orig = file.and_then(|f| f.orig_path.clone());
            let d = r.diff_worktree(path, orig.as_deref(), untracked, has_head).map_err(es)?;
            let in_head = has_head && file.is_none_or(|f| f.in_head() || f.orig_path.is_some());
            let deleted = file.is_some_and(|f| f.kind == ChangeKind::Delete);
            (
                format!("{path} — workspace vs. latest submitted"),
                d,
                in_head.then(|| ("HEAD".to_string(), orig.unwrap_or_else(|| path.clone()))),
                (!deleted).then(|| (None, path.clone())),
            )
        }
        DiffTarget::Commit { id, parent, file } | DiffTarget::Shelf { id, base: parent, file } => {
            let d = r.diff_revs(parent.as_deref(), id, &file.path, file.old_path.as_deref()).map_err(es)?;
            let old = (file.status != 'A')
                .then(|| parent.clone())
                .flatten()
                .map(|p| (p, file.old_path.clone().unwrap_or_else(|| file.path.clone())));
            let new = (file.status != 'D').then(|| (Some(id.clone()), file.path.clone()));
            let what = if matches!(target, DiffTarget::Shelf { .. }) {
                "shelved version".to_string()
            } else {
                r.cl_of(id).map(|(full, n)| crate::git::history::cl_label(n, &full)).unwrap_or_else(|_| id[..id.len().min(8)].to_string())
            };
            (format!("{} — {what}", file.path), d, old, new)
        }
        DiffTarget::Incoming { path } => {
            let d = r.diff_revs(Some("HEAD"), "@{upstream}", path, None).map_err(es)?;
            (
                format!("{path} — your version vs. newer version on the server"),
                d,
                Some(("HEAD".into(), path.clone())),
                Some((Some("@{upstream}".into()), path.clone())),
            )
        }
    };
    let path = match target {
        DiffTarget::Working { path } | DiffTarget::Incoming { path } => path.clone(),
        DiffTarget::Commit { file, .. } | DiffTarget::Shelf { file, .. } => file.path.clone(),
    };
    let mut data = DiffData { title, path: path.clone(), diff, old_image: None, new_image: None, old_size: None, new_size: None };
    let want_bytes = is_image_path(&path) || data.diff.binary || data.diff.lfs.is_some();
    if want_bytes {
        let old_bytes = old.and_then(|(rev, p)| r.blob_bytes(&rev, &p).ok());
        let new_bytes = new.and_then(|(rev, p)| match rev {
            Some(rev) => r.blob_bytes(&rev, &p).ok(),
            None => std::fs::read(r.abs(&p)).ok(),
        });
        data.old_size = old_bytes.as_ref().map(|b| b.len() as u64);
        data.new_size = new_bytes.as_ref().map(|b| b.len() as u64);
        if is_image_path(&path) {
            data.old_image = old_bytes.as_deref().and_then(decode_image);
            data.new_image = new_bytes.as_deref().and_then(decode_image);
        }
    }
    Ok(data)
}
