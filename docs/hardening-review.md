# Hardening and profiling review — 2026-11-13

This pass reviewed the existing uncommitted work, extended it, and verified the resulting working tree. No commits,
pushes, installations, or modifications to an artist's project were performed. Workflow tests and profiles use
temporary repositories with a local bare server and two clones.

## Findings addressed

| Priority | Failure scenario | Resulting safeguard |
| --- | --- | --- |
| High | Revert uses stale status after another editor commits a formerly new file | Status is read again when the queued operation executes and again before the backend reverts files. A now-clean tracked file is not deleted. |
| High | A staged version differs from the working file and is overwritten by submit/revert | The staged tree is retained in an automatic shelf before replacing selected staged changes. Working-copy backups remain separate. |
| High | Another Git client stages unrelated files after the selection check | Submit always uses Git's `commit --only` transaction. The plain-commit fast path was removed. Missing paths are filtered without broadening the selection. |
| High | Submit stages conflict markers and implicitly completes a merge | The backend rejects ordinary submission during an operation or unresolved conflict; use Resolve and Continue. |
| High | Restore overwrites a generated or ignored local asset invisible to status | Restore and Unshelve also check ignored paths and back them up. Rename source paths are included in recovery snapshots. |
| High | Choosing a conflict side or Abort destroys manual resolution work | Current working files are shelved first. Resolution checks fresh conflict/operation state. |
| High | An unreadable changelist file is replaced with an empty store | Read failures disable saving and surface an error. Corrupt-file recovery uses non-overwriting backup creation with no 100-backup cutoff. |
| High | Offline or stale lock information allows an upload attempt | Locks are checked again before submission of lockable files. Failure offers an explicit local-commit path. Attribute detection includes nested and repository-local attributes. |
| Medium | Changelist entries disappear while locks are unknown or during integration | Retain relevant membership until the tree and locks are known; skip pruning during operations. Failed lock refreshes restore the unknown state. |
| Medium | A shelf ref changes between listing and deletion | Ref deletion checks the expected object ID. Remote shelves with the same name but different contents remain visible. |
| Medium | An artist closes a tab/app while a local operation is queued | Closing is blocked while the repository worker has pending work. |
| Medium | A background job panics, silently losing its result | The worker reports the failed job, clears loading state, and remains usable for subsequent jobs. |
| Medium | A text diff or highly compressed image exhausts memory | Text output is drained with a 4 MiB retained preview cap; images have decode allocation and dimension limits and are reduced for display. |
| Medium | Direct file deletion traverses a linked parent directory | Rust deletion paths reject invalid components and linked parent folders; broken symlinks themselves can still be removed. |
| Medium | An install path containing `%...%` is expanded during self-uninstall | The cleanup helper receives paths through environment values and uses literal, nonrecursive filesystem operations. |

The pre-existing changes also provide cached/virtualized Pending rows, virtualized dialog checklists, batched
staging through `update-index --stdin`, bounded parallel LFS lock requests, adaptive status polling, submit-draft
retention, collision-safe shelf creation, and backup-before-restoring branch work. This pass preserves them.
Commit and shelf detail lists now also virtualize their rows and share their data instead of cloning entire lists
every frame. Shelf duplicate detection uses hash sets instead of repeated linear scans.

## Verification

- Full application suite: 28 unit tests and 39 real-Git workflow tests.
- Strict lint check: `cargo clippy --all-targets -- -D warnings`.
- Formatting and whitespace: `cargo fmt --all -- --check`, `git diff --check`.
- Installer: `cargo check --manifest-path installer/Cargo.toml` (compile check; no install/uninstall executed).
- Release profiling: `cargo test --release --test perf -- --ignored --nocapture --test-threads=1`.

Git for Windows subprocesses require execution outside this environment's sandbox: the sandbox denies MSYS
named-object creation. The tests succeed with that restriction removed. Compiler hard-link-cache warnings are
environmental, not Rust or Clippy diagnostics.

## Profile

Windows, release build, 5,000 modified files plus 1,250 new files, spread across directories. These are synthetic
small text files, not representative multi-gigabyte PSD/EXR/video content. Timings are observations on this machine,
not performance guarantees. Filesystem cache state matters substantially.

| Backend operation | Elapsed |
| --- | ---: |
| Status | 85 ms |
| First shelf, new contents not yet stored | 26.28 s |
| Repeat shelf, objects already stored | 2.54 s |
| Revert | 2.48 s |
| Unshelve | 2.82 s |
| Commit selected paths | 7.95 s |

The first shelf spent 23.80 seconds in `update-index` and 2.19 seconds in `write-tree`; the repeat shelf spent
1.97 seconds and 0.32 seconds respectively. This shows why a cached staging benchmark must not be presented as a
cold-shelf speedup. The first snapshot remains limited by Git/object-storage work. It runs in the background.
Submit deliberately retains `--only` despite its cost to enforce the selected-file boundary inside Git.

| UI operation | Mean time |
| --- | ---: |
| Pending frame | 0.32 ms |
| Pending frame, all selected | 0.14 ms |
| Submit dialog frame | 0.36 ms |
| Revert dialog frame | 0.45 ms |
| History frame | 0.10 ms |
| History frame with 5,000-file commit selected | 0.10 ms |
| UI-thread time to queue Revert | 0.49 ms |

The earlier user-supplied baseline reported 25–100 ms per Pending frame. That baseline was not rerun against a
clean checkout, so it is contextual rather than a controlled before/after comparison. Both profiling tests passed,
including the completed revert's clean-worktree assertion. All 67 ordinary tests passed.

## Remaining limits and operational guidance

- This review cannot establish zero risk. Backups live in the same repository and on the same disk; they do not
  replace independent backups against disk loss, filesystem corruption, or deletion of the entire repository.
- Git operations and backups span multiple steps. Another editor saving a file during a backup-and-restore sequence
  can race the snapshot. Avoid simultaneously saving or running another Git operation while replacing files.
- Operations are serialized per open g4v tab, not across all independent g4v/Git processes. Metadata writes are
  atomic, but simultaneous application instances can still replace each other's changelist organization.
- Network tools, credentials, hooks and filters can stall. There is no general cancellation/timeout mechanism;
  forcibly terminating a mutation may leave Git recovery state. The UI stays responsive, but later operations on
  that repository wait in its queue.
- Submodule file contents are not included in a superproject's shelves; work inside a submodule must be protected
  from its own tab. LFS backup content depends on its local LFS objects remaining available.
- Automatic backups follow the configured retention period. Zero or negative retention values now mean keep
  forever. Restore important work before expiry or keep a separate named shelf.
- Live authenticated LFS hosting, real credential-provider windows, GPU rendering, very large binary workflows,
  power-loss fault injection, and full installer execution were not exercised. Headless UI frames measure layout
  work, not end-to-end display latency. Native Windows was tested; other operating systems still need validation.
