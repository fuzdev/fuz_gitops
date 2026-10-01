//! Reading a file that must be a regular one, so a read can't block on a
//! FIFO or a device, or run on through a dir.
//!
//! Shared by the readers of git's formats (`gitdir`, `probe`, `scan`) and of
//! Claude Code's (`sessions`): what they have in common is only this, so it
//! sits beneath both rather than in either.

use std::io::Read as _;
use std::path::Path;

/// Opens a file that must be a regular one, the path followed: a FIFO, a
/// device, or a dir is an `InvalidInput` error rather than an open or a read
/// that could block forever.
///
/// # Errors
///
/// When it can't be stat'd or opened, or isn't a regular file.
pub fn open_regular(path: &Path) -> std::io::Result<std::fs::File> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    std::fs::File::open(path)
}

/// A regular file's bytes (the path followed), at most `max`.
///
/// # Errors
///
/// As `open_regular`, when it can't be read, or when it's larger than `max`
/// (`InvalidData`).
pub fn read_bounded_bytes(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_regular(path)?.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("larger than {max} bytes"),
        ));
    }
    Ok(bytes)
}

/// A regular file's contents (the path followed) as UTF-8, whole.
///
/// # Errors
///
/// As `open_regular`, when it can't be read, or isn't UTF-8.
pub fn read_regular(path: &Path) -> std::io::Result<String> {
    std::io::read_to_string(open_regular(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_regular_file_is_read() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f");
        std::fs::write(&file, "abc").unwrap();
        std::os::unix::fs::symlink(&file, tmp.path().join("link")).unwrap();
        assert_eq!(read_regular(&file).unwrap(), "abc");
        // links followed
        assert_eq!(read_regular(&tmp.path().join("link")).unwrap(), "abc");
        let not = |e: std::io::Error| e.kind() == std::io::ErrorKind::InvalidInput;
        assert!(not(read_regular(tmp.path()).unwrap_err()));
        assert!(not(read_bounded_bytes(tmp.path(), 10).unwrap_err()));
        let fifo = tmp.path().join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        assert!(not(read_regular(&fifo).unwrap_err()));
        assert!(not(read_bounded_bytes(&fifo, 10).unwrap_err()));
        let missing = read_regular(&tmp.path().join("missing")).unwrap_err();
        assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn a_bounded_read_refuses_a_larger_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f");
        std::fs::write(&file, "abcd").unwrap();
        assert_eq!(read_bounded_bytes(&file, 4).unwrap(), b"abcd");
        let e = read_bounded_bytes(&file, 3).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
        std::fs::write(&file, b"\xff").unwrap();
        assert_eq!(read_bounded_bytes(&file, 1).unwrap(), b"\xff");
        let e = read_regular(&file).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    }
}
