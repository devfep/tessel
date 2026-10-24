//! Paths of the configuration, made absolute when the lander is set up. A relative path means
//! relative to the directory the server was started in; the commands the lander runs start in
//! another directory, and the start directory can be removed while the server runs.

use std::io;
use std::path::{Component, Path, PathBuf};

use super::LandingError;

/// `path` against the working directory, with `.` and `..` resolved by name: the result must not
/// need the working directory to exist when it is used.
pub(super) fn absolute(path: &Path) -> Result<PathBuf, LandingError> {
    let fail = |source| LandingError::Resolve {
        path: path.display().to_string(),
        source,
    };
    let full = std::path::absolute(path).map_err(fail)?;
    let mut resolved = PathBuf::new();
    for component in full.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                resolved.push(component);
            }
        }
    }
    Ok(resolved)
}

/// `remote` or `fork` as git is given it: a URL or an `host:path` address as it is, a path made
/// absolute.
pub(super) fn absolute_location(location: &str) -> Result<String, LandingError> {
    let host_path = match (location.find(':'), location.find('/')) {
        (Some(colon), Some(slash)) => colon < slash,
        (Some(_), None) => true,
        (None, _) => false,
    };
    if host_path || location.contains("://") {
        return Ok(location.to_owned());
    }
    absolute(Path::new(location))?
        .into_os_string()
        .into_string()
        .map_err(|_| LandingError::Resolve {
            path: location.to_owned(),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "the absolute path is not valid UTF-8",
            ),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_path_is_resolved_by_name_against_the_working_directory() {
        let here = std::env::current_dir().unwrap();
        assert_eq!(absolute(Path::new("a/../b/./c")).unwrap(), here.join("b/c"));
    }

    #[test]
    fn an_absolute_path_is_kept() {
        assert_eq!(
            absolute(Path::new("/srv/x/../forks")).unwrap(),
            Path::new("/srv/forks")
        );
    }

    #[test]
    fn urls_and_host_addresses_are_kept_and_paths_are_made_absolute() {
        for kept in [
            "https://example.invalid/o/r.git",
            "file:///srv/r.git",
            "git@example.invalid:o/r.git",
            "host:r.git",
        ] {
            assert_eq!(absolute_location(kept).unwrap(), kept);
        }
        let here = std::env::current_dir().unwrap();
        assert_eq!(
            absolute_location("forks/{agent}.git").unwrap(),
            here.join("forks/{agent}.git").display().to_string()
        );
        assert_eq!(absolute_location("/srv/r.git").unwrap(), "/srv/r.git");
    }
}
