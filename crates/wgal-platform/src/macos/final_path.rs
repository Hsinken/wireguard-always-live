//! Where an open file really is, from its descriptor rather than from a
//! path: `fcntl(F_GETPATH)` (through `rustix`, so without `unsafe` here),
//! so nothing done to the folders after the file was opened can make the
//! answer another file's. The desktop GUI's log export uses it (AGENTS.md
//! "Repository layout"); the adapter does not.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, ErrorKind};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

/// Where `file` is now: the absolute path the kernel names it by. A file
/// removed since it was opened is `NotFound`, and anything without an
/// absolute path `InvalidData`.
pub fn final_path_of_file(file: &File) -> io::Result<PathBuf> {
    let path = PathBuf::from(OsString::from_vec(rustix::fs::getpath(file)?.into_bytes()));
    if file.metadata()?.nlink() == 0 {
        return Err(ErrorKind::NotFound.into());
    }
    if !path.is_absolute() {
        return Err(io::Error::new(ErrorKind::InvalidData, "not a file path"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wgal-final-path-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_final_path_of_an_open_file_is_where_the_file_is() {
        let dir = temp_dir("open");
        let path = dir.join("new.txt");
        let file = File::create_new(&path).unwrap();
        assert_eq!(
            final_path_of_file(&file).unwrap(),
            fs::canonicalize(&path).unwrap()
        );
        // Moved, and another file put at its old path: the answer follows
        // the file the descriptor holds.
        let moved = dir.join("moved");
        fs::create_dir(&moved).unwrap();
        fs::rename(&path, moved.join("new.txt")).unwrap();
        fs::write(&path, b"another").unwrap();
        assert_eq!(
            final_path_of_file(&file).unwrap(),
            fs::canonicalize(&moved).unwrap().join("new.txt")
        );
        // Removed: no path names it any more.
        fs::remove_file(moved.join("new.txt")).unwrap();
        assert_eq!(
            final_path_of_file(&file).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        drop(file);
        fs::remove_dir_all(&dir).unwrap();
    }
}
