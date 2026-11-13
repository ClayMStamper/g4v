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
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
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
fn stale_revert_does_not_delete_a_file_committed_by_another_tool() {
    let w = world();
    write(&w.a, "new.txt", "keep me\n");
    let stale = w.a.status().unwrap();
    git(w.a.root(), &["add", "new.txt"]);
    git(w.a.root(), &["commit", "-m", "committed outside g4v"]);
    w.a.revert_files(&stale.files).unwrap();
    assert_eq!(read(&w.a, "new.txt"), "keep me\n");
    assert!(w.a.status().unwrap().files.is_empty());
}

#[test]
fn revert_and_submit_preserve_versions_that_only_exist_in_the_index() {
    let w = world();
    for submit in [false, true] {
        write(&w.a, "a.txt", "staged-only artwork\n");
        git(w.a.root(), &["add", "a.txt"]);
        write(&w.a, "a.txt", "newer working artwork\n");
        if submit {
            w.a.commit_paths(&paths(&["a.txt"]), "working version").unwrap();
        } else {
            w.a.revert_files(&w.a.status().unwrap().files).unwrap();
        }
        let shelves = w.a.list_shelves().unwrap();
        assert!(shelves.iter().filter(|s| s.is_backup()).any(|s| { w.a.blob_bytes(&s.id, "a.txt").unwrap() == b"staged-only artwork\n" }));
    }
}

#[test]
fn submit_does_not_implicitly_resolve_conflicts() {
    let w = world();
    w.a.create_branch("other", None, true).unwrap();
    write(&w.a, "a.txt", "other\n");
    w.a.commit_paths(&paths(&["a.txt"]), "other").unwrap();
    w.a.switch_branch("main").unwrap();
    write(&w.a, "a.txt", "main\n");
    w.a.commit_paths(&paths(&["a.txt"]), "main").unwrap();
    assert!(w.a.merge_into_current("other").is_err());
    let before = git(w.a.root(), &["ls-files", "--unmerged"]);
    assert!(w.a.commit_paths(&paths(&["a.txt"]), "accidental submit").is_err());
    assert_eq!(git(w.a.root(), &["ls-files", "--unmerged"]), before);
}

#[test]
fn resolving_and_aborting_keep_manual_conflict_work_recoverable() {
    let w = world();
    w.a.create_branch("other", None, true).unwrap();
    write(&w.a, "a.txt", "other\n");
    w.a.commit_paths(&paths(&["a.txt"]), "other").unwrap();
    w.a.switch_branch("main").unwrap();
    write(&w.a, "a.txt", "main\n");
    w.a.commit_paths(&paths(&["a.txt"]), "main").unwrap();
    assert!(w.a.merge_into_current("other").is_err());
    write(&w.a, "a.txt", "manual resolution in progress\n");
    let status = w.a.status().unwrap();
    w.a.resolve_take(status.find("a.txt").unwrap(), Side::Mine, RepoOp::Merge).unwrap();
    write(&w.a, "a.txt", "more work before abort\n");
    w.a.abort_op(RepoOp::Merge).unwrap();
    let backups = w.a.list_shelves().unwrap();
    for expected in [b"manual resolution in progress\n".as_slice(), b"more work before abort\n".as_slice()] {
        assert!(backups.iter().filter(|s| s.is_backup()).any(|s| w.a.blob_bytes(&s.id, "a.txt").unwrap() == expected));
    }
}

#[test]
fn stale_shelf_delete_does_not_delete_a_replacement() {
    let w = world();
    write(&w.a, "a.txt", "first\n");
    let first = w.a.create_shelf(&paths(&["a.txt"]), "first").unwrap();
    write(&w.a, "a.txt", "second\n");
    let second = w.a.create_shelf(&paths(&["a.txt"]), "second").unwrap();
    git(w.a.root(), &["update-ref", &first.refname, &second.id]);
    assert!(w.a.delete_shelf(&first).is_err());
    assert_eq!(git(w.a.root(), &["rev-parse", &first.refname]), second.id);
    assert_eq!(w.a.prune_backups(0).unwrap(), 0);
    assert_eq!(w.a.prune_backups(i64::MAX).unwrap(), 0);
}

#[test]
fn very_large_text_diff_is_bounded_without_blocking_git() {
    let w = world();
    write(&w.a, "a.txt", &"large text line\n".repeat(400_000));
    let diff = w.a.diff_worktree("a.txt", None, false, true).unwrap();
    assert!(diff.truncated);
    assert!(diff.lines.len() <= 20_000);
    assert!(read(&w.a, "a.txt").len() > 4 * 1024 * 1024);
}

#[test]
fn worker_reports_panics_and_continues_the_queue() {
    let w = world();
    let worker = g4v::worker::Worker::new(w.a.clone(), "test", || {});
    worker.submit("failing job", |_| -> u32 { panic!("injected failure") });
    worker.submit("next job", |_| 42);
    let start = std::time::Instant::now();
    while worker.busy() && start.elapsed() < std::time::Duration::from_secs(10) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!worker.busy());
    assert_eq!(worker.poll(), vec![42]);
    let errors = worker.poll_errors();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("failing job"));
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

#[test]
fn changelist_numbers_follow_branch_order_and_match_between_clones() {
    use g4v::git::history::HistoryScope;
    let w = world();
    // Alice submits two changes: CL 2 and CL 3 (the initial commit is CL 1).
    for (i, text) in ["a2\n", "a3\n"].iter().enumerate() {
        write(&w.a, "a.txt", text);
        w.a.commit_paths(&paths(&["a.txt"]), &format!("alice {i}")).unwrap();
    }
    let log = w.a.log(&HistoryScope::Current, 100, true).unwrap();
    assert_eq!(log.iter().map(|c| c.cl).collect::<Vec<_>>(), vec![3, 2, 1]);
    assert_eq!(log[0].cl_label(), "CL #3");
    assert_eq!(w.a.push_current().unwrap(), PushOutcome::Pushed);

    // Bob's local change is CL 2 until he gets latest; rebasing it onto Alice's work makes it CL 4.
    write(&w.b, "b.txt", "b2\n");
    w.b.commit_paths(&paths(&["b.txt"]), "bob").unwrap();
    assert_eq!(w.b.cl_of("HEAD").unwrap().1, 2);
    w.b.sync(PullMode::Rebase, &[]).unwrap();
    let (head, n) = w.b.cl_of("HEAD").unwrap();
    assert_eq!(n, 4);
    let blog = w.b.log(&HistoryScope::Current, 100, true).unwrap();
    assert_eq!(blog.iter().map(|c| c.cl).collect::<Vec<_>>(), vec![4, 3, 2, 1]);
    assert_eq!(blog[0].id, head);

    // The same commit has the same number in every clone and in every view.
    let alice_ids: Vec<(String, u64)> = log.iter().map(|c| (c.id.clone(), c.cl)).collect();
    for (id, cl) in &alice_ids {
        assert_eq!(blog.iter().find(|c| &c.id == id).map(|c| c.cl), Some(*cl));
        assert_eq!(w.b.commit_detail(id).unwrap().commit.cl, *cl);
    }
    // A short window (the parent isn't loaded) and file history still get the right numbers.
    let window = w.b.log(&HistoryScope::Current, 1, false).unwrap();
    assert_eq!(window[0].cl, 4);
    let file_log = w.b.log(&HistoryScope::File("a.txt".into()), 100, false).unwrap();
    assert_eq!(file_log.iter().map(|c| c.cl).collect::<Vec<_>>(), vec![3, 2, 1]);
}

// ------------------------------------------------------------------ hardening regressions

#[test]
fn moved_file_is_submitted_as_one_change() {
    let w = world();
    git(w.a.root(), &["mv", "b.txt", "b_moved.txt"]);
    w.a.commit_paths(&paths(&["b_moved.txt", "b.txt"]), "Move b").unwrap();
    assert!(w.a.status().unwrap().files.is_empty(), "the old path must not be left behind as a pending delete");
    let committed = git(w.a.root(), &["show", "--name-status", "--format=", "--no-renames", "HEAD"]);
    assert!(committed.contains("D\tb.txt") && committed.contains("A\tb_moved.txt"), "{committed}");
}

#[test]
fn files_that_vanished_do_not_break_a_batch() {
    let w = world();
    write(&w.a, "a.txt", "edited\n");
    // Added by another tool, then deleted again before g4v got to it: it exists neither on disk nor in HEAD.
    write(&w.a, "temp_render.txt", "x\n");
    git(w.a.root(), &["add", "temp_render.txt"]);
    std::fs::remove_file(w.a.abs("temp_render.txt")).unwrap();
    let st = w.a.status().unwrap();
    let all: Vec<String> = st.files.iter().map(|f| f.path.clone()).collect();
    assert!(all.contains(&"temp_render.txt".to_string()));

    let shelf = w.a.create_shelf(&all, "with a vanished file").unwrap();
    let shelved: Vec<String> = w.a.shelf_files(&shelf).unwrap().into_iter().map(|f| f.path).collect();
    assert_eq!(shelved, vec!["a.txt"]);
    w.a.revert_files(&st.files).unwrap();
    assert!(w.a.status().unwrap().files.is_empty());

    write(&w.a, "a.txt", "edited again\n");
    w.a.commit_paths(&paths(&["a.txt", "never_existed.txt"]), "Edit a").unwrap();
    assert!(w.a.status().unwrap().files.is_empty());
}

#[test]
fn shelves_made_in_the_same_instant_never_replace_each_other() {
    let w = world();
    let mut refs = std::collections::HashSet::new();
    for i in 0..6 {
        write(&w.a, "a.txt", &format!("version {i}\n"));
        refs.insert(w.a.create_shelf(&paths(&["a.txt"]), &format!("shelf {i}")).unwrap().refname);
    }
    assert_eq!(refs.len(), 6);
    let shelves = w.a.list_shelves().unwrap();
    assert_eq!(shelves.len(), 6);
    // An existing shelf ref can't be silently replaced: git itself refuses.
    let existing = &shelves[0];
    let other = &shelves[1];
    let clash = Command::new("git")
        .current_dir(w.a.root())
        .args(["update-ref", existing.refname.as_str(), other.id.as_str(), ""])
        .output()
        .unwrap();
    assert!(!clash.status.success());
    assert_eq!(git(w.a.root(), &["rev-parse", existing.refname.as_str()]), existing.id);
}

#[test]
fn get_latest_checks_the_workspace_itself_for_overlap() {
    let w = world();
    write(&w.b, "a.txt", "bob\n");
    w.b.commit_paths(&paths(&["a.txt"]), "Bob edits a").unwrap();
    assert_eq!(w.b.push_current().unwrap(), PushOutcome::Pushed);
    write(&w.a, "a.txt", "alice, unsaved\n");
    // Even when the caller passes a stale (empty) list of local edits, they must not be overwritten.
    match w.a.sync(PullMode::Rebase, &[]).unwrap() {
        SyncOutcome::Blocked { overlap } => assert_eq!(overlap, vec!["a.txt"]),
        other => panic!("expected Blocked, got {other:?}"),
    }
    assert_eq!(read(&w.a, "a.txt"), "alice, unsaved\n");
}

#[test]
fn large_batches_round_trip() {
    let w = world();
    let n = 400;
    let all: Vec<String> = (0..n).map(|i| format!("batch/dir{:02}/file [{i}].txt", i % 7)).collect();
    for (i, p) in all.iter().enumerate() {
        write(&w.a, p, &format!("content {i}\n"));
    }
    let st = w.a.status().unwrap();
    assert_eq!(st.files.len(), n);
    let shelf = w.a.create_shelf(&all, "big batch").unwrap();
    assert_eq!(w.a.shelf_files(&shelf).unwrap().len(), n);
    w.a.revert_files(&st.files).unwrap();
    assert!(w.a.status().unwrap().files.is_empty());
    assert!(!w.a.abs("batch").exists(), "empty folders are cleaned up");
    assert_eq!(w.a.unshelve(&shelf, Some(&all[..n / 2])).unwrap().len(), n / 2);
    assert_eq!(w.a.status().unwrap().files.len(), n / 2);
    w.a.unshelve(&shelf, None).unwrap();
    assert_eq!(read(&w.a, &all[n - 1]), format!("content {}\n", n - 1));
    w.a.commit_paths(&all, "Add a big batch").unwrap();
    assert!(w.a.status().unwrap().files.is_empty());
    assert_eq!(w.a.push_current().unwrap(), PushOutcome::Pushed);
    assert_eq!(w.b.sync(PullMode::Rebase, &[]).unwrap(), SyncOutcome::Updated { commits: 1 });
    assert_eq!(read(&w.b, &all[0]), "content 0\n");
}

// ------------------------------------------------------------------ the tab's own workflows, driven without a window

mod tab_flows {
    use super::*;
    use g4v::settings::Settings;
    use g4v::ui::tab::{wip_description, Action, Dialog, RepoTab};
    use std::time::{Duration, Instant};

    fn settings() -> Settings {
        Settings { fetch_interval_secs: 0, ..Default::default() }
    }

    /// Runs the tab's background work until everything queued has finished and been applied.
    fn settle(tab: &mut RepoTab, ctx: &eframe::egui::Context) {
        let s = settings();
        let start = Instant::now();
        let mut quiet = 0;
        while quiet < 4 {
            assert!(start.elapsed() < Duration::from_secs(120), "background work did not finish");
            tab.tick(ctx, &s);
            quiet = if tab.is_busy() || tab.net_busy() || tab.status.is_none() { 0 } else { quiet + 1 };
            std::thread::sleep(Duration::from_millis(25));
        }
        tab.handle(Action::Refresh, &s);
        let mut quiet = 0;
        while quiet < 4 {
            assert!(start.elapsed() < Duration::from_secs(120), "background work did not finish");
            tab.tick(ctx, &s);
            quiet = if tab.is_busy() || tab.net_busy() { 0 } else { quiet + 1 };
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn open(repo: &Repo) -> (RepoTab, eframe::egui::Context) {
        let ctx = eframe::egui::Context::default();
        let mut tab = RepoTab::new(repo.clone(), &ctx, None);
        settle(&mut tab, &ctx);
        (tab, ctx)
    }

    fn errors(tab: &RepoTab) -> Vec<String> {
        tab.toasts.iter().filter(|t| t.error).map(|t| t.text.clone()).collect()
    }

    #[test]
    fn restoring_shelved_work_backs_up_newer_edits() {
        let w = world();
        write(&w.a, "a.txt", "work in progress\n");
        let shelf = w.a.create_shelf(&paths(&["a.txt"]), &wip_description("main")).unwrap();
        // Since then the artist kept working on the same file.
        write(&w.a, "a.txt", "newer edits that must not be lost\n");
        let (mut tab, ctx) = open(&w.a);
        tab.handle(Action::RestoreWip(shelf), &settings());
        settle(&mut tab, &ctx);
        assert!(errors(&tab).is_empty(), "{:?}", errors(&tab));
        assert_eq!(read(&w.a, "a.txt"), "work in progress\n");
        let backups: Vec<_> = w.a.list_shelves().unwrap().into_iter().filter(|s| s.is_backup()).collect();
        assert_eq!(backups.len(), 1, "the newer edits are saved as a backup shelf");
        let saved = String::from_utf8(w.a.blob_bytes(&backups[0].id, "a.txt").unwrap()).unwrap();
        assert_eq!(saved, "newer edits that must not be lost\n");
    }

    #[test]
    fn restoring_a_shelf_or_revision_preserves_ignored_local_artwork() {
        let w = world();
        write(&w.a, "render [final].txt", "shelved version\n");
        let shelf = w.a.create_shelf(&paths(&["render [final].txt"]), "render").unwrap();
        w.a.commit_paths(&paths(&["render [final].txt"]), "old render").unwrap();
        let revision = git(w.a.root(), &["rev-parse", "HEAD"]);
        git(w.a.root(), &["rm", "render [final].txt"]);
        git(w.a.root(), &["commit", "-m", "remove generated render"]);
        write(&w.a, ".gitignore", "render*\n");
        w.a.commit_paths(&paths(&[".gitignore"]), "ignore renders").unwrap();
        let (mut tab, ctx) = open(&w.a);
        for action in [
            Action::DoUnshelve { shelf, paths: paths(&["render [final].txt"]) },
            Action::DoGetRevision(revision, "render [final].txt".into()),
        ] {
            write(&w.a, "render [final].txt", "irreplaceable local render\n");
            tab.handle(action, &settings());
            settle(&mut tab, &ctx);
            assert!(errors(&tab).is_empty(), "{:?}", errors(&tab));
            assert_eq!(read(&w.a, "render [final].txt"), "shelved version\n");
            assert!(w
                .a
                .list_shelves()
                .unwrap()
                .iter()
                .filter(|s| s.is_backup())
                .any(|s| { w.a.blob_bytes(&s.id, "render [final].txt").is_ok_and(|b| b == b"irreplaceable local render\n") }));
        }
    }

    #[test]
    fn changelists_survive_a_conflicted_get_latest() {
        let w = world();
        conflict_setup(&w);
        // Bob also has uncommitted work in a named changelist; git sets it aside while the rebase is stopped.
        write(&w.b, "d.txt", "still editing\n");
        let (mut tab, ctx) = open(&w.b);
        let id = tab.changelists.create("Level tweaks");
        tab.handle(Action::MoveFiles(paths(&["d.txt"]), id), &settings());
        assert_eq!(tab.cl_files(id).to_vec(), paths(&["d.txt"]));

        tab.handle(Action::GetLatest, &settings());
        settle(&mut tab, &ctx);
        assert_eq!(tab.op(), RepoOp::Rebase);
        assert!(tab.has_conflicts());
        assert!(tab.file("d.txt").is_none(), "the edit is stashed by git during the rebase");

        tab.handle(Action::Resolve("a.txt".into(), Side::Mine), &settings());
        settle(&mut tab, &ctx);
        tab.handle(Action::ContinueOp, &settings());
        settle(&mut tab, &ctx);
        assert_eq!(tab.op(), RepoOp::None);
        assert_eq!(read(&w.b, "d.txt"), "still editing\n");
        assert_eq!(tab.cl_files(id).to_vec(), paths(&["d.txt"]), "d.txt is still in its changelist");
        // ...and it is still there after a restart.
        assert_eq!(ChangelistStore::load(&w.b.meta_dir()).list_of("d.txt"), id);
    }

    #[test]
    fn submit_description_is_kept_until_the_commit_exists() {
        let w = world();
        write(&w.a, "a.txt", "edited\n");
        // A hook that refuses the commit (studios use these for naming rules and the like).
        let hook = w.a.git_dir().join("hooks").join("pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\necho 'rejected by studio hook' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (mut tab, ctx) = open(&w.a);
        let typed = "Hero: new run cycle\n\nFixed the foot sliding on frames 12-18.";
        tab.submit_draft = Some((0, typed.to_string()));
        let submit = |tab: &mut RepoTab| {
            tab.handle(
                Action::DoSubmit { paths: paths(&["a.txt"]), message: typed.into(), cl_id: 0, push: false, unlock: vec![] },
                &settings(),
            )
        };
        submit(&mut tab);
        settle(&mut tab, &ctx);
        assert_eq!(errors(&tab).len(), 1, "{:?}", errors(&tab));
        tab.handle(Action::OpenSubmit(0), &settings());
        match &tab.dialog {
            Some(Dialog::Submit(d)) => assert_eq!(d.message, typed),
            _ => panic!("the submit dialog should be open"),
        }
        tab.dialog = None;
        std::fs::remove_file(&hook).unwrap();
        submit(&mut tab);
        settle(&mut tab, &ctx);
        assert!(tab.submit_draft.is_none());
        assert!(tab.files().is_empty());
    }

    #[test]
    fn submitting_a_move_takes_the_old_path_along() {
        let w = world();
        git(w.a.root(), &["mv", "b.txt", "b_moved.txt"]);
        let (mut tab, ctx) = open(&w.a);
        tab.handle(
            Action::DoSubmit { paths: paths(&["b_moved.txt"]), message: "Move b".into(), cl_id: 0, push: false, unlock: vec![] },
            &settings(),
        );
        settle(&mut tab, &ctx);
        assert!(errors(&tab).is_empty(), "{:?}", errors(&tab));
        assert!(tab.files().is_empty(), "left pending: {:?}", tab.files());
    }

    #[test]
    fn checked_out_files_keep_their_changelist_until_locks_are_known() {
        let w = world();
        std::fs::write(w.a.abs(".gitattributes"), "*.psd filter=lfs diff=lfs merge=lfs -text lockable\n").unwrap();
        // A plain file is enough here; what matters is that the path is marked lockable.
        git(
            w.a.root(),
            &["-c", "filter.lfs.clean=cat", "-c", "filter.lfs.smudge=cat", "-c", "filter.lfs.required=false", "add", ".gitattributes"],
        );
        write(&w.a, "hero.psd", "pixels\n");
        git(
            w.a.root(),
            &["-c", "filter.lfs.clean=cat", "-c", "filter.lfs.smudge=cat", "-c", "filter.lfs.required=false", "add", "hero.psd"],
        );
        git(
            w.a.root(),
            &[
                "-c",
                "filter.lfs.clean=cat",
                "-c",
                "filter.lfs.smudge=cat",
                "-c",
                "filter.lfs.required=false",
                "commit",
                "-m",
                "lockable art",
            ],
        );
        // "hero.psd" was checked out and put in a changelist in an earlier session, and is unchanged.
        let mut store = ChangelistStore::load(&w.a.meta_dir());
        let id = store.create("Hero rework");
        store.move_files(&["hero.psd", "a.txt"], id);
        store.save().unwrap();
        write(&w.a, "d.txt", "some edit so the status is not empty\n");

        // The lock server can't be reached (like working offline), so the lock list never arrives.
        git(w.a.root(), &["config", "lfs.url", "http://127.0.0.1:9/unreachable"]);
        let (tab, _ctx) = open(&w.a);
        assert!(tab.uses_lfs);
        assert!(tab.locks_err.is_some(), "this test needs the lock request to fail");
        let store = ChangelistStore::load(&w.a.meta_dir());
        assert_eq!(store.list_of("hero.psd"), id, "a lockable file is not dropped from its changelist on a guess");
        assert_eq!(store.list_of("a.txt"), 0, "an ordinary unchanged file is tidied away as before");
        drop(tab);
    }

    #[test]
    fn nested_lock_attributes_and_offline_submit_are_guarded() {
        let w = world();
        write(&w.a, "art/.gitattributes", "*.psd lockable\n");
        write(&w.a, "art/hero.psd", "original\n");
        w.a.commit_paths(&paths(&["art/.gitattributes", "art/hero.psd"]), "nested art").unwrap();
        let before = git(w.a.root(), &["rev-parse", "HEAD"]);
        let mut store = ChangelistStore::load(&w.a.meta_dir());
        let id = store.create("Art work");
        store.move_files(&["art/hero.psd"], id);
        store.save().unwrap();
        git(w.a.root(), &["config", "lfs.url", "http://127.0.0.1:9/unreachable"]);
        let (mut tab, ctx) = open(&w.a);
        assert!(tab.uses_lfs);
        assert!(tab.lockable.contains("art/hero.psd"));
        assert_eq!(tab.changelists.list_of("art/hero.psd"), id);
        write(&w.a, "art/hero.psd", "new artwork\n");
        tab.handle(
            Action::DoSubmit { paths: paths(&["art/hero.psd"]), message: "art".into(), cl_id: id, push: true, unlock: vec![] },
            &settings(),
        );
        settle(&mut tab, &ctx);
        assert_eq!(git(w.a.root(), &["rev-parse", "HEAD"]), before);
        assert!(errors(&tab).iter().any(|e| e.contains("Could not verify file locks")));
        tab.handle(
            Action::DoSubmit { paths: paths(&["art/hero.psd"]), message: "art".into(), cl_id: id, push: false, unlock: vec![] },
            &settings(),
        );
        settle(&mut tab, &ctx);
        assert_ne!(git(w.a.root(), &["rev-parse", "HEAD"]), before);
        assert_eq!(read(&w.a, "art/hero.psd"), "new artwork\n");
    }
}
