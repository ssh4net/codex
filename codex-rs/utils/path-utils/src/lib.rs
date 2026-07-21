//! Path normalization, symlink resolution, and atomic writes shared across Codex crates.

pub(crate) mod env;
pub use env::is_wsl;

use codex_utils_absolute_path::AbsolutePathBuf;
use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use tempfile::NamedTempFile;

pub fn normalize_for_path_comparison(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let canonical = path.as_ref().canonicalize()?;
    Ok(normalize_for_wsl(canonical))
}

/// Compare paths after applying Codex's filesystem normalization.
///
/// If either path cannot be normalized, this falls back to direct path equality.
pub fn paths_match_after_normalization(left: impl AsRef<Path>, right: impl AsRef<Path>) -> bool {
    if let (Ok(left), Ok(right)) = (
        normalize_for_path_comparison(left.as_ref()),
        normalize_for_path_comparison(right.as_ref()),
    ) {
        return left == right;
    }
    left.as_ref() == right.as_ref()
}

/// Returns whether two WSL-mounted Windows paths differ only by ASCII case.
///
/// Unlike [`paths_match_after_normalization`], this does not canonicalize either
/// path, so symlink aliases remain distinct.
pub fn wsl_paths_match_ignoring_case(left: impl AsRef<Path>, right: impl AsRef<Path>) -> bool {
    wsl_paths_match_ignoring_case_with_flag(left.as_ref(), right.as_ref(), env::is_wsl())
}

/// Restore the case-preserving filesystem spelling of a WSL-mounted Windows path.
///
/// This walks directory entries without canonicalizing, so symlink spellings
/// remain intact. The original path is returned outside WSL or on lookup failure.
pub fn restore_wsl_path_spelling(path: PathBuf) -> PathBuf {
    restore_wsl_path_spelling_with_flag(&path, env::is_wsl()).unwrap_or(path)
}

pub fn normalize_for_native_workdir(path: impl AsRef<Path>) -> PathBuf {
    normalize_for_native_workdir_with_flag(path.as_ref().to_path_buf(), cfg!(windows))
}

pub struct SymlinkWritePaths {
    pub read_path: Option<PathBuf>,
    pub write_path: PathBuf,
}

/// Resolve the final filesystem target for `path` while retaining a safe write path.
///
/// This follows symlink chains (including relative symlink targets) until it reaches a
/// non-symlink path. If the chain cycles or any metadata/link resolution fails, it
/// returns `read_path: None` and uses the original absolute path as `write_path`.
/// There is no fixed max-resolution count; cycles are detected via a visited set.
pub fn resolve_symlink_write_paths(path: &Path) -> io::Result<SymlinkWritePaths> {
    let root = AbsolutePathBuf::from_absolute_path(path)
        .map(AbsolutePathBuf::into_path_buf)
        .unwrap_or_else(|_| path.to_path_buf());
    let mut current = root.clone();
    let mut visited = HashSet::new();

    // Follow symlink chains while guarding against cycles.
    loop {
        let meta = match std::fs::symlink_metadata(&current) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(SymlinkWritePaths {
                    read_path: Some(current.clone()),
                    write_path: current,
                });
            }
            Err(_) => {
                return Ok(SymlinkWritePaths {
                    read_path: None,
                    write_path: root,
                });
            }
        };

        if !meta.file_type().is_symlink() {
            return Ok(SymlinkWritePaths {
                read_path: Some(current.clone()),
                write_path: current,
            });
        }

        // If we've already seen this path, the chain cycles.
        if !visited.insert(current.clone()) {
            return Ok(SymlinkWritePaths {
                read_path: None,
                write_path: root,
            });
        }

        let target = match std::fs::read_link(&current) {
            Ok(target) => target,
            Err(_) => {
                return Ok(SymlinkWritePaths {
                    read_path: None,
                    write_path: root,
                });
            }
        };

        let next = if target.is_absolute() {
            AbsolutePathBuf::from_absolute_path(&target)
        } else if let Some(parent) = current.parent() {
            Ok(AbsolutePathBuf::resolve_path_against_base(&target, parent))
        } else {
            return Ok(SymlinkWritePaths {
                read_path: None,
                write_path: root,
            });
        };

        let next = match next {
            Ok(path) => path.into_path_buf(),
            Err(_) => {
                return Ok(SymlinkWritePaths {
                    read_path: None,
                    write_path: root,
                });
            }
        };

        current = next;
    }
}

pub fn write_atomically(write_path: &Path, contents: &str) -> io::Result<()> {
    let parent = write_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path {} has no parent directory", write_path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let tmp = NamedTempFile::new_in(parent)?;
    std::fs::write(tmp.path(), contents)?;
    tmp.persist(write_path)?;
    Ok(())
}

fn normalize_for_wsl(path: PathBuf) -> PathBuf {
    normalize_for_wsl_with_flag(path, env::is_wsl())
}

fn normalize_for_native_workdir_with_flag(path: PathBuf, is_windows: bool) -> PathBuf {
    if is_windows {
        dunce::simplified(&path).to_path_buf()
    } else {
        path
    }
}

fn normalize_for_wsl_with_flag(path: PathBuf, is_wsl: bool) -> PathBuf {
    if !is_wsl {
        return path;
    }

    if !is_wsl_case_insensitive_path(&path) {
        return path;
    }

    lower_ascii_path(path)
}

fn wsl_paths_match_ignoring_case_with_flag(left: &Path, right: &Path, is_wsl: bool) -> bool {
    if !is_wsl || !is_wsl_case_insensitive_path(left) || !is_wsl_case_insensitive_path(right) {
        return false;
    }

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;

        ascii_eq_ignore_case(left.as_os_str().as_bytes(), right.as_os_str().as_bytes())
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn restore_wsl_path_spelling_with_flag(path: &Path, is_wsl: bool) -> Option<PathBuf> {
    if !is_wsl || !is_wsl_case_insensitive_path(path) {
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::path::Component;

        let mut restored = PathBuf::new();
        for component in path.components() {
            match component {
                Component::RootDir => restored.push(component.as_os_str()),
                Component::Normal(requested) => {
                    let mut matched = None;
                    for entry in std::fs::read_dir(&restored).ok()? {
                        let file_name = entry.ok()?.file_name();
                        if file_name == requested {
                            matched = Some(file_name);
                            break;
                        }
                        if ascii_eq_ignore_case(file_name.as_bytes(), requested.as_bytes()) {
                            if matched.is_some() {
                                return None;
                            }
                            matched = Some(file_name);
                        }
                    }
                    restored.push(matched?);
                }
                Component::CurDir | Component::ParentDir | Component::Prefix(_) => return None,
            }
        }
        Some(restored)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn is_wsl_case_insensitive_path(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::path::Component;

        let mut components = path.components();
        let Some(Component::RootDir) = components.next() else {
            return false;
        };
        let Some(Component::Normal(mnt)) = components.next() else {
            return false;
        };
        if !ascii_eq_ignore_case(mnt.as_bytes(), b"mnt") {
            return false;
        }
        let Some(Component::Normal(drive)) = components.next() else {
            return false;
        };
        let drive_bytes = drive.as_bytes();
        drive_bytes.len() == 1 && drive_bytes[0].is_ascii_alphabetic()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

#[cfg(target_os = "linux")]
fn ascii_eq_ignore_case(left: &[u8], right: &[u8]) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(target_os = "linux")]
fn lower_ascii_path(path: PathBuf) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::ffi::OsStringExt;

    // WSL mounts Windows drives under /mnt/<drive>, which are case-insensitive.
    let bytes = path.as_os_str().as_bytes();
    let mut lowered = Vec::with_capacity(bytes.len());
    for byte in bytes {
        lowered.push(byte.to_ascii_lowercase());
    }
    PathBuf::from(OsString::from_vec(lowered))
}

#[cfg(not(target_os = "linux"))]
fn lower_ascii_path(path: PathBuf) -> PathBuf {
    path
}

#[cfg(test)]
#[path = "path_utils_tests.rs"]
mod tests;
