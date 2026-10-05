//! Parsing of scope arguments (`dir/`, `path/file.rs`, `path/file.rs::qualified::name`) into
//! protocol scopes, and conversion of absolute file paths into repo-relative ones.

use std::path::{Component, Path, PathBuf};

use tessel_coordinator::protocol::{Scope, SymbolId};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ScopeError {
    #[error(
        "scope {scope:?} is not a canonical repo-relative path: use no leading or trailing '/' \
         (except one trailing '/' to name a directory), no '.' or '..' segment, no empty \
         segment and no backslash"
    )]
    NotCanonical { scope: String },
    #[error("scope {scope:?} names a symbol without a path or without a name")]
    BadSymbol { scope: String },
    #[error("scope is empty")]
    Empty,
}

/// Parses one scope argument. A trailing `/` makes a directory; `::` splits a file path from a
/// qualified symbol name.
pub fn parse(arg: &str) -> Result<Scope, ScopeError> {
    if arg.is_empty() {
        return Err(ScopeError::Empty);
    }
    if let Some((path, name)) = arg.split_once("::") {
        if name.is_empty() || path.ends_with('/') {
            return Err(ScopeError::BadSymbol {
                scope: arg.to_string(),
            });
        }
        check_path(arg, path)?;
        return Ok(Scope::Symbol(SymbolId {
            path: path.to_string(),
            qualified_name: name.to_string(),
        }));
    }
    if let Some(dir) = arg.strip_suffix('/') {
        check_path(arg, dir)?;
        return Ok(Scope::Dir {
            path: dir.to_string(),
        });
    }
    check_path(arg, arg)?;
    Ok(Scope::File {
        path: arg.to_string(),
    })
}

fn check_path(scope: &str, path: &str) -> Result<(), ScopeError> {
    let bad = path.is_empty()
        || path.contains('\\')
        || path.contains('\0')
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..");
    if bad {
        return Err(ScopeError::NotCanonical {
            scope: scope.to_string(),
        });
    }
    Ok(())
}

/// Resolves `raw` (absolute, or relative to `cwd`) against the worktree `root`, which must be
/// canonical. Returns the repo-relative `/`-separated path, or `None` if the file is outside the
/// worktree. The file need not exist: its deepest existing ancestor is canonicalized, so a
/// symlinked prefix such as macOS `/var` compares equal to the root.
pub fn relative_to_root(root: &Path, cwd: &Path, raw: &str) -> Option<String> {
    let joined = cwd.join(raw);
    let normalized = normalize(&joined);
    let resolved = canonicalize_prefix(&normalized);
    let rel = resolved.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Lexically removes `.` and resolves `..`, without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::Normal(_) | Component::RootDir | Component::Prefix(_) => {
                out.push(component);
            }
        }
    }
    out
}

fn canonicalize_prefix(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut head = path.to_path_buf();
    loop {
        if let Ok(real) = head.canonicalize() {
            let mut full = real;
            for part in tail.iter().rev() {
                full.push(part);
            }
            return full;
        }
        let Some(name) = head.file_name().map(std::ffi::OsStr::to_os_string) else {
            return path.to_path_buf();
        };
        tail.push(name);
        if !head.pop() {
            return path.to_path_buf();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_spellings() {
        assert_eq!(
            parse("src/auth/").unwrap(),
            Scope::Dir {
                path: "src/auth".into()
            }
        );
        assert_eq!(
            parse("src/a.rs").unwrap(),
            Scope::File {
                path: "src/a.rs".into()
            }
        );
        assert_eq!(
            parse("src/a.rs::Session::refresh").unwrap(),
            Scope::Symbol(SymbolId {
                path: "src/a.rs".into(),
                qualified_name: "Session::refresh".into()
            })
        );
    }

    #[test]
    fn rejects_non_canonical_paths() {
        for bad in [
            "/abs.rs",
            "./a.rs",
            "../a.rs",
            "a//b.rs",
            "a/../b.rs",
            "a\\b.rs",
            "/",
            "//",
            "src/./a.rs",
        ] {
            let result = parse(bad);
            assert!(
                matches!(result, Err(ScopeError::NotCanonical { .. })),
                "{bad}: {result:?}"
            );
        }
        assert_eq!(parse(""), Err(ScopeError::Empty));
    }

    #[test]
    fn rejects_incomplete_symbols() {
        assert!(matches!(
            parse("src/a.rs::"),
            Err(ScopeError::BadSymbol { .. })
        ));
        assert!(matches!(
            parse("src/::name"),
            Err(ScopeError::BadSymbol { .. })
        ));
        assert!(matches!(
            parse("::name"),
            Err(ScopeError::NotCanonical { .. })
        ));
    }

    #[test]
    fn resolves_files_inside_the_root_even_when_they_do_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let rel = |raw: &str| relative_to_root(&root, &root, raw);
        assert_eq!(rel("src/new.rs").as_deref(), Some("src/new.rs"));
        assert_eq!(
            rel(root.join("src/../src/x.rs").to_str().unwrap()).as_deref(),
            Some("src/x.rs")
        );
        assert_eq!(rel("../outside.rs"), None);
        assert_eq!(rel("/etc/hosts"), None);
        assert_eq!(rel("."), None);
    }
}
