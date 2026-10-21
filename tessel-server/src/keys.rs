//! The key directory: one Ed25519 key pair per `kid`.
//!
//! `<kid>.key` holds the secret seed and `<kid>.pub` the public key, each as base64url without
//! padding of 32 bytes and a final newline. The seed file is written with mode 0600 and never
//! overwritten. The server loads every `.pub` file at start and never reads a `.key` file; only
//! `admin token` does. A `kid` follows the repository-name grammar of `id.rs`.
//!
//! Errors name the file and say what is wrong with it; they never carry its contents.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use data_encoding::BASE64URL_NOPAD;
use ed25519_dalek::{SigningKey, VerifyingKey};

use crate::id::{Name, NameError};

const SECRET_EXTENSION: &str = "key";
const PUBLIC_EXTENSION: &str = "pub";
const SECRET_MODE: u32 = 0o600;
const PUBLIC_MODE: u32 = 0o644;
const KEY_BYTES: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("cannot read the key directory {dir}: {source}")]
    Dir {
        dir: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot create the key directory {dir}: {source}")]
    CreateDir {
        dir: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot read or write the key file {path}: {source}")]
    File {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("the key file {path} is not valid: {reason}")]
    Format { path: PathBuf, reason: &'static str },
    #[error("the key file {path} has a name that is not a key id: {source}")]
    Kid { path: PathBuf, source: NameError },
    #[error("the key id {kid:?} is not valid: {source}")]
    BadKid { kid: String, source: NameError },
    #[error("the key directory {dir} holds no `.pub` file; run `tessel-server admin keygen`")]
    NoKeys { dir: PathBuf },
    #[error("{path} already exists; keygen never overwrites a key")]
    Exists { path: PathBuf },
    #[error("the operating system gave no random bytes: {0}")]
    Random(getrandom::Error),
}

fn secret_path(dir: &Path, kid: &str) -> PathBuf {
    dir.join(format!("{kid}.{SECRET_EXTENSION}"))
}

fn public_path(dir: &Path, kid: &str) -> PathBuf {
    dir.join(format!("{kid}.{PUBLIC_EXTENSION}"))
}

fn check_kid(kid: &str) -> Result<(), KeyError> {
    Name::parse(kid)
        .map(drop)
        .map_err(|source| KeyError::BadKid {
            kid: kid.to_string(),
            source,
        })
}

/// Write a new key pair for `kid` into `dir`, creating the directory. Refuses if either file
/// exists, and leaves nothing behind if the second file cannot be written.
pub fn generate(dir: &Path, kid: &str) -> Result<(), KeyError> {
    check_kid(kid)?;
    std::fs::create_dir_all(dir).map_err(|source| KeyError::CreateDir {
        dir: dir.to_path_buf(),
        source,
    })?;
    let mut seed = [0_u8; KEY_BYTES];
    getrandom::fill(&mut seed).map_err(KeyError::Random)?;
    let signing = SigningKey::from_bytes(&seed);
    let secret = secret_path(dir, kid);
    let public = public_path(dir, kid);
    create_new(&secret, SECRET_MODE, &BASE64URL_NOPAD.encode(&seed))?;
    let public_text = BASE64URL_NOPAD.encode(signing.verifying_key().as_bytes());
    if let Err(error) = create_new(&public, PUBLIC_MODE, &public_text) {
        if let Err(cleanup) = std::fs::remove_file(&secret) {
            let path = secret.display();
            tracing::warn!(%path, error = %cleanup, "could not remove a half-written key");
        }
        return Err(error);
    }
    Ok(())
}

fn create_new(path: &Path, mode: u32, text: &str) -> Result<(), KeyError> {
    let file_error = |source: std::io::Error| KeyError::File {
        path: path.to_path_buf(),
        source,
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|source| {
            if source.kind() == ErrorKind::AlreadyExists {
                return KeyError::Exists {
                    path: path.to_path_buf(),
                };
            }
            file_error(source)
        })?;
    file.write_all(format!("{text}\n").as_bytes())
        .map_err(file_error)
}

fn read_key_bytes(path: &Path) -> Result<[u8; KEY_BYTES], KeyError> {
    let format = |reason| KeyError::Format {
        path: path.to_path_buf(),
        reason,
    };
    let text = std::fs::read_to_string(path).map_err(|source| KeyError::File {
        path: path.to_path_buf(),
        source,
    })?;
    let bytes = BASE64URL_NOPAD
        .decode(text.trim().as_bytes())
        .map_err(|_| format("not base64url without padding"))?;
    <[u8; KEY_BYTES]>::try_from(bytes).map_err(|_| format("not 32 bytes"))
}

/// The secret key of `kid`, for minting a token.
pub fn load_signing_key(dir: &Path, kid: &str) -> Result<SigningKey, KeyError> {
    check_kid(kid)?;
    let seed = read_key_bytes(&secret_path(dir, kid))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Every public key in `dir`, by `kid`. A `.pub` file that is not a key, or whose name is not a
/// `kid`, fails the load; so does a directory with no key at all.
pub fn load_public_keys(dir: &Path) -> Result<BTreeMap<String, VerifyingKey>, KeyError> {
    let dir_error = |source| KeyError::Dir {
        dir: dir.to_path_buf(),
        source,
    };
    let mut keys = BTreeMap::new();
    for entry in std::fs::read_dir(dir).map_err(dir_error)? {
        let path = entry.map_err(dir_error)?.path();
        if path.extension() != Some(OsStr::new(PUBLIC_EXTENSION)) {
            continue;
        }
        let kid = path
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or_else(|| KeyError::Kid {
                path: path.clone(),
                source: NameError::Character,
            })?;
        Name::parse(kid).map_err(|source| KeyError::Kid {
            path: path.clone(),
            source,
        })?;
        let bytes = read_key_bytes(&path)?;
        let key = VerifyingKey::from_bytes(&bytes).map_err(|_| KeyError::Format {
            path: path.clone(),
            reason: "not an Ed25519 public key",
        })?;
        if key.is_weak() {
            return Err(KeyError::Format {
                path,
                reason: "a weak public key, which verifies forged signatures",
            });
        }
        keys.insert(kid.to_string(), key);
    }
    if keys.is_empty() {
        return Err(KeyError::NoKeys {
            dir: dir.to_path_buf(),
        });
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn keygen_writes_a_secret_seed_with_mode_0600_and_a_public_key() {
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("keys");
        generate(&keys, "k1").unwrap();
        let mode = std::fs::metadata(keys.join("k1.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        let loaded = load_public_keys(&keys).unwrap();
        let signing = load_signing_key(&keys, "k1").unwrap();
        assert_eq!(loaded["k1"], signing.verifying_key());
    }

    #[test]
    fn keygen_refuses_to_overwrite_either_file_and_leaves_both_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        generate(dir.path(), "k1").unwrap();
        let before = (
            std::fs::read(dir.path().join("k1.key")).unwrap(),
            std::fs::read(dir.path().join("k1.pub")).unwrap(),
        );
        let again = generate(dir.path(), "k1").unwrap_err();
        assert!(matches!(again, KeyError::Exists { .. }), "{again}");
        std::fs::remove_file(dir.path().join("k1.key")).unwrap();
        let half = generate(dir.path(), "k1").unwrap_err();
        assert!(matches!(half, KeyError::Exists { .. }), "{half}");
        assert!(
            !dir.path().join("k1.key").exists(),
            "a refused keygen leaves no secret behind"
        );
        assert_eq!(std::fs::read(dir.path().join("k1.pub")).unwrap(), before.1);
    }

    #[test]
    fn a_kid_is_a_name_and_a_bad_one_is_refused_before_a_file_is_made() {
        let dir = tempfile::tempdir().unwrap();
        for kid in ["", "K1", "a/b", "..", "a.b", "-a"] {
            let error = generate(dir.path(), kid).unwrap_err();
            assert!(matches!(error, KeyError::BadKid { .. }), "{kid:?}: {error}");
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_public_keys_of_every_kid_in_the_directory_are_loaded() {
        let dir = tempfile::tempdir().unwrap();
        generate(dir.path(), "old").unwrap();
        generate(dir.path(), "new").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a key").unwrap();
        let kids: Vec<String> = load_public_keys(dir.path()).unwrap().into_keys().collect();
        assert_eq!(kids, ["new", "old"]);
    }

    #[test]
    fn an_empty_directory_a_missing_one_and_a_bad_pub_file_each_fail_naming_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let none = load_public_keys(dir.path()).unwrap_err();
        assert!(matches!(none, KeyError::NoKeys { .. }), "{none}");
        assert!(none.to_string().contains(&dir.path().display().to_string()));
        let missing = load_public_keys(&dir.path().join("absent")).unwrap_err();
        assert!(matches!(missing, KeyError::Dir { .. }), "{missing}");
        for (name, text) in [
            ("a.pub", "!!!"),
            ("b.pub", "AAAA"),
            ("c.pub", ""),
            ("Bad.pub", "AAAA"),
        ] {
            let sub = tempfile::tempdir().unwrap();
            std::fs::write(sub.path().join(name), text).unwrap();
            let error = load_public_keys(sub.path()).unwrap_err();
            assert!(error.to_string().contains(name), "{name}: {error}");
            assert!(!error.to_string().contains(text) || text.is_empty());
        }
    }

    #[test]
    fn a_weak_public_key_is_refused_at_load_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut identity_point = [0_u8; 32];
        identity_point[0] = 1;
        let text = BASE64URL_NOPAD.encode(&identity_point);
        std::fs::write(dir.path().join("weak.pub"), text).unwrap();
        let error = load_public_keys(dir.path()).unwrap_err();
        assert!(matches!(error, KeyError::Format { .. }), "{error}");
        assert!(error.to_string().contains("weak.pub"), "{error}");
    }

    #[test]
    fn a_secret_file_that_is_missing_or_not_a_seed_names_the_file_and_not_its_text() {
        let dir = tempfile::tempdir().unwrap();
        let missing = load_signing_key(dir.path(), "k1").unwrap_err();
        assert!(missing.to_string().contains("k1.key"), "{missing}");
        std::fs::write(dir.path().join("k1.key"), "S3CRET-not-a-seed").unwrap();
        let error = load_signing_key(dir.path(), "k1").unwrap_err();
        assert!(error.to_string().contains("k1.key"), "{error}");
        assert!(!error.to_string().contains("S3CRET"), "{error}");
    }
}
