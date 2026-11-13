//! Thin, reliable wrapper around the `git` command line.
//!
//! g4v deliberately shells out to the user's installed `git` instead of using a
//! library implementation: this guarantees identical behaviour to the command
//! line, including Git LFS filters, hooks, credential helpers, submodules and
//! every config option the user (or their studio) has set up.
//!
//! Every invocation:
//! * never opens an editor or a terminal prompt (so nothing can hang waiting
//!   for input that the GUI cannot provide),
//! * treats pathspecs literally (file names containing `*`, `[` or `:` are
//!   never interpreted as patterns),
//! * is recorded in a shared command log that the UI exposes, so users and
//!   support staff can always see exactly what was run.

pub mod branches;
pub mod diff;
pub mod history;
pub mod lfs;
pub mod ops;
pub mod shelf;
pub mod status;

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One executed git command, as shown in the "Log" tab.
#[derive(Debug, Clone)]
pub struct CmdLogEntry {
    pub time: chrono::DateTime<chrono::Local>,
    pub repo: String,
    pub command: String,
    pub ok: bool,
    pub millis: u128,
    pub output: String,
}

/// Shared, bounded log of executed commands.
#[derive(Clone, Default)]
pub struct CmdLog(Arc<Mutex<VecDeque<CmdLogEntry>>>);

impl CmdLog {
    const MAX: usize = 1000;

    pub fn push(&self, e: CmdLogEntry) {
        if let Ok(mut q) = self.0.lock() {
            if q.len() >= Self::MAX {
                q.pop_front();
            }
            q.push_back(e);
        }
    }

    pub fn snapshot(&self) -> Vec<CmdLogEntry> {
        self.0.lock().map(|q| q.iter().cloned().collect()).unwrap_or_default()
    }

    pub fn clear(&self) {
        if let Ok(mut q) = self.0.lock() {
            q.clear();
        }
    }
}

/// A failed git invocation.
#[derive(Debug, Clone)]
pub struct GitError {
    pub command: String,
    pub code: Option<i32>,
    pub stderr: String,
    pub stdout: String,
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = if !self.stderr.trim().is_empty() { self.stderr.trim() } else { self.stdout.trim() };
        if msg.is_empty() {
            write!(f, "`{}` failed (exit code {:?})", self.command, self.code)
        } else {
            write!(f, "{msg}")
        }
    }
}

impl std::error::Error for GitError {}

pub type GitResult<T> = Result<T, GitError>;

/// Raw output of a successful command.
#[derive(Debug, Clone)]
pub struct GitOutput {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub code: i32,
}

impl GitOutput {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Splits NUL separated output into owned strings, dropping the empty tail.
pub fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect()
}

/// Splits NUL separated output keeping empty fields (except the trailing one).
pub fn split_nul_keep_empty(bytes: &[u8]) -> Vec<String> {
    let mut v: Vec<String> = bytes.split(|b| *b == 0).map(|s| String::from_utf8_lossy(s).into_owned()).collect();
    if v.last().is_some_and(|s| s.is_empty()) {
        v.pop();
    }
    v
}

/// Encodes paths for `--pathspec-from-file=- --pathspec-file-nul`.
pub fn nul_join<S: AsRef<str>>(paths: &[S]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in paths {
        out.extend_from_slice(p.as_ref().as_bytes());
        out.push(0);
    }
    out
}

/// Locates the git executable once.
fn git_exe() -> &'static OsStr {
    use std::sync::OnceLock;
    static EXE: OnceLock<OsString> = OnceLock::new();
    EXE.get_or_init(|| std::env::var_os("G4V_GIT").unwrap_or_else(|| OsString::from("git")))
}

/// Returns `git --version`, or an error if git is not installed.
pub fn git_version() -> Result<String, String> {
    let mut cmd = Command::new(git_exe());
    cmd.arg("--version");
    hide_window(&mut cmd);
    match cmd.output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(format!("Git was not found on this computer ({e}). Install Git from https://git-scm.com and restart g4v.")),
    }
}

#[cfg(windows)]
fn hide_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_cmd: &mut Command) {}

fn apply_safe_env(cmd: &mut Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .env("LANGUAGE", "en")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
}

/// A repository working copy (non-bare).
#[derive(Clone)]
pub struct Repo {
    root: PathBuf,
    git_dir: PathBuf,
    log: CmdLog,
}

impl fmt::Debug for Repo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Repo").field("root", &self.root).finish()
    }
}

impl Repo {
    /// Opens the repository containing `path`.
    pub fn open(path: &Path, log: CmdLog) -> anyhow::Result<Repo> {
        if !path.exists() {
            anyhow::bail!("The folder {} does not exist.", path.display());
        }
        let mut cmd = Command::new(git_exe());
        cmd.current_dir(path).args(["rev-parse", "--is-bare-repository", "--show-toplevel", "--absolute-git-dir"]);
        apply_safe_env(&mut cmd);
        hide_window(&mut cmd);
        let out = cmd.output().map_err(|e| anyhow::anyhow!("Could not run git: {e}"))?;
        if !out.status.success() {
            anyhow::bail!("{} is not inside a Git repository.\n{}", path.display(), String::from_utf8_lossy(&out.stderr).trim());
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines();
        let bare = lines.next().unwrap_or("false").trim() == "true";
        if bare {
            anyhow::bail!("{} is a bare repository (it has no working files).", path.display());
        }
        let root = PathBuf::from(lines.next().unwrap_or_default().trim());
        let git_dir = PathBuf::from(lines.next().unwrap_or_default().trim());
        if root.as_os_str().is_empty() || git_dir.as_os_str().is_empty() {
            anyhow::bail!("Could not determine the repository layout of {}", path.display());
        }
        Ok(Repo { root: normalize(root), git_dir: normalize(git_dir), log })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    pub fn cmd_log(&self) -> &CmdLog {
        &self.log
    }

    /// Short display name (folder name).
    pub fn name(&self) -> String {
        self.root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| self.root.display().to_string())
    }

    /// Directory where g4v keeps its own per-repository metadata.
    pub fn meta_dir(&self) -> PathBuf {
        self.git_dir.join("g4v")
    }

    /// Absolute path of a repository-relative path.
    pub fn abs(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn git(&self) -> GitCmd<'_> {
        GitCmd { repo: self, args: Vec::new(), stdin: None, envs: Vec::new(), ok_codes: vec![0], quiet: false }
    }

    /// Convenience: run with args and return trimmed stdout.
    pub fn run(&self, args: &[&str]) -> GitResult<String> {
        Ok(self.git().args(args).run()?.text().trim_end().to_string())
    }

    /// Reads a config value, `None` when unset.
    pub fn config_get(&self, key: &str) -> Option<String> {
        self.git()
            .args(["config", "--get", key])
            .ok_codes(&[0, 1])
            .quiet()
            .run()
            .ok()
            .map(|o| o.text().trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

fn normalize(p: PathBuf) -> PathBuf {
    // Git for Windows reports `C:/foo/bar`; keep it but make sure separators are native.
    #[cfg(windows)]
    {
        PathBuf::from(p.to_string_lossy().replace('/', "\\"))
    }
    #[cfg(not(windows))]
    {
        p
    }
}

/// Builder for one git invocation.
pub struct GitCmd<'a> {
    repo: &'a Repo,
    args: Vec<OsString>,
    stdin: Option<Vec<u8>>,
    envs: Vec<(OsString, OsString)>,
    ok_codes: Vec<i32>,
    quiet: bool,
}

impl<'a> GitCmd<'a> {
    pub fn arg(mut self, a: impl AsRef<OsStr>) -> Self {
        self.args.push(a.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for a in args {
            self.args.push(a.as_ref().to_os_string());
        }
        self
    }

    pub fn stdin(mut self, data: Vec<u8>) -> Self {
        self.stdin = Some(data);
        self
    }

    /// Feed `paths` as a NUL separated pathspec list on stdin.
    /// The caller must add `--pathspec-from-file=- --pathspec-file-nul`.
    pub fn stdin_paths<S: AsRef<str>>(self, paths: &[S]) -> Self {
        self.stdin(nul_join(paths))
    }

    pub fn env(mut self, k: impl AsRef<OsStr>, v: impl AsRef<OsStr>) -> Self {
        self.envs.push((k.as_ref().to_os_string(), v.as_ref().to_os_string()));
        self
    }

    /// Exit codes that count as success (default: only 0).
    pub fn ok_codes(mut self, codes: &[i32]) -> Self {
        self.ok_codes = codes.to_vec();
        self
    }

    /// Don't record in the command log (used for high-frequency polling).
    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    fn display(&self) -> String {
        let mut s = String::from("git");
        for a in &self.args {
            let a = a.to_string_lossy();
            if a.contains(' ') || a.is_empty() {
                s.push_str(&format!(" \"{a}\""));
            } else {
                s.push(' ');
                s.push_str(&a);
            }
        }
        s
    }

    pub fn run(self) -> GitResult<GitOutput> {
        let display = self.display();
        let started = Instant::now();
        let mut cmd = Command::new(git_exe());
        cmd.current_dir(&self.repo.root)
            .args(["-c", "core.quotepath=false", "-c", "color.ui=false", "-c", "advice.detachedHead=false"])
            .args(&self.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if self.stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        apply_safe_env(&mut cmd);
        for (k, v) in &self.envs {
            cmd.env(k, v);
        }
        hide_window(&mut cmd);

        let fail = |stderr: String, code: Option<i32>, stdout: String| GitError { command: display.clone(), code, stderr, stdout };

        let mut child = cmd.spawn().map_err(|e| fail(format!("Could not start git: {e}"), None, String::new()))?;
        // Write stdin from a helper thread so large inputs can't deadlock with stdout.
        let writer = match (self.stdin, child.stdin.take()) {
            (Some(data), Some(mut pipe)) => Some(std::thread::spawn(move || {
                let _ = pipe.write_all(&data);
            })),
            _ => None,
        };
        let out = child.wait_with_output().map_err(|e| fail(format!("git did not finish: {e}"), None, String::new()))?;
        if let Some(w) = writer {
            let _ = w.join();
        }
        let code = out.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let ok = self.ok_codes.contains(&code);
        if !self.quiet || !ok {
            let mut output = stderr.trim().to_string();
            if !ok && output.is_empty() {
                output = String::from_utf8_lossy(&out.stdout).trim().chars().take(2000).collect();
            }
            self.repo.log.push(CmdLogEntry {
                time: chrono::Local::now(),
                repo: self.repo.name(),
                command: display.clone(),
                ok,
                millis: started.elapsed().as_millis(),
                output,
            });
        }
        if !ok {
            return Err(fail(stderr, Some(code), String::from_utf8_lossy(&out.stdout).into_owned()));
        }
        Ok(GitOutput { stdout: out.stdout, stderr, code })
    }
}

/// Runs git outside of any repository (clone, init, global config).
pub fn run_global(args: &[&OsStr], cwd: Option<&Path>, log: &CmdLog) -> GitResult<String> {
    let started = Instant::now();
    let mut cmd = Command::new(git_exe());
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    cmd.args(args).stdin(Stdio::null());
    apply_safe_env(&mut cmd);
    hide_window(&mut cmd);
    let display = format!("git {}", args.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" "));
    let out = cmd.output().map_err(|e| GitError {
        command: display.clone(),
        code: None,
        stderr: format!("Could not start git: {e}"),
        stdout: String::new(),
    })?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    log.push(CmdLogEntry {
        time: chrono::Local::now(),
        repo: String::from("-"),
        command: display.clone(),
        ok: out.status.success(),
        millis: started.elapsed().as_millis(),
        output: stderr.trim().to_string(),
    });
    if !out.status.success() {
        return Err(GitError {
            command: display,
            code: out.status.code(),
            stderr,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
