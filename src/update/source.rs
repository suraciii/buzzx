//! Main-only source updates. Git diagnostics stay private; Cargo logs use stderr.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Check,
    Plan,
    Apply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    UpToDate,
    UpdateAvailable,
    Updated,
    Blocked,
    Unknown,
    Failed,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub status: Status,
    pub install_kind: &'static str,
    pub source_path: Option<PathBuf>,
    pub branch: Option<String>,
    pub current_commit: Option<String>,
    pub target_commit: Option<String>,
    pub previous_commit: Option<String>,
    pub restart_required: bool,
    pub error: Option<&'static str>,
    pub message: String,
    pub behind: Option<u64>,
    pub commands: Vec<&'static str>,
    pub rollback_succeeded: Option<bool>,
    pub remote_refs_refreshed: bool,
}

impl Report {
    fn new() -> Self {
        Self {
            status: Status::Blocked,
            install_kind: "source",
            source_path: None,
            branch: None,
            current_commit: None,
            target_commit: None,
            previous_commit: None,
            restart_required: false,
            error: None,
            message: String::new(),
            behind: None,
            commands: Vec::new(),
            rollback_succeeded: None,
            remote_refs_refreshed: false,
        }
    }

    fn stop(mut self, status: Status, error: &'static str, message: &str) -> Self {
        self.status = status;
        self.error = Some(error);
        self.message = message.to_owned();
        self
    }

    pub fn exit_code(&self) -> i32 {
        match self.status {
            Status::UpToDate | Status::UpdateAvailable | Status::Updated => 0,
            Status::Blocked => 1,
            Status::Unknown => 2,
            Status::Failed => 4,
        }
    }
}

fn git_command(path: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    command
}

fn git(path: &Path, args: &[&str]) -> Result<String, ()> {
    let output = git_command(path)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| ())
}

fn checkout(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().find_map(|path| {
        if !path.join(".git").exists() {
            return None;
        }
        let text = fs::read_to_string(path.join("Cargo.toml")).ok()?;
        let manifest: toml::Value = toml::from_str(&text).ok()?;
        if manifest.get("package")?.get("name")?.as_str()? != "buzzx" {
            return None;
        }
        let root = git(path, &["rev-parse", "--show-toplevel"]).ok()?;
        let actual = fs::canonicalize(root).ok()?;
        (actual == fs::canonicalize(path).ok()?).then_some(actual)
    })
}

fn install_root(exe: &Path, checkout: &Path) -> Option<PathBuf> {
    let exe = fs::canonicalize(exe).ok()?;
    if exe.starts_with(checkout.join("target")) {
        return None;
    }
    let bin = exe.parent()?;
    if bin.file_name()? != "bin" || exe.file_name()? != "buzzx" {
        return None;
    }
    let root = bin.parent()?;
    let metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join(".crates2.json")).ok()?).ok()?;
    let installs = metadata.get("installs")?.as_object()?;
    installs
        .iter()
        .any(|(package, record)| {
            package.starts_with("buzzx ")
                && package.contains(" (path+")
                && record
                    .get("bins")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|bins| bins.iter().any(|name| name.as_str() == Some("buzzx")))
        })
        .then(|| root.to_owned())
}

fn fetch(path: &Path, timeout: Duration) -> Result<(), ()> {
    let mut child = git_command(path)
        .args([
            "fetch",
            "--no-tags",
            "origin",
            "main:refs/remotes/origin/main",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return if status.success() { Ok(()) } else { Err(()) },
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
        }
    }
}

fn inspect(path: &Path, report: &mut Report) -> Result<String, (&'static str, &'static str)> {
    let current = git(path, &["rev-parse", "HEAD"])
        .map_err(|_| ("head_unavailable", "cannot resolve checkout HEAD"))?;
    report.current_commit = Some(short(&current));
    report.branch = Some(
        git(path, &["branch", "--show-current"])
            .map_err(|_| ("branch_unavailable", "cannot resolve checkout branch"))?,
    );
    if report.branch.as_deref() != Some("main") {
        return Err((
            "not_main",
            "checkout must be on main; select main manually before updating",
        ));
    }
    let changes = git(path, &["status", "--porcelain", "--untracked-files=all"])
        .map_err(|_| ("worktree_unavailable", "cannot inspect checkout changes"))?;
    if !changes.is_empty() {
        return Err((
            "dirty_checkout",
            "checkout has changes; commit or stash them manually before updating",
        ));
    }
    Ok(current)
}

fn short(commit: &str) -> String {
    commit.chars().take(12).collect()
}

fn rollback(mut report: Report, path: &Path, previous: &str, reason: &'static str) -> Report {
    let restored = git(path, &["reset", "--keep", previous]).is_ok()
        && git(path, &["rev-parse", "HEAD"]).as_deref() == Ok(previous);
    report.rollback_succeeded = Some(restored);
    report.status = Status::Failed;
    report.restart_required = false;
    report.error = Some(reason);
    report.message = if restored {
        "installation or verification failed; checkout restored. Reinstall manually from this checkout: cargo install --locked --path . --force".to_owned()
    } else {
        format!(
            "installation or verification failed; checkout rollback failed. Preserve local changes, restore {previous} manually, then run cargo install --locked --path . --force"
        )
    };
    report
}

#[derive(Deserialize)]
struct BuildInfo {
    commit: Option<String>,
    install_kind: String,
    dirty: bool,
}

fn verify(exe: &Path, target: &str) -> bool {
    let Ok(output) = Command::new(exe)
        .arg("build-info")
        .stdin(Stdio::null())
        .output()
    else {
        return false;
    };
    output.status.success()
        && serde_json::from_slice::<BuildInfo>(&output.stdout).is_ok_and(|info| {
            info.commit.as_deref() == Some(short(target).as_str())
                && info.install_kind == "source"
                && !info.dirty
        })
}

pub fn run(action: Action, prerelease: bool) -> Report {
    let report = Report::new();
    if prerelease {
        return report.stop(
            Status::Blocked,
            "prerelease_not_applicable",
            "--prerelease applies only to prebuilt Release checks",
        );
    }
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        return report.stop(Status::Blocked, "unsupported_platform", "automatic source replacement requires Linux or macOS; close buzzx, fetch origin/main, fast-forward main and run cargo install --locked --path . --force manually");
    }
    let Some(path) = env::current_dir().ok().and_then(|cwd| checkout(&cwd)) else {
        return report.stop(Status::Blocked, "checkout_not_found", "run from a buzzx source checkout or its subdirectory; manual installation: cargo install --locked --path . --force");
    };
    let mut report = report;
    report.source_path = Some(path.clone());
    let previous = match inspect(&path, &mut report) {
        Ok(current) => current,
        Err((reason, message)) => return report.stop(Status::Blocked, reason, message),
    };
    let Some(root) = env::current_exe()
        .ok()
        .and_then(|exe| install_root(&exe, &path))
    else {
        return report.stop(Status::Blocked, "not_installed_source", "cannot confirm this executable as a Cargo-installed source binary; run cargo install --locked --path . --force first");
    };
    if fetch(&path, Duration::from_secs(30)).is_err() {
        return report.stop(Status::Unknown, "fetch_failed", "cannot refresh origin/main within 30 seconds; check origin and network access, then retry");
    }
    report.remote_refs_refreshed = true;
    let Ok(target) = git(&path, &["rev-parse", "refs/remotes/origin/main^{commit}"]) else {
        return report.stop(
            Status::Unknown,
            "target_missing",
            "origin/main is unavailable; check origin and its main branch",
        );
    };
    report.target_commit = Some(short(&target));
    if git(&path, &["merge-base", "--is-ancestor", &previous, &target]).is_err() {
        return report.stop(Status::Blocked, "history_diverged", "HEAD is not an ancestor of origin/main; inspect git log --left-right main...origin/main and resolve history manually");
    }
    report.behind = git(
        &path,
        &["rev-list", "--count", &format!("{previous}..{target}")],
    )
    .ok()
    .and_then(|count| count.parse().ok());
    if previous == target {
        report.status = Status::UpToDate;
        report.message = format!("main is up to date at {}", short(&target));
        return report;
    }
    report.commands = vec![
        "git merge --ff-only origin/main",
        "cargo install --locked --path . --force",
    ];
    if action != Action::Apply {
        report.status = Status::UpdateAvailable;
        report.restart_required = action == Action::Plan;
        report.message = format!(
            "update available from {} to {}; fetch refreshed remote refs only",
            short(&previous),
            short(&target)
        );
        return report;
    }
    report.previous_commit = Some(short(&previous));
    // Recheck before moving HEAD: fetch may have taken long enough for a local edit.
    match inspect(&path, &mut report) {
        Ok(current) if current == previous => {}
        _ => {
            return report.stop(
                Status::Blocked,
                "checkout_changed",
                "checkout changed during fetch; no update applied",
            );
        }
    }
    if git(&path, &["merge", "--ff-only", &target]).is_err() {
        return report.stop(
            Status::Unknown,
            "fast_forward_failed",
            "fast-forward failed; inspect the checkout before retrying",
        );
    }
    let installed = Command::new("cargo")
        .args(["install", "--locked", "--path", ".", "--force"])
        .current_dir(&path)
        .env("CARGO_INSTALL_ROOT", &root)
        .env("BUZZX_INSTALL_KIND", "source")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .stderr(Stdio::inherit())
        .status()
        .is_ok_and(|status| status.success());
    if !installed {
        return rollback(report, &path, &previous, "install_failed");
    }
    if !verify(&root.join("bin/buzzx"), &target) {
        return rollback(report, &path, &previous, "verification_failed");
    }
    report.status = Status::Updated;
    report.restart_required = true;
    report.message = format!(
        "updated from {} to {}; restart buzzx to use the new binary",
        short(&previous),
        short(&target)
    );
    report
}

#[cfg(test)]
mod tests;
