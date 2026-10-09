//! Parsing of scope arguments (`dir/`, `path/file.rs`, `path/file.rs::qualified::name`) into
//! protocol scopes, and conversion of absolute file paths into repo-relative ones.

use std::path::{Component, Path, PathBuf};

use tessel_coordinator::protocol::{Mode, Scope, SymbolId};
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
    #[error(
        "scope {scope:?} does not exist in this worktree. Pass each path as a separate argument \
         (a space-joined shell variable makes one scope), or add `--mode create` to claim \
         something you are about to create"
    )]
    Missing { scope: String },
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

/// Refuses `scope` (parsed from `arg`) when the file or directory it names is absent from the
/// worktree `root`. A symbol scope needs its file. Claims in `create` mode skip this check
/// because they name what the agent is about to add.
pub fn check_exists(root: &Path, arg: &str, scope: &Scope, mode: Mode) -> Result<(), ScopeError> {
    match mode {
        Mode::Create => return Ok(()),
        Mode::Depend | Mode::EditBody | Mode::EditSignature => {}
    }
    let path = match scope {
        Scope::File { path } | Scope::Dir { path } => path,
        Scope::Symbol(symbol) => &symbol.path,
    };
    if root.join(path).exists() {
        return Ok(());
    }
    Err(ScopeError::Missing {
        scope: arg.to_string(),
    })
}

/// A scope argument holding whitespace is usually several paths joined into one word.
pub fn has_whitespace(arg: &str) -> bool {
    arg.chars().any(char::is_whitespace)
}

/// The file scope for a repo-relative path taken as it stands, so a `::` in a file name is not
/// read as a symbol separator.
pub fn file(path: &str) -> Result<Scope, ScopeError> {
    check_path(path, path)?;
    Ok(Scope::File {
        path: path.to_string(),
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
    classify(root, &resolve(cwd, raw))
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

/// Walks `raw` from `cwd` one component at a time, the way the filesystem does: each existing
/// component that is a symlink is replaced by its target before the next component is read, so
/// `link/..` is the parent of the link's target, not the directory holding the link. Components
/// that do not exist yet are appended as they are.
fn resolve(cwd: &Path, raw: &str) -> PathBuf {
    let start = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    resolve_from(start, Path::new(raw), 0)
}

/// A chain of links longer than this is a loop; the path is left as it stands.
const MAX_LINK_DEPTH: u32 = 40;

fn resolve_from(start: PathBuf, raw: &Path, depth: u32) -> PathBuf {
    let mut current = start;
    for component in raw.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => {
                current = PathBuf::from(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                current.pop();
            }
            Component::Normal(name) => {
                current.push(name);
                let is_link = current
                    .symlink_metadata()
                    .is_ok_and(|meta| meta.file_type().is_symlink());
                if is_link {
                    current = follow_link(&current, depth);
                }
            }
        }
    }
    current
}

/// Where the symlink at `link` leads. A dangling link has no canonical path, but a write
/// through it still lands at its target, so its target is read and resolved in turn.
fn follow_link(link: &Path, depth: u32) -> PathBuf {
    if let Ok(target) = link.canonicalize() {
        return target;
    }
    let (Some(parent), Ok(target)) = (link.parent(), std::fs::read_link(link)) else {
        return link.to_path_buf();
    };
    if depth >= MAX_LINK_DEPTH {
        return link.to_path_buf();
    }
    resolve_from(parent.to_path_buf(), &target, depth + 1)
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
    fn a_file_scope_keeps_a_double_colon_in_the_name() {
        assert_eq!(
            file("src/a::b.rs"),
            Ok(Scope::File {
                path: "src/a::b.rs".into()
            })
        );
        assert!(file("src//a.rs").is_err());
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
    fn dot_dot_after_a_symlink_means_the_parent_of_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let far = outside.path().canonicalize().unwrap().join("sub");
        std::fs::create_dir_all(&far).unwrap();
        std::fs::create_dir_all(root.join("a/real")).unwrap();
        std::os::unix::fs::symlink(&far, root.join("out")).unwrap();
        std::os::unix::fs::symlink(root.join("a/real"), root.join("in")).unwrap();
        // Written lexically `out/../x.rs` would be inside; on disk it is beside `sub`, outside.
        assert_eq!(locate(&root, &root, "out/../x.rs"), Located::Outside);
        assert_eq!(
            locate(&root, &root, "in/../x.rs"),
            Located::Inside("a/x.rs".into())
        );
    }

    #[test]
    fn a_dangling_symlink_resolves_to_where_a_write_would_land() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let far = outside.path().canonicalize().unwrap().join("missing.rs");
        std::os::unix::fs::symlink("src/new.rs", root.join("alias.rs")).unwrap();
        std::os::unix::fs::symlink("alias.rs", root.join("chain.rs")).unwrap();
        std::os::unix::fs::symlink(&far, root.join("away.rs")).unwrap();
        std::os::unix::fs::symlink("loop_b.rs", root.join("loop_a.rs")).unwrap();
        std::os::unix::fs::symlink("loop_a.rs", root.join("loop_b.rs")).unwrap();
        let inside = |path: &str| Located::Inside(path.to_string());
        assert_eq!(locate(&root, &root, "alias.rs"), inside("src/new.rs"));
        assert_eq!(locate(&root, &root, "chain.rs"), inside("src/new.rs"));
        assert_eq!(locate(&root, &root, "away.rs"), Located::Outside);
        // A loop has no target; it stays where it is and is claimed as itself.
        assert_eq!(locate(&root, &root, "loop_a.rs"), inside("loop_a.rs"));
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
