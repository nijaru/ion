//! Bounded reads of verified regular files, including symlink targets.
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

use rustix::fs::{Mode, OFlags, open};

pub(crate) fn open_regular(path: &Path) -> io::Result<File> {
    // Checking the path before a blocking open races with replacement. Open
    // nonblocking, then validate the same descriptor we will actually read.
    let file = File::from(open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

/// Read at most `limit` bytes from a verified regular file or its symlink.
/// Special files are rejected without a blocking open; oversized data is an error.
pub fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let bound = u64::try_from(limit)
        .ok()
        .and_then(|limit| limit.checked_add(1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "read byte bound overflow"))?;
    let file = open_regular(path)?;
    let too_large = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file exceeds {limit}-byte read bound"),
        )
    };
    if file.metadata()?.len() >= bound {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(bound).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(too_large());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn bounded_reads_follow_regular_symlinks_and_reject_special_files() {
        let root = std::env::temp_dir().join(format!("ion-file-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("text"), "abc").unwrap();
        symlink("text", root.join("link")).unwrap();
        assert_eq!(read_bounded(&root.join("link"), 3).unwrap(), b"abc");
        assert!(read_bounded(&root.join("text"), 2).is_err());
        assert!(open_regular(&root).is_err());
        assert!(
            std::process::Command::new("mkfifo")
                .arg(root.join("pipe"))
                .status()
                .unwrap()
                .success()
        );
        assert!(open_regular(&root.join("pipe")).is_err());
        symlink("pipe", root.join("pipe-link")).unwrap();
        assert!(read_bounded(&root.join("pipe-link"), 1024).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
