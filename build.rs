//! Freezes the build identity inputs into OUT_DIR/version_metadata.rs for
//! src/version.rs. The derivation rules live in that module, where they are
//! unit tested; this script only gathers facts. No timestamps are recorded:
//! one commit must always produce one identity.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let pkg_version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is set by Cargo");
    let target = env::var("TARGET").expect("TARGET is set by Cargo");
    let install_kind = install_kind_override();
    let git = GitFacts::probe(&pkg_version);
    watch(&git);

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"))
        .join("version_metadata.rs");
    fs::write(&out, generated(&pkg_version, &target, &install_kind, &git))
        .expect("write version_metadata.rs");
}

/// The Git facts of the checkout. Each is independently absent when Git or
/// the repository is not there; the module turns absence into the
/// `0.0.0-dev.unknown` identity.
struct GitFacts {
    /// The tag that is exactly `v{package version}` and points at HEAD.
    tag: Option<String>,
    /// The checked-out branch; absent when HEAD is detached.
    branch: Option<String>,
    /// The first 12 characters of the HEAD object id.
    commit: Option<String>,
    /// Whether the checkout differs from HEAD, including untracked files:
    /// a source tree that is not reproducible must say so.
    dirty: bool,
    /// The per-worktree Git directory and the shared directory, used for the
    /// rebuild watches.
    git_dir: Option<PathBuf>,
    common_dir: Option<PathBuf>,
}

impl GitFacts {
    fn probe(pkg_version: &str) -> Self {
        let git_dir = git(&["rev-parse", "--absolute-git-dir"]).map(PathBuf::from);
        let common_dir = git(&["rev-parse", "--git-common-dir"]).map(|dir| {
            let path = PathBuf::from(&dir);
            if path.is_absolute() {
                path
            } else {
                env::current_dir().expect("current dir").join(path)
            }
        });
        let tag = git(&["tag", "--points-at", "HEAD"]).and_then(|tags| {
            tags.lines()
                .find(|tag| *tag == format!("v{pkg_version}"))
                .map(str::to_owned)
        });
        let branch = git(&["branch", "--show-current"])
            .and_then(|branch| (!branch.is_empty()).then_some(branch));
        let commit =
            git(&["rev-parse", "HEAD"]).map(|head| head.get(..12).unwrap_or(&head).to_owned());
        let dirty = git(&["status", "--porcelain", "--untracked-files=all"])
            .is_some_and(|status| !status.is_empty());
        Self {
            tag,
            branch,
            commit,
            dirty,
            git_dir,
            common_dir,
        }
    }
}

/// GIT_OPTIONAL_LOCKS keeps every probe read-only, so watching the Git
/// directories below can never make a build script rerun loop.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The install kind defaults to `source`; release automation sets
/// `BUZZX_INSTALL_KIND=prebuilt`. An unrecognized value fails the build
/// instead of shipping a binary that lies about its provenance.
fn install_kind_override() -> String {
    match env::var("BUZZX_INSTALL_KIND") {
        Ok(value) if value == "source" || value == "prebuilt" => value,
        Ok(value) => panic!("BUZZX_INSTALL_KIND must be `source` or `prebuilt`, got `{value}`"),
        Err(env::VarError::NotPresent) => "source".to_owned(),
        Err(_) => panic!("BUZZX_INSTALL_KIND must be valid Unicode"),
    }
}

/// Rebuild when an identity input changes: the build script itself, the
/// package version, HEAD, the refs that move with a commit, tag, or checkout,
/// and the tracked sources whose edits flip the dirty flag. Untracked source
/// files under `src` are covered by the directory watch; other untracked
/// files do not affect the compiled binary.
fn watch(git: &GitFacts) {
    changed("build.rs");
    changed("Cargo.toml");
    changed("src");
    println!("cargo:rerun-if-env-changed=BUZZX_INSTALL_KIND");
    let Some(git_dir) = &git.git_dir else {
        return;
    };
    changed(git_dir.join("HEAD"));
    if let Some(common_dir) = &git.common_dir {
        changed(common_dir.join("refs"));
        changed(common_dir.join("packed-refs"));
    }
}

fn changed(path: impl AsRef<Path>) {
    let path = path.as_ref();
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn generated(pkg_version: &str, target: &str, install_kind: &str, git: &GitFacts) -> String {
    let GitFacts {
        tag,
        branch,
        commit,
        dirty,
        ..
    } = git;
    format!(
        "// Generated by build.rs from Cargo.toml and the Git checkout. Do not edit.\n\
         \n\
         const PKG_VERSION: &str = {pkg_version:?};\n\
         const GIT_TAG: Option<&str> = {};\n\
         const GIT_BRANCH: Option<&str> = {};\n\
         const GIT_COMMIT: Option<&str> = {};\n\
         const GIT_DIRTY: bool = {dirty};\n\
         const INSTALL_KIND: &str = {install_kind:?};\n\
         const TARGET: &str = {target:?};\n",
        opt(tag),
        opt(branch),
        opt(commit),
    )
}

/// `{:?}` quoting produces a valid Rust string literal for any tag or branch
/// name Git allows.
fn opt(value: &Option<String>) -> String {
    match value {
        Some(value) => format!("Some({value:?})"),
        None => "None".to_owned(),
    }
}
