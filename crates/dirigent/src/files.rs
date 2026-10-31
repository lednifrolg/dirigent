//! Private files: owner-only permissions and atomic replacement.
//!
//! On Windows, files and directories take the access rules of the directory they are made in;
//! those of the user profile, where dirigent keeps its files, admit only the owner.

use std::fs::{self, DirBuilder, File};
use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::AppError;

/// Creates `dir` and its parents, readable only by the owner.
pub fn private_dir(dir: &Path) -> Result<(), AppError> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir).map_err(|e| AppError::io(dir, e))
}

fn parent(path: &Path) -> &Path {
    path.parent().unwrap_or_else(|| Path::new("."))
}

/// Reads JSON from `path`; `None` when the file does not exist.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, AppError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|source| AppError::Corrupt {
                path: path.to_path_buf(),
                message: source.to_string(),
            }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(AppError::io(path, e)),
    }
}

/// Replaces `path` with `value` as JSON. Readers see the old or the new file, never half of
/// one, and the file is readable only by the owner.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), AppError> {
    replace(path, |file| {
        serde_json::to_writer_pretty(&mut *file, value)?;
        file.write_all(b"\n")
    })
}

/// Replaces `path` with what `write` puts in a new file, atomically and readable only by the
/// owner.
fn replace(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> Result<(), AppError> {
    let dir = parent(path);
    private_dir(dir)?;
    // A random name, created exclusively with owner-only permissions; deleted again on failure.
    let mut temporary = tempfile::Builder::new()
        .prefix(".dirigent-")
        .tempfile_in(dir)
        .map_err(|e| AppError::io(dir, e))?;
    write(temporary.as_file_mut())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|e| AppError::io(path, e))?;
    // Renamed by std rather than `persist`: on Windows, std replaces a file that another process
    // has open, such as another dirigent reading the pairing, where `persist` fails with "access
    // denied".
    let temporary = temporary
        .into_temp_path()
        .keep()
        .map_err(|e| AppError::io(path, e.error))?;
    fs::rename(&temporary, path).map_err(|e| {
        let _ = fs::remove_file(&temporary);
        AppError::io(path, e)
    })
}

/// Deletes `path`; `false` when it did not exist.
pub fn remove(path: &Path) -> Result<bool, AppError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(AppError::io(path, e)),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn written_files_are_private_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/value.json");
        write_json(&path, &vec![1, 2, 3]).unwrap();

        assert_eq!(read_json::<Vec<i32>>(&path).unwrap(), Some(vec![1, 2, 3]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }
        let leftovers = fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1, "temporary file left behind");
        assert!(remove(&path).unwrap());
        assert!(!remove(&path).unwrap());
    }

    #[test]
    fn a_file_is_replaced_while_a_reader_holds_it_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("value.json");
        write_json(&path, &1).unwrap();
        let mut reader = File::open(&path).unwrap();

        write_json(&path, &2).unwrap();

        assert_eq!(read_json::<i32>(&path).unwrap(), Some(2));
        let mut old = String::new();
        reader.read_to_string(&mut old).unwrap();
        assert_eq!(old, "1\n");
    }

    #[test]
    fn missing_and_corrupt_files_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("value.json");
        assert_eq!(read_json::<i32>(&path).unwrap(), None);
        fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            read_json::<i32>(&path),
            Err(AppError::Corrupt { .. })
        ));
    }
}
