//! File protection, as the running platform provides it. The contract is
//! docs/configuration.md: a file that holds a private key is readable by its
//! owner alone.
//!
//! Unix states that with mode bits and can be asked the question. Windows has
//! no mode bits: the protection is the ACL a file inherits from the user
//! profile, so a path outside the profile cannot promise owner-only access and
//! is reported instead of refused. Nothing here reads the file or chooses an
//! exit code; the caller does. The refusal message is the platform's own
//! answer, because only the platform knows the remedy.

use std::fs::OpenOptions;
use std::io;
use std::path::Path;

/// Whether a caller may read a path that holds a private key. `Err` carries
/// the refusal, which names the platform's remedy. A platform that cannot
/// promise owner-only access says so on stderr and returns `Ok`: the user
/// chose the path, and refusing it would make the tool unusable there.
/// `label` names the file in the message, for example "config" or "key file".
pub(crate) fn check_secret(path: &Path, label: &str) -> Result<(), String> {
    imp::check_secret(path, label)
}

/// Restrict a directory this process created to its owner.
pub(crate) fn restrict_dir(path: &Path) -> io::Result<()> {
    imp::restrict_dir(path)
}

/// Restrict a file this process created to its owner.
pub(crate) fn restrict_file(path: &Path) -> io::Result<()> {
    imp::restrict_file(path)
}

/// Set the creation mode on options for a file that holds a private key.
pub(crate) fn secret_create(options: &mut OpenOptions) {
    imp::secret_create(options)
}

/// The mode a file this process created carries, for the login success line.
/// `None` where the platform has no mode to report.
pub(crate) fn mode_note(path: &Path) -> Option<String> {
    imp::mode_note(path)
}

#[cfg(unix)]
mod imp {
    use std::fs::{self, OpenOptions};
    use std::io;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::Path;

    pub(super) fn check_secret(path: &Path, label: &str) -> Result<(), String> {
        let mode = match fs::metadata(path) {
            Ok(meta) => meta.permissions().mode(),
            // A path that cannot be examined is not a verdict: the read that
            // follows fails with a message about the path, not about access.
            Err(_) => return Ok(()),
        };
        if mode & 0o077 == 0 {
            return Ok(());
        }
        Err(format!(
            "{label} {} is readable by other users; run chmod 600 on it",
            path.display()
        ))
    }

    pub(super) fn restrict_dir(path: &Path) -> io::Result<()> {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }

    pub(super) fn restrict_file(path: &Path) -> io::Result<()> {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }

    pub(super) fn secret_create(options: &mut OpenOptions) {
        options.mode(0o600);
    }

    pub(super) fn mode_note(path: &Path) -> Option<String> {
        // The caller asks about a file this process just created, so the
        // create mode is the answer when the metadata cannot be read back.
        let mode = fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o777)
            .unwrap_or(0o600);
        Some(format!("{mode:o}"))
    }
}

#[cfg(windows)]
mod imp {
    use std::fs::OpenOptions;
    use std::io;
    use std::path::{Component, Path, PathBuf};

    // Windows has no mode bits to set. A file under the user profile inherits
    // that profile's ACL, which is the protection Unix mode 0600 states: other
    // standard users cannot read it, an administrator can, and the path stays
    // a plain readable file the user can edit and move between machines.

    pub(super) fn check_secret(path: &Path, label: &str) -> Result<(), String> {
        match dirs::home_dir() {
            Some(profile) if is_inside(path, &profile) => {}
            Some(profile) => eprintln!("buzzx: warning: {}", outside_note(path, label, &profile)),
            None => eprintln!(
                "buzzx: warning: {label} {}: the user profile cannot be resolved, so \
                 whether other users can read it cannot be confirmed",
                path.display()
            ),
        }
        Ok(())
    }

    /// The warning for a secret-bearing path that does not inherit the user
    /// profile's ACL.
    pub(super) fn outside_note(path: &Path, label: &str, profile: &Path) -> String {
        format!(
            "{label} {} is outside {}; other users may be able to read it",
            path.display(),
            profile.display()
        )
    }

    pub(super) fn restrict_dir(_path: &Path) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn restrict_file(_path: &Path) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn secret_create(_options: &mut OpenOptions) {}

    pub(super) fn mode_note(_path: &Path) -> Option<String> {
        None
    }

    /// Whether `path` sits under `root`. Both are made absolute and resolved
    /// lexically, so the answer holds for a path that does not exist yet and
    /// does not depend on how the path was spelled. Components are compared
    /// one at a time and without case, the way NTFS does, so `C:\Users\ann`
    /// does not claim `C:\Users\anna`.
    pub(super) fn is_inside(path: &Path, root: &Path) -> bool {
        let (Some(path), Some(root)) = (normalized(path), normalized(root)) else {
            return false;
        };
        let mut rest = path.components();
        root.components().all(|part| {
            rest.next()
                .is_some_and(|step| step.as_os_str().eq_ignore_ascii_case(part.as_os_str()))
        })
    }

    /// An absolute path with its verbatim prefix removed and its `.` and `..`
    /// components resolved without touching the filesystem. A path that
    /// cannot be spelled as a location - a device path, for example - is not
    /// a path this comparison can judge.
    fn normalized(path: &Path) -> Option<PathBuf> {
        let text = absolute(path).ok()?.to_string_lossy().into_owned();
        let stripped = match text.strip_prefix(r"\\?\UNC\") {
            Some(rest) => format!(r"\\{rest}"),
            None => text
                .strip_prefix(r"\\?\")
                .map(str::to_owned)
                .unwrap_or(text),
        };
        let mut resolved = PathBuf::new();
        for part in Path::new(&stripped).components() {
            match part {
                // A `..` above the root goes nowhere, the way the path
                // resolver treats it.
                Component::ParentDir => {
                    resolved.pop();
                }
                Component::CurDir => {}
                other => resolved.push(other),
            }
        }
        Some(resolved)
    }

    fn absolute(path: &Path) -> io::Result<PathBuf> {
        std::path::absolute(path)
    }
}

/// No other platform is a target for this client. A platform without mode
/// bits and without the user profile needs its own answer for what protects a
/// private key; guessing one here would hide that decision.
#[cfg(not(any(unix, windows)))]
compile_error!(
    "buzzx targets Unix and Windows; this platform needs its own file-protection answer"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_shared_file_is_refused_until_it_is_restricted() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("buzzx-platform-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        fs::write(&path, "relay_url = \"http://localhost:3000\"\n").expect("write");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        let err = check_secret(&path, "config").expect_err("a shared file is refused");
        assert!(err.contains("chmod 600"), "{err}");
        assert_eq!(mode_note(&path).as_deref(), Some("644"));

        restrict_file(&path).expect("restrict the file");
        assert!(check_secret(&path, "config").is_ok());
        assert_eq!(mode_note(&path).as_deref(), Some("600"));

        restrict_dir(&dir).expect("restrict the directory");
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700,
            "the directory this tool created is the user's alone"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn secret_create_starts_a_new_file_at_six_hundred() {
        use std::fs;
        use std::io::Write as _;

        let dir = std::env::temp_dir().join(format!("buzzx-platform-new-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");

        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        secret_create(&mut options);
        let mut file = options.open(&path).expect("create");
        file.write_all(b"relay_url = \"http://localhost:3000\"\n")
            .expect("write");
        drop(file);

        assert_eq!(mode_note(&path).as_deref(), Some("600"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_path_is_left_to_the_callers_own_read() {
        assert!(check_secret(Path::new("/nonexistent/buzzx-platform-probe"), "config").is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn a_profile_path_holds_and_a_sibling_directory_does_not() {
        use super::imp::is_inside;

        let root = Path::new(r"C:\Users\ann");
        assert!(is_inside(
            Path::new(r"C:\Users\ann\AppData\Roaming\buzzx\config.toml"),
            root
        ));
        assert!(
            is_inside(Path::new(r"C:\Users\ann"), root),
            "the profile itself"
        );
        assert!(
            is_inside(Path::new(r"c:\users\ANN\config.toml"), root),
            "NTFS compares without case"
        );
        assert!(
            !is_inside(Path::new(r"C:\Users\anna\config.toml"), root),
            "a name that only starts the same is not inside"
        );
        assert!(!is_inside(Path::new(r"C:\Users\ann.bak\config.toml"), root));
        assert!(!is_inside(Path::new(r"D:\buzzx\config.toml"), root));
        assert!(
            is_inside(
                Path::new(r"\\?\C:\Users\ann\AppData\Roaming\buzzx\config.toml"),
                root
            ),
            "a verbatim path names the same location"
        );
        assert!(
            is_inside(Path::new(r"C:\Users\ann\.\config.toml"), root),
            "a `.` component is not another location"
        );
        assert!(
            !is_inside(Path::new(r"C:\Users\ann\..\bob\config.toml"), root),
            "a path that climbs out of the profile is not inside it"
        );
        assert!(
            !is_inside(Path::new(r"C:\Users\ann\..\..\shared\config.toml"), root),
            "climbing past the profile root does not come back inside"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_profile_config_passes_and_an_outside_one_names_the_profile() {
        use super::imp::outside_note;

        let profile = dirs::home_dir().expect("a user profile");
        assert!(
            check_secret(&profile.join("buzzx-guard-probe.toml"), "config").is_ok(),
            "the default location inherits the profile ACL"
        );
        assert!(
            check_secret(Path::new(r"C:\buzzx-guard-probe.toml"), "config").is_ok(),
            "a path outside the profile is reported, not refused"
        );

        let note = outside_note(Path::new(r"C:\buzzx-guard-probe.toml"), "config", &profile);
        assert!(
            note.contains(&profile.display().to_string()),
            "the warning names the profile: {note}"
        );
    }
}
