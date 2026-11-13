//! Batch-size profiling harness. Not part of the normal test run:
//!
//! ```text
//! cargo test --release --test perf -- --ignored --nocapture
//! ```
//!
//! `G4V_PERF_FILES` sets the number of changed files (default 5000). It prints how long the backend operations
//! and a UI frame take when an artist has that many files pending, so regressions in batch handling are easy to spot.

use g4v::git::{CmdLog, Repo};
use g4v::settings::Settings;
use g4v::ui::tab::{Action, CenterTab, RepoTab};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").current_dir(dir).args(args).output().expect("git runs");
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
}

fn n_files() -> usize {
    std::env::var("G4V_PERF_FILES").ok().and_then(|s| s.parse().ok()).unwrap_or(5000)
}

fn rel(i: usize) -> String {
    format!("art/set{:02}/group{:02}/asset_{i:06}.txt", i % 20, (i / 20) % 25)
}

/// A repository with `n` committed files, all of them modified, plus n/4 new files.
fn dirty_repo(n: usize) -> (tempfile::TempDir, Repo) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "Perf"]);
    git(&root, &["config", "user.email", "perf@example.com"]);
    git(&root, &["config", "core.autocrlf", "false"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    for i in 0..n {
        let p = root.join(rel(i));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("v1 {i}\n")).unwrap();
    }
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "initial"]);
    for i in 0..n {
        std::fs::write(root.join(rel(i)), format!("v2 {i}\nmore\n")).unwrap();
    }
    for i in 0..n / 4 {
        let p = root.join(format!("new/batch{:02}/render_{i:06}.txt", i % 10));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("new {i}\n")).unwrap();
    }
    let repo = Repo::open(&root, CmdLog::default()).unwrap();
    (tmp, repo)
}

fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    println!("  {label:<46} {:>9.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    r
}

#[test]
#[ignore]
fn backend_batch_operations() {
    let n = n_files();
    let (_tmp, repo) = dirty_repo(n);
    println!("\nbackend, {n} modified + {} new files", n / 4);
    let st = timed("status", || repo.status().unwrap());
    let all: Vec<String> = st.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(all.len(), n + n / 4);
    let shelf = timed("create_shelf (all files)", || repo.create_shelf(&all, "perf").unwrap());
    timed("create_shelf (objects already stored)", || repo.create_shelf(&all, "perf warm").unwrap());
    timed("revert_files (all files)", || repo.revert_files(&st.files).unwrap());
    assert!(repo.status().unwrap().files.is_empty());
    timed("unshelve (all files)", || repo.unshelve(&shelf, Some(&all)).unwrap());
    let st = repo.status().unwrap();
    assert_eq!(st.files.len(), n + n / 4);
    timed("sync overlap check (no upstream)", || repo.sync(Default::default(), &all).unwrap());
    timed("commit_paths (all files)", || repo.commit_paths(&all, "perf commit").unwrap());
    assert!(repo.status().unwrap().files.is_empty());
    if std::env::var_os("G4V_PERF_LOG").is_some() {
        println!("  -- git commands over 50 ms");
        for e in repo.cmd_log().snapshot().iter().filter(|e| e.millis >= 50) {
            println!("  {:>8} ms  {}", e.millis, e.command.chars().take(110).collect::<String>());
        }
    }
}

fn frame(ctx: &eframe::egui::Context, tab: &mut RepoTab, settings: &Settings, log: &CmdLog) {
    let input = eframe::egui::RawInput {
        screen_rect: Some(eframe::egui::Rect::from_min_size(eframe::egui::Pos2::ZERO, eframe::egui::vec2(1400.0, 880.0))),
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |ui| {
        tab.tick(ui.ctx(), settings);
        tab.show(ui, settings, log);
    });
}

fn avg_frame_ms(ctx: &eframe::egui::Context, tab: &mut RepoTab, settings: &Settings, log: &CmdLog, frames: u32) -> f64 {
    let t = Instant::now();
    for _ in 0..frames {
        frame(ctx, tab, settings, log);
    }
    t.elapsed().as_secs_f64() * 1000.0 / frames as f64
}

#[test]
#[ignore]
fn ui_frame_with_many_pending_files() {
    let n = n_files();
    let (_tmp, repo) = dirty_repo(n);
    let log = repo.cmd_log().clone();
    let settings = Settings { fetch_interval_secs: 0, ..Default::default() };
    let ctx = eframe::egui::Context::default();
    let mut tab = RepoTab::new(repo, &ctx, None);
    // Let the background jobs deliver the first status and file tree.
    let start = Instant::now();
    while (tab.status.is_none() || tab.tree.nodes.is_empty() || tab.is_busy()) && start.elapsed() < Duration::from_secs(120) {
        frame(&ctx, &mut tab, &settings, &log);
        std::thread::sleep(Duration::from_millis(20));
    }
    let files: Vec<String> = tab.files().iter().map(|f| f.path.clone()).collect();
    assert_eq!(files.len(), n + n / 4);
    // Three numbered changelists holding half of the files.
    for (k, chunk) in files[..files.len() / 2].chunks(files.len() / 6 + 1).enumerate() {
        let id = tab.changelists.create(&format!("Changelist {k}"));
        tab.handle(Action::MoveFiles(chunk.to_vec(), id), &settings);
    }
    println!("\nui, {} pending files in 4 changelists", files.len());
    tab.center = CenterTab::Pending;
    println!("  {:<46} {:>9.2} ms", "frame: Pending tab", avg_frame_ms(&ctx, &mut tab, &settings, &log, 30));
    tab.selection = files.iter().cloned().collect();
    println!("  {:<46} {:>9.2} ms", "frame: Pending tab, everything selected", avg_frame_ms(&ctx, &mut tab, &settings, &log, 30));
    tab.selection.clear();
    tab.handle(Action::OpenSubmit(0), &settings);
    println!("  {:<46} {:>9.2} ms", "frame: Submit dialog open", avg_frame_ms(&ctx, &mut tab, &settings, &log, 30));
    tab.dialog = None;
    tab.handle(Action::OpenRevert(files.clone()), &settings);
    println!("  {:<46} {:>9.2} ms", "frame: Revert dialog open", avg_frame_ms(&ctx, &mut tab, &settings, &log, 30));
    tab.dialog = None;
    tab.center = CenterTab::History;
    println!("  {:<46} {:>9.2} ms", "frame: History tab (pending not shown)", avg_frame_ms(&ctx, &mut tab, &settings, &log, 30));
    let head = tab.repo.run(&["rev-parse", "HEAD"]).unwrap();
    tab.handle(Action::SelectCommit(head), &settings);
    let start = Instant::now();
    while tab.commit_detail.is_none() && start.elapsed() < Duration::from_secs(120) {
        frame(&ctx, &mut tab, &settings, &log);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(tab.commit_detail.as_ref().unwrap().files.len(), n);
    println!("  {:<46} {:>9.2} ms", "frame: History, large commit selected", avg_frame_ms(&ctx, &mut tab, &settings, &log, 30));
    let t = Instant::now();
    tab.handle(Action::DoRevert(files.clone()), &settings);
    println!("  {:<46} {:>9.2} ms", "UI-thread stall when starting Revert", t.elapsed().as_secs_f64() * 1000.0);
    // Keep the temporary repository alive until its worker finishes; dropping it mid-revert invalidates timings
    // and can leave an orphaned process racing TempDir's cleanup.
    let start = Instant::now();
    while tab.has_pending_work() && start.elapsed() < Duration::from_secs(180) {
        tab.tick(&ctx, &settings);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!tab.has_pending_work());
    tab.tick(&ctx, &settings);
    assert!(tab.repo.status().unwrap().files.is_empty());
}
