//! g4v Windows installer / uninstaller.
//!
//! A single self-contained executable that embeds `g4v.exe`. It installs per user
//! (no administrator rights needed) into `%LOCALAPPDATA%\Programs\g4v`, creates
//! Start Menu / desktop shortcuts, and registers in "Apps & features" so g4v can
//! be uninstalled normally. The installed copy of this program is the uninstaller.
//!
//! Command line:
//!   g4v-setup.exe                         interactive install
//!   g4v-setup.exe --silent [--dir D]      unattended install
//!       [--no-shortcuts] [--no-register]
//!   g4v-setup.exe --uninstall [--silent]  remove g4v
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, RichText};
use std::path::{Path, PathBuf};
use std::process::Command;

const PAYLOAD: &[u8] = include_bytes!(env!("G4V_PAYLOAD"));
const VERSION: &str = env!("G4V_VERSION");
const APP_EXE: &str = "g4v.exe";
const UNINSTALL_EXE: &str = "uninstall.exe";
const REG_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\g4v";

#[derive(Clone)]
struct Options {
    dir: PathBuf,
    start_menu: bool,
    desktop: bool,
    register: bool,
}

fn default_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\g4v"));
    base.join("Programs").join("g4v")
}

#[cfg(windows)]
fn hide(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000)
}
#[cfg(not(windows))]
fn hide(cmd: &mut Command) -> &mut Command {
    cmd
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let out = hide(Command::new(program).args(args)).output().map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn start_menu_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(r"Microsoft\Windows\Start Menu\Programs"))
}

fn desktop_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(|u| PathBuf::from(u).join("Desktop"))
}

/// Creates a .lnk shortcut using the Windows Script Host COM object.
fn create_shortcut(link: &Path, target: &Path, workdir: &Path) -> Result<(), String> {
    let q = |p: &Path| p.display().to_string().replace('\'', "''");
    let script = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.WorkingDirectory='{}';$s.IconLocation='{},0';$s.Description='g4v - visual Git client';$s.Save()",
        q(link),
        q(target),
        q(workdir),
        q(target)
    );
    run("powershell", &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
}

fn is_running(exe: &Path) -> bool {
    // A running exe cannot be opened for writing on Windows.
    exe.exists() && std::fs::OpenOptions::new().write(true).open(exe).is_err()
}

fn install(o: &Options) -> Result<PathBuf, String> {
    let app = o.dir.join(APP_EXE);
    if is_running(&app) {
        return Err("g4v is currently running. Close it and try again.".into());
    }
    std::fs::create_dir_all(&o.dir).map_err(|e| format!("Could not create {}: {e}", o.dir.display()))?;
    // Write to a temp name then rename, so a failed copy never leaves a broken g4v.exe.
    let tmp = o.dir.join("g4v.exe.new");
    std::fs::write(&tmp, PAYLOAD).map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &app).map_err(|e| format!("Could not replace {}: {e}", app.display()))?;

    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let uninstaller = o.dir.join(UNINSTALL_EXE);
    if me != uninstaller {
        std::fs::copy(&me, &uninstaller).map_err(|e| format!("Could not write the uninstaller: {e}"))?;
    }

    if o.start_menu {
        if let Some(d) = start_menu_dir() {
            create_shortcut(&d.join("g4v.lnk"), &app, &o.dir)?;
        }
    }
    if o.desktop {
        if let Some(d) = desktop_dir() {
            create_shortcut(&d.join("g4v.lnk"), &app, &o.dir)?;
        }
    }
    if o.register {
        let size_kb = (PAYLOAD.len() / 1024 + std::fs::metadata(&uninstaller).map(|m| m.len() as usize / 1024).unwrap_or(0)).to_string();
        let uninstall_cmd = format!("\"{}\" --uninstall", uninstaller.display());
        let quiet_cmd = format!("\"{}\" --uninstall --silent", uninstaller.display());
        let icon = app.display().to_string();
        let loc = o.dir.display().to_string();
        let values: [(&str, &str, &str); 10] = [
            ("DisplayName", "REG_SZ", "g4v"),
            ("DisplayVersion", "REG_SZ", VERSION),
            ("Publisher", "REG_SZ", "g4v contributors"),
            ("DisplayIcon", "REG_SZ", &icon),
            ("InstallLocation", "REG_SZ", &loc),
            ("UninstallString", "REG_SZ", &uninstall_cmd),
            ("QuietUninstallString", "REG_SZ", &quiet_cmd),
            ("EstimatedSize", "REG_DWORD", &size_kb),
            ("NoModify", "REG_DWORD", "1"),
            ("NoRepair", "REG_DWORD", "1"),
        ];
        for (name, kind, data) in values {
            run("reg", &["add", REG_KEY, "/v", name, "/t", kind, "/d", data, "/f"])?;
        }
    }
    Ok(app)
}

/// Install directory recorded in the registry, else the folder this program runs from.
fn installed_dir() -> PathBuf {
    let out = hide(Command::new("reg").args(["query", REG_KEY, "/v", "InstallLocation"])).output();
    if let Ok(o) = out {
        let text = String::from_utf8_lossy(&o.stdout);
        if let Some(line) = text.lines().find(|l| l.contains("InstallLocation")) {
            if let Some(idx) = line.find("REG_SZ") {
                let p = line[idx + 6..].trim();
                if !p.is_empty() {
                    return PathBuf::from(p);
                }
            }
        }
    }
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(default_dir)
}

fn uninstall() -> Result<(), String> {
    let dir = installed_dir();
    let app = dir.join(APP_EXE);
    if is_running(&app) {
        return Err("g4v is currently running. Close it and try again.".into());
    }
    for d in [start_menu_dir(), desktop_dir()].into_iter().flatten() {
        let lnk = d.join("g4v.lnk");
        if lnk.exists() {
            let _ = std::fs::remove_file(lnk);
        }
    }
    let _ = run("reg", &["delete", REG_KEY, "/f"]);
    if app.exists() {
        std::fs::remove_file(&app).map_err(|e| format!("Could not remove {}: {e}", app.display()))?;
    }
    // Only g4v's own files are removed. User settings (%APPDATA%\g4v) and repositories are untouched.
    // This program may be the uninstaller inside `dir`, which can't delete itself while running:
    // hand that off to a short-lived background command.
    let me = std::env::current_exe().ok();
    let uninstaller = dir.join(UNINSTALL_EXE);
    if me.as_deref() == Some(uninstaller.as_path()) {
        let cmd = format!(
            "ping 127.0.0.1 -n 3 > nul & del /f /q \"{}\" & rmdir \"{}\"",
            uninstaller.display(),
            dir.display()
        );
        let mut c = Command::new("cmd");
        // cmd.exe has its own quoting rules; pass the command line through untouched.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.raw_arg("/C").raw_arg(&cmd);
        }
        let _ = hide(&mut c).spawn();
    } else {
        let _ = std::fs::remove_file(&uninstaller);
        let _ = std::fs::remove_dir(&dir);
    }
    Ok(())
}

// ------------------------------------------------------------------ GUI

enum Page {
    Welcome,
    Done(PathBuf),
    UninstallConfirm,
    Uninstalled,
    Failed(String),
}

struct SetupApp {
    page: Page,
    opts: Options,
    dir_text: String,
    launch: bool,
}

impl eframe::App for SetupApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("g4v").size(30.0).strong().color(egui::Color32::from_rgb(64, 156, 255)));
            ui.label(RichText::new(format!("Version {VERSION} · visual Git client for artists and teams")).weak());
            ui.add_space(14.0);
            let mut next: Option<Page> = None;
            match &self.page {
                Page::Welcome => {
                    ui.label("Install location");
                    ui.add(egui::TextEdit::singleline(&mut self.dir_text).desired_width(f32::INFINITY));
                    ui.label(RichText::new("Installs for your user account only — no administrator rights needed.").small().weak());
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.opts.start_menu, "Add to Start menu");
                    ui.checkbox(&mut self.opts.desktop, "Create a desktop shortcut");
                    ui.checkbox(&mut self.launch, "Start g4v when finished");
                    ui.add_space(6.0);
                    ui.label(RichText::new("g4v needs Git (git-scm.com). Git LFS is recommended for art files.").small().weak());
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new("Install").strong()).min_size(egui::vec2(110.0, 30.0))).clicked() {
                            self.opts.dir = PathBuf::from(self.dir_text.trim());
                            next = Some(match install(&self.opts) {
                                Ok(app) => {
                                    if self.launch {
                                        let _ = Command::new(&app).current_dir(&self.opts.dir).spawn();
                                    }
                                    Page::Done(app)
                                }
                                Err(e) => Page::Failed(e),
                            });
                        }
                        if ui.button("Cancel").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                }
                Page::Done(app) => {
                    ui.label(RichText::new("✔ g4v is installed.").size(16.0).color(egui::Color32::from_rgb(80, 190, 110)));
                    ui.label(format!("Location: {}", app.display()));
                    ui.label("You can uninstall it any time from Settings › Apps.");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if !self.launch && ui.button("Start g4v").clicked() {
                            let _ = Command::new(app).spawn();
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if ui.button("Close").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                }
                Page::UninstallConfirm => {
                    ui.label("Remove g4v from this computer?");
                    ui.label(RichText::new("Your repositories and g4v settings are not touched.").small().weak());
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new("Uninstall").strong()).min_size(egui::vec2(110.0, 30.0))).clicked() {
                            next = Some(match uninstall() {
                                Ok(()) => Page::Uninstalled,
                                Err(e) => Page::Failed(e),
                            });
                        }
                        if ui.button("Cancel").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                }
                Page::Uninstalled => {
                    ui.label("g4v was removed.");
                    if ui.button("Close").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                Page::Failed(e) => {
                    ui.label(RichText::new("Something went wrong").size(16.0).color(egui::Color32::from_rgb(235, 87, 87)));
                    ui.label(e.as_str());
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Back").clicked() {
                            next = Some(Page::Welcome);
                        }
                        if ui.button("Close").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                }
            }
            if let Some(p) = next {
                self.page = p;
            }
        });
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let uninstall_mode = has("--uninstall");
    let mut opts = Options {
        dir: value("--dir").map(PathBuf::from).unwrap_or_else(default_dir),
        start_menu: !has("--no-shortcuts"),
        desktop: false,
        register: !has("--no-register"),
    };

    if has("--silent") {
        let res = if uninstall_mode { uninstall() } else { install(&opts).map(|_| ()) };
        if let Err(e) = res {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }

    opts.desktop = true;
    let app = SetupApp {
        page: if uninstall_mode { Page::UninstallConfirm } else { Page::Welcome },
        dir_text: opts.dir.display().to_string(),
        opts,
        launch: true,
    };
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(if uninstall_mode { "Uninstall g4v" } else { "g4v Setup" })
            .with_inner_size([520.0, 380.0])
            .with_resizable(false)
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../../assets/g4v.png")).unwrap_or_default()),
        ..Default::default()
    };
    let _ = eframe::run_native("g4v-setup", native, Box::new(|_| Ok(Box::new(app))));
}
