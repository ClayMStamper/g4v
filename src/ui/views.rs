//! Main panels of a repository tab.

use super::tab::{Action, CenterTab, DiffTarget, LeftTab, RepoTab, SwitchTarget};
use super::theme::{self, icon, ACCENT, GREEN, MUTED, ORANGE, PURPLE, RED, YELLOW};
use super::tree::TreeFilter;
use super::widgets::{self, row, DraggedFiles, ROW_H};
use crate::changelists::DEFAULT_ID;
use crate::git::branches::RefKind;
use crate::git::history::HistoryScope;
use crate::git::ops::Side;
use crate::git::status::{ChangeKind, FileStatus, RepoOp};
use crate::git::CmdLog;
use crate::settings::Settings;
use eframe::egui::{self, Color32, RichText, Ui};

pub fn kind_icon(k: ChangeKind) -> (&'static str, Color32, &'static str) {
    match k {
        ChangeKind::Add => (icon::ADD, GREEN, "New file (will be added)"),
        ChangeKind::Edit => (icon::EDIT, ACCENT, "Edited"),
        ChangeKind::Delete => (icon::DELETE, RED, "Deleted (will be removed)"),
        ChangeKind::Move => (icon::MOVE, PURPLE, "Moved / renamed"),
        ChangeKind::TypeChange => (icon::EDIT, ACCENT, "File type changed"),
        ChangeKind::Conflict => (icon::CONFLICT, RED, "Conflict — needs resolving"),
    }
}

impl RepoTab {
    pub fn show(&mut self, ui: &mut Ui, settings: &Settings, log: &CmdLog) {
        let mut actions: Vec<Action> = Vec::new();
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui, &mut actions);
            ui.add_space(2.0);
            self.banners(ui, &mut actions, settings);
            ui.add_space(4.0);
        });
        egui::Panel::left("workspace_panel").resizable(true).default_size(330.0).min_size(220.0).max_size(700.0).show(ui, |ui| {
            self.left_panel(ui, &mut actions);
        });
        egui::CentralPanel::default_margins().show(ui, |ui| {
            self.center_tabs(ui);
            ui.separator();
            egui::Panel::bottom("detail_panel").resizable(true).default_size(400.0).min_size(120.0).show(ui, |ui| {
                self.detail_panel(ui, &mut actions);
            });
            egui::CentralPanel::no_frame().show(ui, |ui| match self.center {
                CenterTab::Pending => self.pending_view(ui, &mut actions),
                CenterTab::History => self.history_view(ui, &mut actions),
                CenterTab::Shelves => self.shelves_view(ui, &mut actions),
                CenterTab::Locks => self.locks_view(ui, &mut actions),
                CenterTab::Log => log_view(ui, log),
            });
        });
        self.dialogs(ui.ctx(), &mut actions, settings);
        if ui.ctx().input(|i| i.key_pressed(egui::Key::F5)) {
            actions.push(Action::Refresh);
        }
        for a in actions {
            self.handle(a, settings);
        }
    }

    /// The changelist the toolbar acts on: the one holding the selection, else Default.
    fn active_changelist(&self) -> u32 {
        self.selection.iter().find(|p| self.file(p).is_some()).map(|p| self.changelists.list_of(p)).unwrap_or(DEFAULT_ID)
    }

    fn toolbar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let st = self.status.clone();
        let behind = st.as_ref().map(|s| s.branch.behind).unwrap_or(0);
        let ahead = st.as_ref().map(|s| s.branch.ahead).unwrap_or(0);
        let busy = self.is_busy();
        let sel: Vec<String> = self.selection.iter().cloned().collect();
        let sel_changed: Vec<String> = sel.iter().filter(|p| self.file(p).is_some()).cloned().collect();
        let sel_lockable: Vec<String> =
            sel.iter().filter(|p| self.lock_of(p).is_none() && self.file(p).is_none_or(|f| !f.untracked)).cloned().collect();
        let sel_mine: Vec<String> = sel.iter().filter(|p| self.lock_of(p).is_some_and(|l| l.ours)).cloned().collect();
        let cl = self.active_changelist();
        let pending_in_cl = self.cl_files(cl).iter().filter(|p| self.file(p).is_some()).count();

        ui.horizontal(|ui| {
            if theme::tool_button(ui, icon::REFRESH, "Refresh", true, None, "Re-read the workspace and check the server (F5)").clicked() {
                actions.push(Action::Refresh);
            }
            let tip = if behind > 0 {
                format!("Download {behind} newer change(s) from the server")
            } else {
                "Download the newest version of everything from the server".to_string()
            };
            let badge = (behind > 0).then(|| behind.to_string());
            if theme::tool_button(ui, icon::GET_LATEST, "Get Latest", !busy, badge, &tip).clicked() {
                actions.push(Action::GetLatest);
            }
            ui.separator();
            let label = if cl == DEFAULT_ID { "Submit".to_string() } else { format!("Submit #{cl}") };
            if theme::tool_button(
                ui,
                icon::SUBMIT,
                &label,
                !busy && pending_in_cl > 0,
                None,
                "Commit the changelist and upload it to the server",
            )
            .clicked()
            {
                actions.push(Action::OpenSubmit(cl));
            }
            let shelve_paths = if sel_changed.is_empty() { self.cl_files(cl) } else { sel_changed.clone() };
            if theme::tool_button(
                ui,
                icon::SHELVE,
                "Shelve",
                !busy && shelve_paths.iter().any(|p| self.file(p).is_some()),
                None,
                "Save the selected changes on the side without submitting them",
            )
            .clicked()
            {
                actions.push(Action::OpenShelve(shelve_paths, Some(cl)));
            }
            let lock_tip = if self.uses_lfs {
                "Check out: lock the selected files so nobody else can submit changes to them"
            } else {
                "Locking needs Git LFS (see the Locks tab)"
            };
            if theme::tool_button(ui, icon::LOCK, "Check Out", self.uses_lfs && !sel_lockable.is_empty(), None, lock_tip).clicked() {
                actions.push(Action::Lock(sel_lockable));
            }
            if theme::tool_button(ui, icon::UNLOCK, "Release", !sel_mine.is_empty(), None, "Release your lock on the selected files")
                .clicked()
            {
                actions.push(Action::Unlock(sel_mine, false));
            }
            if theme::tool_button(
                ui,
                icon::REVERT,
                "Revert",
                !busy && !sel_changed.is_empty(),
                None,
                "Throw away local changes to the selected files (a backup is kept)",
            )
            .clicked()
            {
                actions.push(Action::OpenRevert(sel_changed));
            }
            if ahead > 0 {
                ui.separator();
                if theme::tool_button(ui, "⤴", "Push", !busy, Some(ahead.to_string()), "Upload your committed changes to the server")
                    .clicked()
                {
                    actions.push(Action::Push);
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Server status
                if self.net_busy() {
                    ui.add(egui::Spinner::new());
                }
                let server = match (&self.fetch_err, self.last_fetch) {
                    (Some(_), _) => RichText::new(format!("{} server unreachable", icon::WARN)).color(ORANGE).small(),
                    (None, Some(t)) => {
                        let secs = t.elapsed().as_secs();
                        let when = if secs < 60 { "just now".to_string() } else { format!("{} min ago", secs / 60) };
                        theme::small(format!("{} checked {when}", icon::CLOUD))
                    }
                    (None, None) => theme::small(format!("{} checking…", icon::CLOUD)),
                };
                let r = ui.label(server);
                if let Some(e) = &self.fetch_err {
                    r.on_hover_text(e);
                }
                ui.separator();
                self.branch_picker(ui, actions);
                if let Some(l) = self.busy_label() {
                    ui.add(egui::Spinner::new());
                    ui.label(theme::small(format!("{l}…")));
                }
            });
        });
    }

    fn branch_picker(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let current = self.status.as_ref().and_then(|s| s.branch.head.clone());
        let rebasing = self.status.as_ref().and_then(|s| s.rebase_branch.clone());
        let text = match (&current, &rebasing) {
            (Some(b), _) => format!("{} {b}", icon::BRANCH),
            (None, Some(b)) => format!("{} {b} (getting latest…)", icon::BRANCH),
            (None, None) => format!("{} (no branch)", icon::BRANCH),
        };
        let locals: Vec<String> = self
            .refs
            .as_ref()
            .map(|r| r.branches.iter().filter(|b| b.kind == RefKind::Local).map(|b| b.name.clone()).collect())
            .unwrap_or_default();
        egui::ComboBox::from_id_salt("branch_picker")
            .selected_text(RichText::new(text).strong())
            .width(180.0)
            .show_ui(ui, |ui| {
                for b in &locals {
                    let is_cur = current.as_deref() == Some(b.as_str());
                    if ui.selectable_label(is_cur, b).clicked() && !is_cur {
                        actions.push(Action::Switch(SwitchTarget::Local(b.clone())));
                    }
                }
                ui.separator();
                if ui.button("✚ New branch…").clicked() {
                    actions.push(Action::NewBranch(None));
                }
                if ui.button("All branches…").clicked() {
                    self.left = LeftTab::Branches;
                }
            })
            .response
            .on_hover_text("Current branch (like a Perforce stream). Click to switch.");
    }

    fn banners(&mut self, ui: &mut Ui, actions: &mut Vec<Action>, settings: &Settings) {
        let Some(st) = self.status.clone() else {
            if let Some(e) = &self.status_err {
                theme::banner(ui, RED, |ui| {
                    ui.label(format!("{} Could not read the workspace: {e}", icon::WARN));
                });
            } else {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label("Reading workspace…");
                });
            }
            return;
        };
        if !self.lfs_installed {
            theme::banner(ui, RED, |ui| {
                ui.label(format!(
                    "{} This project stores large files with Git LFS, but Git LFS is not installed. Install it from https://git-lfs.com and restart g4v, otherwise art files will show up as small text 'pointer' files.",
                    icon::WARN
                ));
            });
        }
        if st.index_lock {
            let age = self.repo.index_lock_age_secs().unwrap_or(0);
            if age > 20 && !self.is_busy() {
                theme::banner(ui, ORANGE, |ui| {
                    ui.label(format!(
                        "{} Git left a lock file behind ({}s old), probably from a program that crashed. Operations will fail until it is removed.",
                        icon::WARN, age
                    ));
                    if ui.button("Remove lock file").clicked() {
                        actions.push(Action::RemoveIndexLock);
                    }
                });
            }
        }
        if st.op != RepoOp::None {
            let n = st.conflicts().count();
            theme::banner(ui, RED, |ui| {
                if n > 0 {
                    ui.label(
                        RichText::new(format!(
                            "{} A {} stopped because {n} file(s) were changed both by you and by someone else.",
                            icon::CONFLICT,
                            st.op.label()
                        ))
                        .strong(),
                    );
                    if ui.button("Show conflicts").clicked() {
                        self.center = CenterTab::Pending;
                    }
                } else {
                    ui.label(
                        RichText::new(format!("{} All conflicts resolved — press Continue to finish the {}.", icon::OK, st.op.label()))
                            .strong(),
                    );
                    if ui.button(RichText::new("Continue").strong()).clicked() {
                        actions.push(Action::ContinueOp);
                    }
                }
                if ui.button("Abort").on_hover_text("Undo the whole operation and go back to how things were").clicked() {
                    actions.push(Action::AbortOp);
                }
            });
        }
        if st.branch.head.is_none() && st.branch.oid.is_some() && st.op == RepoOp::None {
            theme::banner(ui, ORANGE, |ui| {
                ui.label(format!(
                    "{} You are looking at an old version, not a branch (\"detached HEAD\"). Anything you submit here could get lost.",
                    icon::WARN
                ));
                if ui.button("Create a branch here").clicked() {
                    actions.push(Action::NewBranch(None));
                }
                if let Some(main) = self.default_branch_guess() {
                    if ui.button(format!("Go back to {main}")).clicked() {
                        actions.push(Action::Switch(SwitchTarget::Local(main)));
                    }
                }
            });
        }
        if !self.identity.complete() {
            theme::banner(ui, ACCENT, |ui| {
                ui.label(format!("{} Set your name and email so your teammates know who made each change.", icon::INFO));
                if ui.button("Set up…").clicked() {
                    actions.push(Action::SetupIdentity);
                }
            });
        }
        // Conflict risk: files I'm editing that changed on the server.
        let risky: Vec<String> = st.files.iter().filter(|f| self.incoming.contains_key(&f.path)).map(|f| f.path.clone()).collect();
        if !risky.is_empty() {
            theme::banner(ui, ORANGE, |ui| {
                ui.label(format!(
                    "{} {} file(s) you are editing were also changed on the server: {}. Get latest before continuing to avoid conflicts.",
                    icon::WARN,
                    risky.len(),
                    short_list(&risky, 3)
                ));
            });
        }
        // Edits to files someone else has locked.
        let blocked = self.locked_by_others(st.files.iter().map(|f| &f.path));
        if !blocked.is_empty() {
            theme::banner(ui, RED, |ui| {
                let list: Vec<String> = blocked.iter().map(|(p, o)| format!("{p} ({o})")).collect();
                ui.label(format!(
                    "{} You changed file(s) that someone else has checked out: {}. You won't be able to submit them until the lock is released.",
                    icon::LOCK,
                    short_list(&list, 3)
                ));
            });
        }
        // Edits to lockable files without a lock.
        let unlocked = self.lockable_not_locked(st.files.iter().map(|f| &f.path));
        if !unlocked.is_empty() && self.locks_err.is_none() {
            theme::banner(ui, YELLOW, |ui| {
                ui.label(format!(
                    "{} You edited {} file(s) that should be checked out first (binary files can't be merged): {}",
                    icon::LOCK,
                    unlocked.len(),
                    short_list(&unlocked, 3)
                ));
                if ui.button("Check out now").clicked() {
                    actions.push(Action::Lock(unlocked.clone()));
                }
            });
        }
        if st.branch.behind > 0 && risky.is_empty() && st.op == RepoOp::None {
            theme::banner(ui, ACCENT, |ui| {
                ui.label(format!(
                    "{} {} newer change(s) are on the server ({} file(s)).",
                    icon::INCOMING,
                    st.branch.behind,
                    self.incoming.len()
                ));
                if ui.button("Get Latest").clicked() {
                    actions.push(Action::GetLatest);
                }
            });
        }
        if st.branch.ahead > 0 && st.op == RepoOp::None {
            theme::banner(ui, PURPLE, |ui| {
                ui.label(format!("{} {} committed change(s) haven't been uploaded to the server yet.", icon::SUBMIT, st.branch.ahead));
                if ui.button("Push now").clicked() {
                    actions.push(Action::Push);
                }
            });
        }
        if st.branch.head.is_some() && st.branch.upstream.is_none() && st.branch.oid.is_some() && settings.push_on_submit {
            let has_remote = self.refs.as_ref().is_some_and(|r| !r.remotes.is_empty());
            theme::banner(ui, MUTED, |ui| {
                if has_remote {
                    ui.label(format!("{} This branch only exists on your computer.", icon::INFO));
                    if ui.button("Publish to server").clicked() {
                        actions.push(Action::Push);
                    }
                } else {
                    ui.label(format!("{} This repository isn't connected to a server yet.", icon::INFO));
                    if ui.button("Connect to server…").clicked() {
                        actions.push(Action::AddRemote);
                    }
                }
            });
        }
        if let Some(shelf) = self.wip_shelf_for_current().cloned() {
            if st.files.is_empty() {
                theme::banner(ui, GREEN, |ui| {
                    ui.label(format!(
                        "{} You shelved work when you last left this branch ({}).",
                        icon::SHELVE,
                        theme::format_time(shelf.time)
                    ));
                    if ui.button(RichText::new("Restore it").strong()).clicked() {
                        actions.push(Action::RestoreWip(shelf.clone()));
                    }
                    if ui.button("Show shelf").clicked() {
                        actions.push(Action::SelectShelf(shelf.refname.clone()));
                        self.center = CenterTab::Shelves;
                    }
                });
            }
        }
    }

    fn default_branch_guess(&self) -> Option<String> {
        let refs = self.refs.as_ref()?;
        ["main", "master", "develop", "trunk"]
            .iter()
            .find(|n| refs.branches.iter().any(|b| b.kind == RefKind::Local && b.name == **n))
            .map(|s| s.to_string())
    }

    // ------------------------------------------------------------ left panel

    fn left_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.left, LeftTab::Workspace, RichText::new(format!("{} Workspace", icon::FOLDER)).strong());
            ui.selectable_value(&mut self.left, LeftTab::Branches, RichText::new(format!("{} Branches", icon::BRANCH)).strong());
        });
        ui.separator();
        match self.left {
            LeftTab::Workspace => self.workspace_tree(ui, actions),
            LeftTab::Branches => self.branches_view(ui, actions),
        }
    }

    fn file_badges(&self, path: &str, p: &mut widgets::RowPainter) -> Vec<String> {
        let mut tips = Vec::new();
        let changed = self.file(path);
        if let Some(f) = changed {
            let (i, c, t) = kind_icon(f.kind);
            p.small(i, c);
            tips.push(t.to_string());
        }
        if let Some(c) = self.incoming.get(path) {
            if changed.is_some() {
                p.small(icon::WARN, RED);
                tips.push("Also changed on the server — get latest to avoid a conflict".into());
            } else {
                p.small(icon::INCOMING, YELLOW);
                tips.push(match c {
                    'A' => "New on the server — Get Latest to download".into(),
                    'D' => "Deleted on the server — Get Latest to update".into(),
                    _ => "A newer version is on the server — Get Latest to update".into(),
                });
            }
        }
        if let Some(l) = self.lock_of(path) {
            if l.ours {
                p.small(icon::LOCK, ACCENT);
                tips.push("Checked out (locked) by you".into());
            } else {
                p.small(&format!("{} {}", icon::LOCK, l.owner), RED);
                tips.push(format!("Checked out by {} — don't edit; you won't be able to submit it", l.owner));
            }
        } else if self.lockable.contains(path) {
            p.small(icon::UNLOCK, MUTED);
            tips.push("Lockable: check it out before editing".into());
        }
        if self.lfs_paths.contains(path) {
            tips.push("Stored with Git LFS".into());
        }
        tips
    }

    fn workspace_tree(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("tree_filter")
                    .width(100.0)
                    .selected_text(match self.tree.filter {
                        TreeFilter::All => "All files",
                        TreeFilter::Changed => "Changed",
                        TreeFilter::OutOfDate => "Out of date",
                        TreeFilter::Locked => "Locked",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.tree.filter, TreeFilter::All, "All files");
                        ui.selectable_value(&mut self.tree.filter, TreeFilter::Changed, "Changed by me");
                        ui.selectable_value(&mut self.tree.filter, TreeFilter::OutOfDate, "Out of date");
                        ui.selectable_value(&mut self.tree.filter, TreeFilter::Locked, "Checked out");
                    });
                ui.add(egui::TextEdit::singleline(&mut self.tree.search).hint_text("🔍 Find file…").desired_width(ui.available_width()));
            })
        });
        {
            let status_paths = &self.status_by_path;
            let incoming = &self.incoming;
            let locks = &self.locks_by_path;
            self.tree.refresh_rows(|p, f| match f {
                TreeFilter::All => true,
                TreeFilter::Changed => status_paths.contains_key(p),
                TreeFilter::OutOfDate => incoming.contains_key(p),
                TreeFilter::Locked => locks.contains_key(p),
            });
        }
        if self.tree.nodes.is_empty() {
            ui.label(theme::small("Loading files…"));
            return;
        }
        if self.tree.rows.is_empty() {
            ui.add_space(10.0);
            ui.label(theme::small("Nothing matches."));
            return;
        }
        let rows = std::mem::take(&mut self.tree.rows);
        let modifiers = ui.input(|i| i.modifiers);
        let mut clicked_file: Option<String> = None;
        egui::ScrollArea::vertical().id_salt("tree_scroll").auto_shrink([false, false]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for &idx in &rows[range] {
                let node = self.tree.nodes[idx].clone();
                let selected = !node.is_dir && self.selection.contains(&node.path);
                let (resp, mut p) = row(ui, selected, false);
                p.indent(node.depth as f32 * 14.0);
                if node.is_dir {
                    let open = self.tree.is_filtering() || self.tree.expanded.contains(&node.path);
                    p.small(if open { "⏷" } else { "⏵" }, MUTED);
                    p.text(icon::FOLDER, Some(YELLOW.gamma_multiply(0.8)));
                    p.text(&node.name, None);
                    if self.changed_dirs.contains(&node.path) {
                        p.small("•", ACCENT);
                    }
                    if self.incoming_dirs.contains(&node.path) {
                        p.small(icon::INCOMING, YELLOW);
                    }
                    if self.locked_dirs.contains(&node.path) {
                        p.small(icon::LOCK, MUTED);
                    }
                    if resp.clicked() && !self.tree.is_filtering() {
                        self.tree.toggle(&node.path);
                    }
                    let path = node.path.clone();
                    resp.context_menu(|ui| self.folder_menu(ui, &path, actions));
                } else {
                    let color = if node.ghost { Some(MUTED) } else { None };
                    p.text(icon::FILE, Some(MUTED));
                    if node.ghost {
                        p.italic(&node.name, MUTED);
                    } else {
                        p.text(&node.name, color);
                    }
                    let tips = self.file_badges(&node.path, &mut p);
                    let resp = if tips.is_empty() { resp } else { resp.on_hover_text(tips.join("\n")) };
                    if resp.clicked() {
                        clicked_file = Some(node.path.clone());
                        if self.file(&node.path).is_some() {
                            actions.push(Action::ShowDiff(DiffTarget::Working { path: node.path.clone() }));
                        } else if self.incoming.contains_key(&node.path) {
                            actions.push(Action::ShowDiff(DiffTarget::Incoming { path: node.path.clone() }));
                        }
                    }
                    if resp.double_clicked() && !node.ghost {
                        actions.push(Action::OpenFile(node.path.clone()));
                    }
                    if resp.secondary_clicked() && !self.selection.contains(&node.path) {
                        self.selection = [node.path.clone()].into_iter().collect();
                    }
                    let path = node.path.clone();
                    resp.context_menu(|ui| self.file_menu(ui, &path, actions));
                }
            }
        });
        if let Some(path) = clicked_file {
            // Build the visible file order only when needed (shift-click ranges).
            let order: Vec<String> =
                rows.iter().filter(|&&i| !self.tree.nodes[i].is_dir).map(|&i| self.tree.nodes[i].path.clone()).collect();
            widgets::update_selection(&mut self.selection, &mut self.sel_anchor, &order, &path, modifiers);
        }
        if self.tree.rows.is_empty() {
            self.tree.rows = rows;
        }
    }

    /// Paths the context menu applies to: the selection if it includes `path`, else just `path`.
    fn menu_targets(&self, path: &str) -> Vec<String> {
        if self.selection.contains(path) {
            self.selection.iter().cloned().collect()
        } else {
            vec![path.to_string()]
        }
    }

    pub fn file_menu(&self, ui: &mut Ui, path: &str, actions: &mut Vec<Action>) {
        let targets = self.menu_targets(path);
        let changed: Vec<String> = targets.iter().filter(|p| self.file(p).is_some()).cloned().collect();
        let n = targets.len();
        let plural = if n > 1 { format!(" ({n} files)") } else { String::new() };
        if !changed.is_empty() && ui.button("Diff against latest submitted".to_string()).clicked() {
            actions.push(Action::ShowDiff(DiffTarget::Working { path: path.to_string() }));
            ui.close();
        }
        if self.incoming.contains_key(path) && ui.button("Diff against newer server version").clicked() {
            actions.push(Action::ShowDiff(DiffTarget::Incoming { path: path.to_string() }));
            ui.close();
        }
        ui.separator();
        if self.uses_lfs {
            let lockable: Vec<String> = targets.iter().filter(|p| self.lock_of(p).is_none()).cloned().collect();
            if !lockable.is_empty() && ui.button(format!("{} Check out (lock){plural}", icon::LOCK)).clicked() {
                actions.push(Action::Lock(lockable));
                ui.close();
            }
            let mine: Vec<String> = targets.iter().filter(|p| self.lock_of(p).is_some_and(|l| l.ours)).cloned().collect();
            if !mine.is_empty() && ui.button(format!("{} Release lock", icon::UNLOCK)).clicked() {
                actions.push(Action::Unlock(mine, false));
                ui.close();
            }
            let theirs: Vec<String> = targets.iter().filter(|p| self.lock_of(p).is_some_and(|l| !l.ours)).cloned().collect();
            if !theirs.is_empty() && ui.button("Break other user's lock…").clicked() {
                actions.push(Action::Unlock(theirs, true));
                ui.close();
            }
        }
        if !changed.is_empty() {
            ui.menu_button("Move to changelist", |ui| {
                if ui.button("Default").clicked() {
                    actions.push(Action::MoveFiles(changed.clone(), DEFAULT_ID));
                    ui.close();
                }
                for cl in &self.changelists.lists {
                    if ui.button(format!("#{} {}", cl.id, super::tab::first_line(&cl.description))).clicked() {
                        actions.push(Action::MoveFiles(changed.clone(), cl.id));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("New changelist…").clicked() {
                    actions.push(Action::NewChangelist(changed.clone()));
                    ui.close();
                }
            });
            if ui.button(format!("{} Shelve{plural}…", icon::SHELVE)).clicked() {
                actions.push(Action::OpenShelve(changed.clone(), None));
                ui.close();
            }
            if ui.button(format!("{} Revert{plural}…", icon::REVERT)).clicked() {
                actions.push(Action::OpenRevert(changed.clone()));
                ui.close();
            }
        }
        ui.separator();
        if ui.button(format!("{} File history", icon::HISTORY)).clicked() {
            actions.push(Action::FileHistory(path.to_string()));
            ui.close();
        }
        if ui.button("Open").clicked() {
            actions.push(Action::OpenFile(path.to_string()));
            ui.close();
        }
        if ui.button("Show in folder").clicked() {
            actions.push(Action::RevealFile(path.to_string()));
            ui.close();
        }
        if ui.button("Copy path").clicked() {
            ui.ctx().copy_text(path.to_string());
            ui.close();
        }
        if let Some(ext) = path.rsplit_once('.').map(|(_, e)| e.to_lowercase()) {
            if !self.lfs_paths.contains(path) && self.file(path).is_some_and(|f| f.untracked) {
                ui.separator();
                if ui.button(format!("Store all *.{ext} files with Git LFS")).clicked() {
                    actions.push(Action::TrackLfs(format!("*.{ext}")));
                    ui.close();
                }
            }
        }
    }

    fn folder_menu(&self, ui: &mut Ui, path: &str, actions: &mut Vec<Action>) {
        let files = self.tree.files_under(path);
        let changed: Vec<String> = files.iter().filter(|p| self.file(p).is_some()).cloned().collect();
        if self.uses_lfs {
            let lockable: Vec<String> = files.iter().filter(|p| self.lockable.contains(*p) && self.lock_of(p).is_none()).cloned().collect();
            if !lockable.is_empty() && ui.button(format!("{} Check out lockable files ({})", icon::LOCK, lockable.len())).clicked() {
                actions.push(Action::Lock(lockable));
                ui.close();
            }
        }
        if !changed.is_empty() {
            if ui.button(format!("{} Shelve changes in folder ({})…", icon::SHELVE, changed.len())).clicked() {
                actions.push(Action::OpenShelve(changed.clone(), None));
                ui.close();
            }
            if ui.button(format!("{} Revert changes in folder ({})…", icon::REVERT, changed.len())).clicked() {
                actions.push(Action::OpenRevert(changed));
                ui.close();
            }
        }
        if ui.button(format!("{} Folder history", icon::HISTORY)).clicked() {
            actions.push(Action::FileHistory(path.to_string()));
            ui.close();
        }
        if ui.button("Open folder").clicked() {
            actions.push(Action::OpenFile(path.to_string()));
            ui.close();
        }
    }

    fn branches_view(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let Some(refs) = &self.refs else {
            ui.label(theme::small("Loading…"));
            return;
        };
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("✚ New branch").clicked() {
                    actions.push(Action::NewBranch(None));
                }
                ui.add(egui::TextEdit::singleline(&mut self.branch_filter).hint_text("🔍 Filter…").desired_width(ui.available_width()));
            });
        });
        let filter = self.branch_filter.to_lowercase();
        let matches = |n: &str| filter.is_empty() || n.to_lowercase().contains(&filter);
        egui::ScrollArea::vertical().id_salt("branches_scroll").auto_shrink([false, false]).show(ui, |ui| {
            egui::CollapsingHeader::new(RichText::new("Local branches").strong()).default_open(true).show(ui, |ui| {
                for b in refs.branches.iter().filter(|b| b.kind == RefKind::Local && matches(&b.name)) {
                    let (resp, mut p) = row(ui, false, false);
                    if b.is_head {
                        p.text(icon::OK, Some(GREEN));
                        p.text(&b.name, Some(ui.visuals().strong_text_color()));
                    } else {
                        p.text(icon::BRANCH, Some(MUTED));
                        p.text(&b.name, None);
                    }
                    if b.ahead > 0 {
                        p.small(&format!("⬆{}", b.ahead), PURPLE);
                    }
                    if b.behind > 0 {
                        p.small(&format!("⬇{}", b.behind), YELLOW);
                    }
                    if b.gone {
                        p.small("deleted on server", ORANGE);
                    } else if b.upstream.is_none() {
                        p.small("local only", MUTED);
                    }
                    let resp = resp.on_hover_text(format!("{}\nLast change: {} ({})", b.name, b.subject, theme::format_time(b.time)));
                    if resp.double_clicked() && !b.is_head {
                        actions.push(Action::Switch(SwitchTarget::Local(b.name.clone())));
                    }
                    let name = b.name.clone();
                    let is_head = b.is_head;
                    resp.context_menu(|ui| {
                        if !is_head && ui.button("Switch to this branch").clicked() {
                            actions.push(Action::Switch(SwitchTarget::Local(name.clone())));
                            ui.close();
                        }
                        if !is_head && ui.button("Merge into current branch…").clicked() {
                            actions.push(Action::MergeBranch(name.clone()));
                            ui.close();
                        }
                        if ui.button("New branch from here…").clicked() {
                            actions.push(Action::NewBranch(Some(name.clone())));
                            ui.close();
                        }
                        if !is_head {
                            ui.separator();
                            if ui.button("Delete…").clicked() {
                                actions.push(Action::DeleteBranch(name.clone(), false));
                                ui.close();
                            }
                            if ui.button("Delete (discard unmerged work)…").clicked() {
                                actions.push(Action::Confirm {
                                    title: format!("Permanently delete {name}?"),
                                    body: "Any work on this branch that isn't merged anywhere else will be lost.".into(),
                                    action: Box::new(Action::DoDeleteBranch(name.clone(), true)),
                                });
                                ui.close();
                            }
                        }
                    });
                }
            });
            let remotes: Vec<String> = refs.remotes.iter().map(|(n, _)| n.clone()).collect();
            for remote in &remotes {
                egui::CollapsingHeader::new(RichText::new(format!("{} {remote}", icon::CLOUD)).strong())
                    .id_salt(("remote", remote))
                    .default_open(false)
                    .show(ui, |ui| {
                        for b in refs
                            .branches
                            .iter()
                            .filter(|b| b.kind == RefKind::Remote && b.name.starts_with(&format!("{remote}/")) && matches(&b.name))
                        {
                            let (resp, mut p) = row(ui, false, false);
                            p.text(icon::BRANCH, Some(MUTED));
                            p.text(b.name.trim_start_matches(&format!("{remote}/")), None);
                            p.small(&theme::format_time(b.time), MUTED);
                            let resp = resp.on_hover_text(format!("{}\n{}", b.name, b.subject));
                            if resp.double_clicked() {
                                actions.push(Action::Switch(SwitchTarget::Remote(b.name.clone())));
                            }
                            let name = b.name.clone();
                            resp.context_menu(|ui| {
                                if ui.button("Check out as local branch").clicked() {
                                    actions.push(Action::Switch(SwitchTarget::Remote(name.clone())));
                                    ui.close();
                                }
                                if ui.button("Merge into current branch…").clicked() {
                                    actions.push(Action::MergeBranch(name.clone()));
                                    ui.close();
                                }
                                if ui.button("New branch from here…").clicked() {
                                    actions.push(Action::NewBranch(Some(name.clone())));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Delete from server…").clicked() {
                                    actions.push(Action::DeleteRemoteBranch(name.clone()));
                                    ui.close();
                                }
                            });
                        }
                    });
            }
            egui::CollapsingHeader::new(RichText::new(format!("{} Tags", icon::TAG)).strong()).default_open(false).show(ui, |ui| {
                let mut any = false;
                for b in refs.branches.iter().filter(|b| b.kind == RefKind::Tag && matches(&b.name)) {
                    any = true;
                    let (resp, mut p) = row(ui, false, false);
                    p.text(icon::TAG, Some(ORANGE));
                    p.text(&b.name, None);
                    let name = b.name.clone();
                    resp.context_menu(|ui| {
                        if ui.button("Upload tag to server").clicked() {
                            actions.push(Action::PushTag(name.clone()));
                            ui.close();
                        }
                        if ui.button("New branch from tag…").clicked() {
                            actions.push(Action::NewBranch(Some(name.clone())));
                            ui.close();
                        }
                        if ui.button("Delete tag (local)").clicked() {
                            actions.push(Action::DeleteTag(name.clone()));
                            ui.close();
                        }
                    });
                }
                if !any {
                    ui.label(theme::small("No tags. Right-click a change in History to create one."));
                }
            });
            if !refs.submodules.is_empty() {
                egui::CollapsingHeader::new(RichText::new(format!("{} Submodules", icon::SUBMODULE)).strong()).default_open(true).show(
                    ui,
                    |ui| {
                        for s in &refs.submodules {
                            let (resp, mut p) = row(ui, false, false);
                            p.text(icon::SUBMODULE, Some(ACCENT));
                            p.text(&s.path, None);
                            if !s.initialized {
                                p.small("not downloaded", ORANGE);
                            } else if s.out_of_sync {
                                p.small("needs update", YELLOW);
                            }
                            let resp = resp.on_hover_text("Double-click to open in its own tab");
                            if resp.double_clicked() && s.initialized {
                                actions.push(Action::OpenSubmodule(s.path.clone()));
                            }
                            let path = s.path.clone();
                            let init = s.initialized;
                            resp.context_menu(|ui| {
                                if init && ui.button("Open in new tab").clicked() {
                                    actions.push(Action::OpenSubmodule(path.clone()));
                                    ui.close();
                                }
                                if ui.button("Download / update submodules").clicked() {
                                    actions.push(Action::UpdateSubmodules);
                                    ui.close();
                                }
                            });
                        }
                    },
                );
            }
            egui::CollapsingHeader::new(RichText::new(format!("{} Servers (remotes)", icon::CLOUD)).strong()).default_open(false).show(
                ui,
                |ui| {
                    for (n, u) in &refs.remotes {
                        ui.label(format!("{n}: {u}"));
                    }
                    if ui.button("Add server…").clicked() {
                        actions.push(Action::AddRemote);
                    }
                },
            );
        });
    }

    // ------------------------------------------------------------ center

    fn center_tabs(&mut self, ui: &mut Ui) {
        let pending = self.pending_paths().len() + self.status.as_ref().map(|s| s.conflicts().count()).unwrap_or(0);
        let shelves = self.shelves.iter().filter(|s| !s.is_backup()).count();
        let locks = self.locks.len();
        let mut switched = false;
        ui.horizontal(|ui| {
            let mut tab = |ui: &mut Ui, t: CenterTab, label: String| {
                let sel = self.center == t;
                if ui.selectable_label(sel, RichText::new(label).size(15.0).strong()).clicked() && !sel {
                    self.center = t;
                    switched = true;
                }
            };
            tab(ui, CenterTab::Pending, format!("Pending ({pending})"));
            tab(ui, CenterTab::History, format!("{} History", icon::HISTORY));
            tab(ui, CenterTab::Shelves, format!("{} Shelves ({shelves})", icon::SHELVE));
            tab(ui, CenterTab::Locks, format!("{} Locks ({locks})", icon::LOCK));
            tab(ui, CenterTab::Log, "Log".to_string());
        });
        if switched {
            let keep = matches!(
                (&self.diff_target, self.center),
                (Some(DiffTarget::Working { .. } | DiffTarget::Incoming { .. }), CenterTab::Pending | CenterTab::Locks)
                    | (Some(DiffTarget::Commit { .. }), CenterTab::History)
                    | (Some(DiffTarget::Shelf { .. }), CenterTab::Shelves)
            );
            if !keep {
                self.clear_diff();
            }
        }
    }

    fn pending_view(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let Some(st) = self.status.clone() else {
            ui.add(egui::Spinner::new());
            return;
        };
        ui.horizontal(|ui| {
            if ui
                .button("✚ New changelist")
                .on_hover_text("Group related files together so they can be submitted or shelved as one")
                .clicked()
            {
                actions.push(Action::NewChangelist(self.selection.iter().filter(|p| self.file(p).is_some()).cloned().collect()));
            }
            ui.label(theme::small("Tip: drag files between changelists, right-click for more."));
        });
        ui.add_space(4.0);
        let modifiers = ui.input(|i| i.modifiers);
        egui::ScrollArea::vertical().id_salt("pending_scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            // Conflicts first.
            let conflicts: Vec<FileStatus> = st.conflicts().cloned().collect();
            if !conflicts.is_empty() || st.op != RepoOp::None {
                self.conflicts_section(ui, &conflicts, st.op, actions);
                ui.add_space(8.0);
            }
            let mut ids = vec![DEFAULT_ID];
            ids.extend(self.changelists.lists.iter().map(|c| c.id));
            // Order of all visible files for shift-selection.
            let order: Vec<String> = ids.iter().flat_map(|id| self.cl_files(*id)).collect();
            if order.is_empty() && conflicts.is_empty() {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(format!("{} No pending changes", icon::OK)).size(18.0).color(GREEN));
                    ui.label(theme::small("Edit, add or delete files in your workspace folder and they'll appear here automatically."));
                });
            }
            for id in ids {
                self.changelist_section(ui, id, &order, modifiers, actions);
            }
        });
    }

    fn conflicts_section(&mut self, ui: &mut Ui, conflicts: &[FileStatus], op: RepoOp, actions: &mut Vec<Action>) {
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, RED))
            .corner_radius(6)
            .inner_margin(8)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} Resolve conflicts ({})", icon::CONFLICT, conflicts.len())).strong().color(RED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Abort").on_hover_text("Cancel and go back to how things were").clicked() {
                            actions.push(Action::AbortOp);
                        }
                        if ui.add_enabled(conflicts.is_empty(), egui::Button::new(RichText::new("Continue").strong())).clicked() {
                            actions.push(Action::ContinueOp);
                        }
                    });
                });
                if conflicts.is_empty() {
                    ui.label(format!("All files resolved. Press Continue to finish the {}.", op.label()));
                    return;
                }
                ui.label(theme::small(
                    "These files were changed by you and by someone else. For art/binary files pick a whole version; for text you can also merge in a merge tool.",
                ));
                for f in conflicts {
                    ui.horizontal(|ui| {
                        let sel = self.selection.contains(&f.path);
                        if ui.selectable_label(sel, format!("{} {}", icon::CONFLICT, f.path)).clicked() {
                            self.selection = [f.path.clone()].into_iter().collect();
                            actions.push(Action::ShowDiff(DiffTarget::Working { path: f.path.clone() }));
                        }
                        let what = match (f.conflict_ours_exists(), f.conflict_theirs_exists()) {
                            (true, true) => "",
                            (false, _) | (_, false) => " (deleted on one side)",
                        };
                        ui.label(theme::small(what));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Mark resolved").on_hover_text("I edited the file myself and it's correct now").clicked() {
                                actions.push(Action::MarkResolved(f.path.clone()));
                            }
                            if ui.button("Merge tool").on_hover_text("Open your configured merge tool").clicked() {
                                actions.push(Action::MergeTool(f.path.clone()));
                            }
                            if ui.button("Take theirs").on_hover_text("Use the version from the server / other branch").clicked() {
                                actions.push(Action::Resolve(f.path.clone(), Side::Theirs));
                            }
                            if ui.button(RichText::new("Keep mine").strong()).on_hover_text("Use your version").clicked() {
                                actions.push(Action::Resolve(f.path.clone(), Side::Mine));
                            }
                        });
                    });
                }
            });
    }

    fn changelist_section(&mut self, ui: &mut Ui, id: u32, order: &[String], modifiers: egui::Modifiers, actions: &mut Vec<Action>) {
        let files = self.cl_files(id);
        // The default list is shown when it has files, or as a drop target next to numbered lists.
        if id != DEFAULT_ID || !files.is_empty() || !self.changelists.lists.is_empty() {
            let collapsed = self.collapsed_cls.contains(&id);
            let n_changed = files.iter().filter(|p| self.file(p).is_some()).count();
            let title = if id == DEFAULT_ID { "Default changelist".to_string() } else { format!("#{id}  {}", self.changelist_name(id)) };
            let header = egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .corner_radius(4)
                .inner_margin(egui::Margin::symmetric(6, 3))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        if ui.small_button(if collapsed { "⏵" } else { "⏷" }).clicked() {
                            if collapsed {
                                self.collapsed_cls.remove(&id);
                            } else {
                                self.collapsed_cls.insert(id);
                            }
                        }
                        ui.label(RichText::new(title).strong());
                        ui.label(theme::small(format!("{} file(s)", files.len())));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if id != DEFAULT_ID && ui.small_button("✏").on_hover_text("Edit description").clicked() {
                                actions.push(Action::EditChangelist(id));
                            }
                            if ui.add_enabled(n_changed > 0, egui::Button::new(format!("{} Shelve", icon::SHELVE)).small()).clicked() {
                                actions.push(Action::OpenShelve(files.clone(), Some(id)));
                            }
                            if ui
                                .add_enabled(
                                    n_changed > 0 && !self.is_busy(),
                                    egui::Button::new(RichText::new(format!("{} Submit…", icon::SUBMIT)).strong()).small(),
                                )
                                .clicked()
                            {
                                actions.push(Action::OpenSubmit(id));
                            }
                        });
                    });
                })
                .response;
            let header = header.interact(egui::Sense::click());
            if let Some(payload) = header.dnd_release_payload::<DraggedFiles>() {
                actions.push(Action::MoveFiles(payload.0.clone(), id));
            }
            if header.dnd_hover_payload::<DraggedFiles>().is_some() {
                ui.painter().rect_stroke(header.rect, 4.0, egui::Stroke::new(2.0, ACCENT), egui::StrokeKind::Inside);
            }
            header.context_menu(|ui| {
                if ui.button("Submit…").clicked() {
                    actions.push(Action::OpenSubmit(id));
                    ui.close();
                }
                if ui.button("Shelve…").clicked() {
                    actions.push(Action::OpenShelve(files.clone(), Some(id)));
                    ui.close();
                }
                if ui.button("Revert all files…").clicked() {
                    actions.push(Action::OpenRevert(files.clone()));
                    ui.close();
                }
                if id != DEFAULT_ID {
                    ui.separator();
                    if ui.button("Edit description…").clicked() {
                        actions.push(Action::EditChangelist(id));
                        ui.close();
                    }
                    if ui.button("Delete changelist (files move to Default)").clicked() {
                        actions.push(Action::DeleteChangelist(id));
                        ui.close();
                    }
                }
            });
            if collapsed {
                return;
            }
            if files.is_empty() {
                ui.label(theme::small("    (empty — drag files here)"));
            }
            for path in &files {
                let selected = self.selection.contains(path);
                let (resp, mut p) = row(ui, selected, true);
                p.indent(18.0);
                let f = self.file(path).cloned();
                match &f {
                    Some(f) => {
                        let (i, c, _) = kind_icon(f.kind);
                        p.text(i, Some(c));
                    }
                    None => {
                        p.text(icon::LOCK, Some(ACCENT));
                    }
                }
                let limit = p.right_edge() - 150.0;
                let (dir, name) = match path.rsplit_once('/') {
                    Some((d, n)) => (Some(d), n),
                    None => (None, path.as_str()),
                };
                p.text_clipped(name, None, limit);
                if let Some(d) = dir {
                    p.text_clipped(d, Some(MUTED), limit);
                }
                if let Some(o) = f.as_ref().and_then(|f| f.orig_path.clone()) {
                    p.small(&format!("from {o}"), MUTED);
                }
                let kind_label = match &f {
                    Some(f) if f.submodule => "submodule".to_string(),
                    Some(f) => f.kind.label().to_string(),
                    None => "checked out, unchanged".to_string(),
                };
                p.right_text(&kind_label, MUTED);
                if let Some(l) = self.lock_of(path) {
                    if l.ours {
                        p.right_text(icon::LOCK, ACCENT);
                    } else {
                        p.right_text(&format!("{} {}", icon::LOCK, l.owner), RED);
                    }
                }
                if self.incoming.contains_key(path) {
                    p.right_text(icon::WARN, RED);
                }
                let mut tip = vec![path.clone()];
                if self.incoming.contains_key(path) {
                    tip.push("Also changed on the server — get latest to avoid a conflict".into());
                }
                if let Some(l) = self.lock_of(path) {
                    tip.push(if l.ours { "Checked out by you".into() } else { format!("Checked out by {}", l.owner) });
                }
                let resp = resp.on_hover_text(tip.join("\n"));
                if resp.drag_started() {
                    if !self.selection.contains(path) {
                        self.selection = [path.clone()].into_iter().collect();
                    }
                    let dragged: Vec<String> =
                        self.selection.iter().filter(|p| self.file(p).is_some() || self.lock_of(p).is_some()).cloned().collect();
                    resp.dnd_set_drag_payload(DraggedFiles(dragged));
                }
                if resp.clicked() {
                    widgets::update_selection(&mut self.selection, &mut self.sel_anchor, order, path, modifiers);
                    if f.is_some() {
                        actions.push(Action::ShowDiff(DiffTarget::Working { path: path.clone() }));
                    }
                }
                if resp.double_clicked() {
                    actions.push(Action::OpenFile(path.clone()));
                }
                if resp.secondary_clicked() && !self.selection.contains(path) {
                    self.selection = [path.clone()].into_iter().collect();
                }
                if let Some(payload) = resp.dnd_release_payload::<DraggedFiles>() {
                    actions.push(Action::MoveFiles(payload.0.clone(), id));
                }
                let pth = path.clone();
                resp.context_menu(|ui| {
                    if ui.button("Reveal in workspace tree").clicked() {
                        actions.push(Action::RevealInTree(pth.clone()));
                        ui.close();
                    }
                    self.file_menu(ui, &pth, actions);
                });
            }
            ui.add_space(6.0);
        }
    }

    fn history_view(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let mut reload = false;
        ui.horizontal(|ui| {
            let label = match &self.history_scope {
                HistoryScope::Current => "This branch".to_string(),
                HistoryScope::All => "All branches".to_string(),
                HistoryScope::File(p) => format!("File: {p}"),
            };
            egui::ComboBox::from_id_salt("hist_scope").selected_text(label).width(200.0).show_ui(ui, |ui| {
                if ui.selectable_label(self.history_scope == HistoryScope::Current, "This branch").clicked() {
                    self.history_scope = HistoryScope::Current;
                    reload = true;
                }
                if ui.selectable_label(self.history_scope == HistoryScope::All, "All branches").clicked() {
                    self.history_scope = HistoryScope::All;
                    reload = true;
                }
            });
            if matches!(self.history_scope, HistoryScope::File(_))
                && ui.small_button("✖").on_hover_text("Show the whole branch again").clicked()
            {
                self.history_scope = HistoryScope::Current;
                reload = true;
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.history_filter).hint_text("🔍 Search description, author or id").desired_width(260.0),
            );
            if self.history_loading {
                ui.add(egui::Spinner::new());
            }
            ui.label(theme::small(format!("{} change(s)", self.history.len())));
        });
        if reload {
            self.request_history();
        }
        if let Some(e) = &self.history_err {
            ui.colored_label(RED, e);
        }
        if self.history.is_empty() && !self.history_loading {
            ui.label(theme::small("No submitted changes yet."));
            return;
        }
        let filter = self.history_filter.to_lowercase();
        let visible: Vec<usize> = (0..self.history.len())
            .filter(|&i| {
                let c = &self.history[i];
                filter.is_empty()
                    || c.subject.to_lowercase().contains(&filter)
                    || c.author.to_lowercase().contains(&filter)
                    || c.id.starts_with(&filter)
            })
            .collect();
        let show_graph = filter.is_empty();
        let max_lanes = if show_graph { self.graph.iter().map(|g| g.width).max().unwrap_or(1).min(12) } else { 0 };
        let lane_w = 14.0;
        let graph_w = max_lanes as f32 * lane_w + 8.0;
        let head = self.status.as_ref().and_then(|s| s.branch.oid.clone());
        let ahead = self.status.as_ref().map(|s| s.branch.ahead).unwrap_or(0);
        let has_upstream = self.status.as_ref().is_some_and(|s| s.branch.upstream.is_some());
        egui::ScrollArea::vertical().id_salt("history_scroll").auto_shrink([false, false]).show_rows(
            ui,
            ROW_H,
            visible.len(),
            |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for &i in &visible[range] {
                    let c = self.history[i].clone();
                    let selected = self.selected_commit.as_deref() == Some(c.id.as_str());
                    let (resp, mut p) = row(ui, selected, false);
                    if show_graph {
                        if let Some(g) = self.graph.get(i) {
                            let x0 = p.rect.left() + 8.0;
                            let col_x = |c: usize| x0 + (c.min(11) as f32) * lane_w;
                            let top = p.rect.top();
                            let mid = p.rect.center().y;
                            let bot = p.rect.bottom();
                            for (a, b) in &g.up {
                                p.painter.line_segment(
                                    [egui::pos2(col_x(*a), top), egui::pos2(col_x(*b), mid)],
                                    egui::Stroke::new(2.0, theme::lane_color(*a)),
                                );
                            }
                            for (a, b) in &g.down {
                                p.painter.line_segment(
                                    [egui::pos2(col_x(*a), mid), egui::pos2(col_x(*b), bot)],
                                    egui::Stroke::new(2.0, theme::lane_color(*b)),
                                );
                            }
                            let is_head = head.as_deref() == Some(c.id.as_str());
                            let color = theme::lane_color(g.node);
                            if is_head {
                                p.painter.circle_filled(egui::pos2(col_x(g.node), mid), 5.5, ui.visuals().panel_fill);
                                p.painter.circle_stroke(egui::pos2(col_x(g.node), mid), 5.0, egui::Stroke::new(2.5, color));
                            } else {
                                p.painter.circle_filled(egui::pos2(col_x(g.node), mid), 4.5, color);
                            }
                        }
                        p.indent(graph_w);
                    }
                    for r in c.refs.iter().filter(|r| !r.ends_with("/HEAD")) {
                        let (label, color) = if let Some(b) = r.strip_prefix("HEAD -> ") {
                            (b.to_string(), GREEN)
                        } else if let Some(t) = r.strip_prefix("tag: ") {
                            (format!("{} {t}", icon::TAG), ORANGE)
                        } else if r == "HEAD" {
                            ("HEAD".to_string(), GREEN)
                        } else if r.contains('/') {
                            (r.clone(), ACCENT)
                        } else {
                            (r.clone(), PURPLE)
                        };
                        p.pill(&label, color);
                    }
                    let date = theme::format_time(c.time);
                    p.right_text(c.short_id(), MUTED);
                    p.right_text(&date, MUTED);
                    let author_r = p.right_text(&c.author, MUTED);
                    let limit = author_r.left() - 12.0;
                    p.text_clipped(&c.subject, None, limit);
                    let resp = resp.on_hover_text(format!("{}\n{} <{}>\n{}", c.subject, c.author, c.email, c.id));
                    if resp.clicked() {
                        actions.push(Action::SelectCommit(c.id.clone()));
                    }
                    let id = c.id.clone();
                    let is_merge = c.parents.len() > 1;
                    let is_head = head.as_deref() == Some(c.id.as_str());
                    resp.context_menu(|ui| {
                        if ui.button("Copy change id").clicked() {
                            ui.ctx().copy_text(id.clone());
                            ui.close();
                        }
                        if ui.button("New branch from here…").clicked() {
                            actions.push(Action::NewBranch(Some(id.clone())));
                            ui.close();
                        }
                        if ui.button(format!("{} New tag here…", icon::TAG)).clicked() {
                            actions.push(Action::NewTag(id.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Back out this change…").on_hover_text("Create a new change that undoes this one").clicked() {
                            actions.push(Action::RevertCommit(id.clone(), is_merge));
                            ui.close();
                        }
                        if !is_head && !is_merge && ui.button("Copy this change to my branch (cherry-pick)").clicked() {
                            actions.push(Action::CherryPick(id.clone()));
                            ui.close();
                        }
                        let can_undo = is_head && (ahead > 0 || !has_upstream);
                        if can_undo && ui.button("Undo this commit (keep changes)…").clicked() {
                            actions.push(Action::UndoLastCommit);
                            ui.close();
                        }
                    });
                }
            },
        );
    }

    fn shelves_view(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let backups = self.shelves.iter().filter(|s| s.is_backup()).count();
        ui.horizontal(|ui| {
            ui.label(theme::small(
                "Shelves keep changes on the side without submitting them. Share a shelf to let teammates review or take over your work.",
            ));
        });
        ui.checkbox(&mut self.show_backups, format!("Show automatic backups ({backups})"))
            .on_hover_text("g4v saves a backup shelf before every revert or overwrite so nothing is ever lost");
        ui.add_space(4.0);
        let me = self.identity.name.clone().unwrap_or_default();
        egui::ScrollArea::vertical().id_salt("shelves_scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let list: Vec<_> = self.shelves.iter().filter(|s| self.show_backups || !s.is_backup()).cloned().collect();
            if list.is_empty() {
                ui.add_space(20.0);
                ui.label(theme::small("No shelves. Select files in Pending and press Shelve."));
            }
            for s in list {
                let selected = self.selected_shelf.as_deref() == Some(s.refname.as_str());
                let (resp, mut p) = row(ui, selected, false);
                let (ic, col) = if s.is_backup() { (icon::REVERT, MUTED) } else { (icon::SHELVE, ORANGE) };
                p.text(ic, Some(col));
                p.right_text(&theme::format_time(s.time), MUTED);
                let who = if s.remote.is_some() || s.author != me { s.author.clone() } else { "me".to_string() };
                let r = p.right_text(&who, MUTED);
                if s.shared {
                    p.right_text(&format!("{} shared", icon::CLOUD), ACCENT);
                } else if let Some(rm) = &s.remote {
                    p.right_text(&format!("{} from {rm}", icon::CLOUD), PURPLE);
                }
                if let Some(files) = self.shelf_files.get(&s.id) {
                    p.right_text(&format!("{} file(s)", files.len()), MUTED);
                }
                let _ = r;
                let limit = p.right_edge();
                p.text_clipped(&s.display_name(), None, limit);
                if resp.clicked() {
                    actions.push(Action::SelectShelf(s.refname.clone()));
                }
                let sh = s.clone();
                resp.context_menu(|ui| {
                    if ui.button("Unshelve…").clicked() {
                        actions.push(Action::SelectShelf(sh.refname.clone()));
                        actions.push(Action::OpenUnshelve(sh.clone(), vec![]));
                        ui.close();
                    }
                    if sh.remote.is_none() && !sh.shared && ui.button("Share with team").clicked() {
                        actions.push(Action::ShareShelf(sh.clone()));
                        ui.close();
                    }
                    if sh.shared && ui.button("Stop sharing").clicked() {
                        actions.push(Action::UnshareShelf(sh.clone()));
                        ui.close();
                    }
                    if ui.button("Delete…").clicked() {
                        actions.push(Action::DeleteShelf(sh.clone()));
                        ui.close();
                    }
                });
            }
            if let Some(refs) = &self.refs {
                if !refs.stashes.is_empty() {
                    ui.add_space(12.0);
                    ui.label(theme::strong("Git stashes"));
                    ui.label(theme::small("Created by other Git tools. Apply to bring the changes back."));
                    for s in &refs.stashes {
                        ui.horizontal(|ui| {
                            ui.label(format!("stash@{{{}}}  {}", s.index, s.message));
                            ui.label(theme::small(theme::format_time(s.time)));
                            if ui.small_button("Apply & remove").clicked() {
                                actions.push(Action::StashApply(s.index, true));
                            }
                            if ui.small_button("Apply").clicked() {
                                actions.push(Action::StashApply(s.index, false));
                            }
                            if ui.small_button("Drop…").clicked() {
                                actions.push(Action::Confirm {
                                    title: "Drop stash?".into(),
                                    body: format!("stash@{{{}}} will be deleted permanently.", s.index),
                                    action: Box::new(Action::StashDrop(s.index)),
                                });
                            }
                        });
                    }
                }
            }
        });
    }

    fn locks_view(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        if !self.uses_lfs {
            ui.add_space(10.0);
            ui.label(theme::strong("File locking is not set up for this repository."));
            ui.label(
                "Locking (\"check out\") prevents two people from editing the same binary file. It needs Git LFS and a server that supports locks (GitHub, GitLab, Bitbucket, Gitea…).",
            );
            ui.label("To enable it, store your art files with LFS as lockable, e.g. right-click a new file › 'Store all *.ext files with Git LFS', or run:");
            ui.code("git lfs track --lockable \"*.psd\"");
            return;
        }
        ui.horizontal(|ui| {
            if ui.button(format!("{} Refresh locks", icon::REFRESH)).clicked() {
                self.request_locks();
            }
            if let Some(e) = &self.locks_err {
                ui.colored_label(ORANGE, format!("{} {e}", icon::WARN));
            }
        });
        ui.add_space(4.0);
        let mut locks = self.locks.clone();
        locks.sort_by(|a, b| b.ours.cmp(&a.ours).then(a.path.cmp(&b.path)));
        if locks.is_empty() {
            ui.label(theme::small("No files are checked out by anyone."));
        }
        egui::ScrollArea::vertical().id_salt("locks_scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for l in locks {
                let (resp, mut p) = row(ui, self.selection.contains(&l.path), false);
                p.text(icon::LOCK, Some(if l.ours { ACCENT } else { RED }));
                p.right_text(&l.locked_at.chars().take(16).collect::<String>().replace('T', " "), MUTED);
                p.right_text(if l.ours { "me" } else { &l.owner }, if l.ours { ACCENT } else { RED });
                if self.file(&l.path).is_some() {
                    p.right_text("modified", MUTED);
                }
                let limit = p.right_edge();
                p.text_clipped(&l.path, None, limit);
                if resp.clicked() {
                    self.selection = [l.path.clone()].into_iter().collect();
                }
                let path = l.path.clone();
                let ours = l.ours;
                resp.context_menu(|ui| {
                    if ours && ui.button("Release lock").clicked() {
                        actions.push(Action::Unlock(vec![path.clone()], false));
                        ui.close();
                    }
                    if !ours && ui.button("Break lock…").clicked() {
                        actions.push(Action::Unlock(vec![path.clone()], true));
                        ui.close();
                    }
                    if ui.button("Show in workspace").clicked() {
                        actions.push(Action::RevealInTree(path.clone()));
                        ui.close();
                    }
                });
            }
        });
    }
}

fn short_list(v: &[String], n: usize) -> String {
    let mut s = v.iter().take(n).cloned().collect::<Vec<_>>().join(", ");
    if v.len() > n {
        s.push_str(&format!(" and {} more", v.len() - n));
    }
    s
}

fn log_view(ui: &mut Ui, log: &CmdLog) {
    ui.horizontal(|ui| {
        ui.label(theme::small("Every git command g4v runs is listed here, newest first."));
        if ui.button("Copy all").clicked() {
            let text: String = log
                .snapshot()
                .iter()
                .map(|e| {
                    format!(
                        "[{}] {} ({} ms){}\n{}\n",
                        e.time.format("%H:%M:%S"),
                        e.command,
                        e.millis,
                        if e.ok { "" } else { " FAILED" },
                        e.output
                    )
                })
                .collect();
            ui.ctx().copy_text(text);
        }
        if ui.button("Clear").clicked() {
            log.clear();
        }
    });
    let entries = log.snapshot();
    egui::ScrollArea::vertical().id_salt("log_scroll").auto_shrink([false, false]).show(ui, |ui| {
        for e in entries.iter().rev() {
            let color = if e.ok { MUTED } else { RED };
            let head = format!("{}  [{}]  {}  ({} ms)", e.time.format("%H:%M:%S"), e.repo, e.command, e.millis);
            if e.output.is_empty() {
                ui.label(RichText::new(head).monospace().color(color));
            } else {
                egui::CollapsingHeader::new(RichText::new(head).monospace().color(color))
                    .id_salt((e.time.timestamp_nanos_opt(), &e.command))
                    .show(ui, |ui| {
                        ui.label(RichText::new(&e.output).monospace().small());
                    });
            }
        }
    });
}
