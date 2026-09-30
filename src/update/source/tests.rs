use super::*;

struct Repo {
    path: PathBuf,
}
impl Repo {
    fn new() -> Self {
        let path = env::temp_dir().join(format!("buzzx-update-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        let repo = Self { path };
        repo.command(&["init", "-b", "main"]);
        repo.command(&["config", "user.name", "Update Test"]);
        repo.command(&["config", "user.email", "update@example.invalid"]);
        fs::write(
            repo.path.join("Cargo.toml"),
            "[package]\nname='buzzx'\nversion='0.1.0'\n",
        )
        .unwrap();
        repo.commit("initial");
        repo
    }
    fn command(&self, args: &[&str]) -> String {
        git(&self.path, args).unwrap()
    }
    fn commit(&self, message: &str) {
        self.command(&["add", "."]);
        let output = git_command(&self.path)
            .args(["commit", "-m", message])
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .output()
            .unwrap();
        assert!(output.status.success());
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn structured_manifest_and_nested_checkout_resolve_without_matching_dependency_names() {
    let repo = Repo::new();
    let sub = repo.path.join("nested/sub");
    fs::create_dir_all(&sub).unwrap();
    assert_eq!(checkout(&sub), Some(repo.path.clone()));
    fs::write(
        repo.path.join("Cargo.toml"),
        "[package]\nname='other'\n[dependencies]\nbuzzx={version='1'}\n",
    )
    .unwrap();
    assert_eq!(checkout(&sub), None);
}

#[test]
fn dirty_and_detached_checkouts_preserve_local_work_and_head() {
    let repo = Repo::new();
    let head = repo.command(&["rev-parse", "HEAD"]);
    fs::write(repo.path.join("local"), "keep this").unwrap();
    let mut report = Report::new();
    assert_eq!(
        inspect(&repo.path, &mut report).unwrap_err().0,
        "dirty_checkout"
    );
    assert_eq!(
        fs::read_to_string(repo.path.join("local")).unwrap(),
        "keep this"
    );
    assert_eq!(repo.command(&["rev-parse", "HEAD"]), head);
    repo.command(&["checkout", "--detach"]);
    assert_eq!(inspect(&repo.path, &mut report).unwrap_err().0, "not_main");
    assert_eq!(repo.command(&["rev-parse", "HEAD"]), head);
}

#[test]
fn rollback_restores_fast_forward_but_preserves_conflicting_local_edits() {
    let repo = Repo::new();
    let previous = repo.command(&["rev-parse", "HEAD"]);
    fs::write(
        repo.path.join("Cargo.toml"),
        "[package]\nname='buzzx'\nversion='0.2.0'\n",
    )
    .unwrap();
    repo.commit("new version");
    let target = repo.command(&["rev-parse", "HEAD"]);
    let report = rollback(Report::new(), &repo.path, &previous, "install_failed");
    assert_eq!(report.status, Status::Failed);
    assert_eq!(report.rollback_succeeded, Some(true));
    assert_eq!(repo.command(&["rev-parse", "HEAD"]), previous);
    repo.command(&["merge", "--ff-only", &target]);
    fs::write(repo.path.join("Cargo.toml"), "local changes").unwrap();
    let report = rollback(Report::new(), &repo.path, &previous, "verification_failed");
    assert_eq!(report.rollback_succeeded, Some(false));
    assert_eq!(repo.command(&["rev-parse", "HEAD"]), target);
    assert_eq!(
        fs::read_to_string(repo.path.join("Cargo.toml")).unwrap(),
        "local changes"
    );
}

#[test]
fn install_record_requires_path_package_and_matching_binary() {
    let repo = Repo::new();
    let root = repo.path.join("install");
    fs::create_dir_all(root.join("bin")).unwrap();
    let exe = root.join("bin/buzzx");
    fs::write(&exe, "binary").unwrap();
    let package = format!(
        "buzzx 0.1.0 (path+{})",
        reqwest::Url::from_file_path(&repo.path).unwrap()
    );
    let record = |package: &str, bins: &[&str]| {
        serde_json::json!({"installs": {package: {"bins":bins}}}).to_string()
    };
    fs::write(
        root.join(".crates2.json"),
        record("buzzx 0.1.0 (registry+url)", &["buzzx"]),
    )
    .unwrap();
    assert_eq!(install_root(&exe, &repo.path), None);
    fs::write(root.join(".crates2.json"), record(&package, &["another"])).unwrap();
    assert_eq!(install_root(&exe, &repo.path), None);
    fs::write(
        root.join(".crates2.json"),
        record("buzzx 0.1.0 (path+file:///source)", &["buzzx"]),
    )
    .unwrap();
    assert_eq!(install_root(&exe, &repo.path), None);
    fs::write(root.join(".crates2.json"), record(&package, &["buzzx"])).unwrap();
    assert_eq!(install_root(&exe, &repo.path), Some(root));
}
