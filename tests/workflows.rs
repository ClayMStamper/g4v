//! End-to-end tests of the backend workflows against real git repositories:
//! a bare "server" repository and two clones playing two teammates.

use g4v::changelists::ChangelistStore;
use g4v::git::ops::{PullMode, PushOutcome, Side, SyncOutcome};
use g4v::git::status::{ChangeKind, RepoOp};
use g4v::git::{CmdLog, Repo};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

static ENV: Once = Once::new();

/// Isolates the tests from the developer's own git configuration.
fn isolate() {
    ENV.call_once(|| {
        let dir = std::env::temp_dir().join(format!("g4v-test-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("gitconfig");
        std::fs::write(
            &cfg,
            "[user]\n\tname = Tester\n\temail = tester@example.com\n[init]\n\tdefaultBranch = main\n[core]\n\tautocrlf = false\n[commit]\n\tgpgsign = false\n",
        )
        .unwrap();
        // SAFETY: set once, before any test spawns git, and never modified afterwards.
        unsafe {
            std::env::set_var("GIT_CONFIG_GLOBAL", &cfg);
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        }
    });
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").current_dir(dir).args(args).output().expect("git runs");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct World {
    _tmp: tempfile::TempDir,
    a: Repo,
    b: Repo,
}

fn write(repo: &Repo, rel: &str, content: &str) {
    let p = repo.abs(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn read(repo: &Repo, rel: &str) -> String {
    std::fs::read_to_string(repo.abs(rel)).unwrap()
}

/// Server with one initial commit (a.txt, b.txt, d.txt), cloned twice.
fn world() -> World {
    isolate();
    let tmp = tempfile::tempdir().unwrap();
    let server: PathBuf = tmp.path().join("server.git");
    git(tmp.path(), &["init", "--bare", "-b", "main", server.to_str().unwrap()]);
    let seed = tmp.path().join("seed");
    git(tmp.path(), &["clone", server.to_str().unwrap(), seed.to_str().unwrap()]);
    std::fs::write(seed.join("a.txt"), "a1\n").unwrap();
    std::fs::write(seed.join("b.txt"), "b1\n").unwrap();
    std::fs::write(seed.join("d.txt"), "d1\n").unwrap();
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-m", "initial"]);
    git(&seed, &["push", "-u", "origin", "main"]);
    let pa = tmp.path().join("alice");
    let pb = tmp.path().join("bob");
    git(tmp.path(), &["clone", server.to_str().unwrap(), pa.to_str().unwrap()]);
    git(tmp.path(), &["clone", server.to_str().unwrap(), pb.to_str().unwrap()]);
    git(&pb, &["config", "user.name", "Bob"]);
    let log = CmdLog::default();
    World { a: Repo::open(&pa, log.clone()).unwrap(), b: Repo::open(&pb, log).unwrap(), _tmp: tmp }
}

fn paths(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn submit_commits_only_the_changelist_files() {
    let w = world();
    write(&w.a, "a.txt", "a2\n");
    write(&w.a, "b.txt", "b2\n");
    write(&w.a, "new/c.txt", "c\n");
    std::fs::remove_file(w.a.abs("d.txt")).unwrap();
    // Something staged by another tool must not sneak into the commit.
    git(w.a.root(), &["add", "b.txt"]);

    let st = w.a.status().unwrap();
    assert_eq!(st.files.len(), 4);
    assert_eq!(st.find("new/c.txt").unwrap().kind, ChangeKind::Add);
    assert_eq!(st.find("d.txt").unwrap().kind, ChangeKind::Delete);

    w.a.commit_paths(&paths(&["a.txt", "new/c.txt", "d.txt"]), "Update a, add c, remove d").unwrap();
    let st = w.a.status().unwrap();
    let left: Vec<_> = st.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(left, vec!["b.txt"]);
    let committed = git(w.a.root(), &["show", "--name-status", "--format=", "HEAD"]);
    assert!(committed.contains("a.txt") && committed.contains("new/c.txt") && committed.contains("d.txt"));
    assert!(!committed.contains("b.txt"));
    assert_eq!(w.a.push_current().unwrap(), PushOutcome::Pushed);
}

#[test]
fn empty_description_is_refused() {
    let w = world();
    write(&w.a, "a.txt", "x\n");
    assert!(w.a.commit_paths(&paths(&["a.txt"]), "   ").is_err());
}

#[test]
fn out_of_date_files_are_detected_and_synced() {
    let w = world();
    write(&w.a, "a.txt", "from alice\n");
    w.a.commit_paths(&paths(&["a.txt"]), "alice edit").unwrap();
    w.a.push_current().unwrap();

    w.b.fetch().unwrap();
    assert_eq!(w.b.incoming_files().unwrap(), vec!["a.txt".to_string()]);
    assert_eq!(w.b.status().unwrap().branch.behind, 1);

    // Unrelated local edit does not block getting latest.
    write(&w.b, "b.txt", "bob local\n");
    let out = w.b.sync(PullMode::Rebase, &paths(&["b.txt"])).unwrap();
    assert_eq!(out, SyncOutcome::Updated { commits: 1 });
    assert_eq!(read(&w.b, "a.txt"), "from alice\n");
    assert_eq!(read(&w.b, "b.txt"), "bob local\n");
    assert!(w.b.incoming_files().unwrap().is_empty());
}

#[test]
fn sync_is_blocked_when_local_edits_overlap_incoming() {
    let w = world();
    write(&w.a, "a.txt", "alice\n");
    w.a.commit_paths(&paths(&["a.txt"]), "alice").unwrap();
    w.a.push_current().unwrap();

    write(&w.b, "a.txt", "bob\n");
    let out = w.b.sync(PullMode::Rebase, &paths(&["a.txt"])).unwrap();
    assert_eq!(out, SyncOutcome::Blocked { overlap: paths(&["a.txt"]) });
    // Nothing was touched.
    assert_eq!(read(&w.b, "a.txt"), "bob\n");
}

#[test]
fn rejected_push_then_sync_rebases_local_commit() {
    let w = world();
    write(&w.a, "a.txt", "alice\n");
    w.a.commit_paths(&paths(&["a.txt"]), "alice").unwrap();
    w.a.push_current().unwrap();

    write(&w.b, "b.txt", "bob\n");
    w.b.commit_paths(&paths(&["b.txt"]), "bob").unwrap();
    assert!(matches!(w.b.push_current().unwrap(), PushOutcome::Rejected(_)));
    assert_eq!(w.b.sync(PullMode::Rebase, &[]).unwrap(), SyncOutcome::Updated { commits: 1 });
    assert_eq!(w.b.push_current().unwrap(), PushOutcome::Pushed);
    let log = git(w.b.root(), &["log", "--format=%s", "-3"]);
    assert_eq!(log, "bob\nalice\ninitial");
}

fn conflict_setup(w: &World) {
    write(&w.a, "a.txt", "alice\n");
    w.a.commit_paths(&paths(&["a.txt"]), "alice").unwrap();
    w.a.push_current().unwrap();
    write(&w.b, "a.txt", "bob\n");
    w.b.commit_paths(&paths(&["a.txt"]), "bob").unwrap();
}

#[test]
fn rebase_conflict_keep_mine() {
    let w = world();
    conflict_setup(&w);
    assert_eq!(w.b.sync(PullMode::Rebase, &[]).unwrap(), SyncOutcome::Conflicts);
    let st = w.b.status().unwrap();
    assert_eq!(st.op, RepoOp::Rebase);
    let f = st.conflicts().next().unwrap().clone();
    w.b.resolve_take(&f, Side::Mine, st.op).unwrap();
    w.b.continue_op(st.op).unwrap();
    let st = w.b.status().unwrap();
    assert_eq!(st.op, RepoOp::None);
    assert_eq!(read(&w.b, "a.txt"), "bob\n");
    assert_eq!(st.branch.ahead, 1);
    assert_eq!(st.branch.behind, 0);
}

#[test]
fn merge_conflict_take_theirs() {
    let w = world();
    conflict_setup(&w);
    assert_eq!(w.b.sync(PullMode::Merge, &[]).unwrap(), SyncOutcome::Conflicts);
    let st = w.b.status().unwrap();
    assert_eq!(st.op, RepoOp::Merge);
    let f = st.conflicts().next().unwrap().clone();
    w.b.resolve_take(&f, Side::Theirs, st.op).unwrap();
    w.b.continue_op(st.op).unwrap();
    assert_eq!(read(&w.b, "a.txt"), "alice\n");
    assert_eq!(w.b.status().unwrap().op, RepoOp::None);
}

#[test]
fn rebase_conflict_take_theirs_skips_empty_commit() {
    let w = world();
    conflict_setup(&w);
    assert_eq!(w.b.sync(PullMode::Rebase, &[]).unwrap(), SyncOutcome::Conflicts);
    let st = w.b.status().unwrap();
    let f = st.conflicts().next().unwrap().clone();
    w.b.resolve_take(&f, Side::Theirs, st.op).unwrap();
    w.b.continue_op(st.op).unwrap();
    let st = w.b.status().unwrap();
    assert_eq!(st.op, RepoOp::None);
    assert_eq!(read(&w.b, "a.txt"), "alice\n");
    assert_eq!(st.branch.ahead, 0);
}

#[test]
fn abort_restores_previous_state() {
    let w = world();
    conflict_setup(&w);
    assert_eq!(w.b.sync(PullMode::Rebase, &[]).unwrap(), SyncOutcome::Conflicts);
    w.b.abort_op(RepoOp::Rebase).unwrap();
    assert_eq!(w.b.status().unwrap().op, RepoOp::None);
    assert_eq!(read(&w.b, "a.txt"), "bob\n");
}

#[test]
fn shelve_revert_unshelve_roundtrip() {
    let w = world();
    write(&w.a, "a.txt", "shelved edit\n");
    write(&w.a, "art/new.txt", "brand new\n");
    std::fs::remove_file(w.a.abs("d.txt")).unwrap();
    let files = paths(&["a.txt", "art/new.txt", "d.txt"]);

    let shelf = w.a.create_shelf(&files, "WIP hero").unwrap();
    // Shelving does not touch the working copy.
    assert_eq!(read(&w.a, "a.txt"), "shelved edit\n");

    let st = w.a.status().unwrap();
    w.a.revert_files(&st.files).unwrap();
    assert!(w.a.status().unwrap().files.is_empty());
    assert!(!w.a.abs("art/new.txt").exists());
    assert!(!w.a.abs("art").exists(), "empty folder left behind");

    let listed = w.a.list_shelves().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].description, "WIP hero");
    let shelved_files = w.a.shelf_files(&listed[0]).unwrap();
    assert_eq!(shelved_files.len(), 3);

    let written = w.a.unshelve(&shelf, None).unwrap();
    assert_eq!(written.len(), 3);
    assert_eq!(read(&w.a, "a.txt"), "shelved edit\n");
    assert_eq!(read(&w.a, "art/new.txt"), "brand new\n");
    assert!(!w.a.abs("d.txt").exists());
    let st = w.a.status().unwrap();
    assert_eq!(st.find("art/new.txt").unwrap().kind, ChangeKind::Add);
    assert_eq!(st.find("d.txt").unwrap().kind, ChangeKind::Delete);
    // No commits were created on the branch.
    assert_eq!(st.branch.ahead, 0);

    w.a.delete_shelf(&shelf).unwrap();
    assert!(w.a.list_shelves().unwrap().is_empty());
}

#[test]
fn shelves_can_be_shared_with_teammates() {
    let w = world();
    write(&w.a, "a.txt", "for review\n");
    let shelf = w.a.create_shelf(&paths(&["a.txt"]), "Please review").unwrap();
    w.a.share_shelf(&shelf, "origin").unwrap();
    let mine = w.a.list_shelves().unwrap();
    assert_eq!(mine.len(), 1, "local + mirrored remote copy should collapse into one entry");
    assert!(mine[0].shared);

    w.b.fetch().unwrap();
    let theirs = w.b.list_shelves().unwrap();
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0].remote.as_deref(), Some("origin"));
    w.b.unshelve(&theirs[0], None).unwrap();
    assert_eq!(read(&w.b, "a.txt"), "for review\n");

    w.a.unshare_shelf(&shelf.name, "origin").unwrap();
    w.b.fetch().unwrap();
    assert!(w.b.list_shelves().unwrap().is_empty());
}

#[test]
fn revert_handles_every_kind_of_change() {
    let w = world();
    write(&w.a, "a.txt", "edited\n");
    write(&w.a, "untracked.txt", "u\n");
    write(&w.a, "staged_new.txt", "s\n");
    git(w.a.root(), &["add", "staged_new.txt"]);
    std::fs::remove_file(w.a.abs("d.txt")).unwrap();
    git(w.a.root(), &["mv", "b.txt", "b_moved.txt"]);
    let st = w.a.status().unwrap();
    assert!(st.files.iter().any(|f| f.kind == ChangeKind::Move));
    w.a.revert_files(&st.files).unwrap();
    let st = w.a.status().unwrap();
    assert!(st.files.is_empty(), "left over: {:?}", st.files);
    assert_eq!(read(&w.a, "a.txt"), "a1\n");
    assert_eq!(read(&w.a, "b.txt"), "b1\n");
    assert_eq!(read(&w.a, "d.txt"), "d1\n");
}

#[test]
fn special_characters_in_paths_are_literal() {
    let w = world();
    // `*` is not allowed in Windows file names; brackets are still glob syntax.
    let weird = if cfg!(windows) { "art/hero [final] v2 #1.txt" } else { "art/hero [final] *v2*.txt" };
    write(&w.a, weird, "x\n");
    write(&w.a, "art/hero f.txt", "must not be matched by a glob\n");
    let shelf = w.a.create_shelf(&paths(&[weird]), "weird").unwrap();
    assert_eq!(w.a.shelf_files(&shelf).unwrap().len(), 1);
    w.a.commit_paths(&paths(&[weird]), "weird name").unwrap();
    let st = w.a.status().unwrap();
    assert_eq!(st.files.len(), 1);
    assert_eq!(st.files[0].path, "art/hero f.txt");
}

#[test]
fn first_commit_in_new_repository() {
    isolate();
    let tmp = tempfile::tempdir().unwrap();
    let log = CmdLog::default();
    g4v::git::ops::init(tmp.path(), &log).unwrap();
    let r = Repo::open(tmp.path(), log).unwrap();
    assert!(!r.has_head());
    write(&r, "readme.txt", "hello\n");
    write(&r, "later.txt", "later\n");
    let shelf = r.create_shelf(&paths(&["readme.txt"]), "before first commit").unwrap();
    assert!(shelf.base.is_none());
    r.commit_paths(&paths(&["readme.txt"]), "First").unwrap();
    assert!(r.has_head());
    let st = r.status().unwrap();
    assert_eq!(st.files.len(), 1);
    assert_eq!(st.branch.head.as_deref(), Some("main"));
}

#[test]
fn branches_create_switch_delete() {
    let w = world();
    w.a.create_branch("feature/x", None, true).unwrap();
    assert_eq!(w.a.status().unwrap().branch.head.as_deref(), Some("feature/x"));
    assert!(w.a.branch_name_problem("bad name").is_some());
    assert!(w.a.branch_name_problem("good-name").is_none());
    write(&w.a, "f.txt", "f\n");
    w.a.commit_paths(&paths(&["f.txt"]), "feature work").unwrap();
    w.a.switch_branch("main").unwrap();
    // Unmerged work is protected by the safe delete.
    assert!(w.a.delete_branch("feature/x", false).is_err());
    w.a.merge_into_current("feature/x").unwrap();
    w.a.delete_branch("feature/x", false).unwrap();
    let names: Vec<String> = w.a.branches().unwrap().into_iter().map(|b| b.name).collect();
    assert!(names.contains(&"main".to_string()));
    assert!(names.contains(&"origin/main".to_string()));
    assert!(!names.contains(&"feature/x".to_string()));
}

#[test]
fn publishing_a_new_branch_sets_upstream() {
    let w = world();
    w.a.create_branch("topic", None, true).unwrap();
    write(&w.a, "t.txt", "t\n");
    w.a.commit_paths(&paths(&["t.txt"]), "topic").unwrap();
    assert!(w.a.status().unwrap().branch.upstream.is_none());
    assert_eq!(w.a.push_current().unwrap(), PushOutcome::Pushed);
    assert_eq!(w.a.status().unwrap().branch.upstream.as_deref(), Some("origin/topic"));
}

#[test]
fn history_and_commit_details() {
    let w = world();
    write(&w.a, "a.txt", "2\n");
    w.a.commit_paths(&paths(&["a.txt"]), "Second\n\nLonger body").unwrap();
    let log = w.a.log(&g4v::git::history::HistoryScope::Current, 100, true).unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].subject, "Second");
    let d = w.a.commit_detail(&log[0].id).unwrap();
    assert!(d.message.contains("Longer body"));
    assert_eq!(d.files.len(), 1);
    let root = w.a.commit_detail(&log[1].id).unwrap();
    assert_eq!(root.files.len(), 3);
    let diff = w.a.diff_revs(Some(&log[1].id), &log[0].id, "a.txt", None).unwrap();
    assert_eq!((diff.adds, diff.dels), (1, 1));
    let file_log = w.a.log(&g4v::git::history::HistoryScope::File("a.txt".into()), 100, false).unwrap();
    assert_eq!(file_log.len(), 2);
}

#[test]
fn undo_last_commit_keeps_changes() {
    let w = world();
    write(&w.a, "a.txt", "oops\n");
    w.a.commit_paths(&paths(&["a.txt"]), "wrong").unwrap();
    w.a.undo_last_commit().unwrap();
    let st = w.a.status().unwrap();
    assert_eq!(st.branch.ahead, 0);
    assert_eq!(st.find("a.txt").unwrap().kind, ChangeKind::Edit);
    assert_eq!(read(&w.a, "a.txt"), "oops\n");
}

#[test]
fn changelists_persist_in_git_dir() {
    let w = world();
    let mut s = ChangelistStore::load(&w.a.meta_dir());
    let id = s.create("Textures");
    s.move_files(&["a.txt"], id);
    s.save().unwrap();
    let s2 = ChangelistStore::load(&w.a.meta_dir());
    assert_eq!(s2.list_of("a.txt"), id);
    // Metadata lives inside .git, so it never shows up as a change.
    assert!(w.a.status().unwrap().files.is_empty());
}
