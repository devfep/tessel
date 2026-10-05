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

/// Where a file lies relative to the worktree.
#[derive(Debug, PartialEq, Eq)]
pub enum Located {
    /// Repo-relative, `/`-separated path.
    Inside(String),
    Outside,
    /// Inside, but a path component is not valid UTF-8, so no scope can name it.
    NotUtf8,
}

/// Resolves `raw` (absolute, or relative to `cwd`) against the worktree `root`, which must be
/// canonical. Symlinks are resolved first: the deepest existing ancestor is canonicalized, so a
/// link inside the worktree that points outside counts as outside, one that points at another
/// file in the worktree counts as that file, and a symlinked prefix such as macOS `/var`
/// compares equal to the root. The file itself need not exist.
pub fn locate(root: &Path, cwd: &Path, raw: &str) -> Located {
    let joined = cwd.join(raw);
    let normalized = normalize(&joined);
    classify(root, &canonicalize_prefix(&normalized))
}

fn classify(root: &Path, resolved: &Path) -> Located {
    let Ok(rel) = resolved.strip_prefix(root) else {
        return Located::Outside;
    };
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => match part.to_str() {
                Some(part) => parts.push(part.to_string()),
                None => return Located::NotUtf8,
            },
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Located::Outside;
            }
        }
    }
    if parts.is_empty() {
        Located::Outside
    } else {
        Located::Inside(parts.join("/"))
    }
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
    fn locates_files_inside_the_root_even_when_they_do_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let find = |raw: &str| locate(&root, &root, raw);
        let inside = |path: &str| Located::Inside(path.to_string());
        assert_eq!(find("src/new.rs"), inside("src/new.rs"));
        assert_eq!(
            find(root.join("src/../src/x.rs").to_str().unwrap()),
            inside("src/x.rs")
        );
        assert_eq!(find("../outside.rs"), Located::Outside);
        assert_eq!(find("/etc/hosts"), Located::Outside);
        assert_eq!(find("."), Located::Outside);
    }

    #[test]
    fn symlinks_are_resolved_before_the_inside_check() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("escape")).unwrap();
        std::os::unix::fs::symlink(root.join("src/a.rs"), root.join("alias.rs")).unwrap();
        assert_eq!(locate(&root, &root, "escape/new.rs"), Located::Outside);
        assert_eq!(
            locate(&root, &root, "alias.rs"),
            Located::Inside("src/a.rs".into())
        );
    }

    #[test]
    fn a_non_utf8_name_is_reported_not_ignored() {
        use std::os::unix::ffi::OsStrExt;
        let root = Path::new("/work/tree");
        let bad = root.join(std::ffi::OsStr::from_bytes(b"caf\xe9.rs"));
        assert_eq!(classify(root, &bad), Located::NotUtf8);
        assert_eq!(
            classify(root, &root.join("ok.rs")),
            Located::Inside("ok.rs".into())
        );
    }
}
