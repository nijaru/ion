//! Literal home-prefix expansion shared by native tools and local input paths.
use std::{borrow::Cow, ffi::OsStr, io, os::unix::ffi::OsStrExt, path::Path};

/// Expand a leading `~` component using an explicitly supplied absolute home.
/// Other paths are unchanged; no shell evaluation or parent normalization occurs.
pub fn expand_home_path<'a>(path: &'a Path, home: Option<&Path>) -> io::Result<Cow<'a, Path>> {
    // Components discard trailing separators and `/.`; retain the literal
    // suffix so expansion cannot change a directory path into a file path.
    let raw = path.as_os_str().as_bytes();
    if raw != b"~" && !raw.starts_with(b"~/") {
        return Ok(Cow::Borrowed(path));
    }
    let home = home.filter(|home| home.is_absolute()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "HOME is unavailable or not absolute; cannot resolve a home-relative path",
        )
    })?;
    let mut expanded = home.to_path_buf();
    if raw.len() > 1 {
        let mut suffix = &raw[1..];
        // A repeated separator must not turn the suffix into a rooted join.
        while let Some(rest) = suffix.strip_prefix(b"/") {
            suffix = rest;
        }
        expanded.push(OsStr::from_bytes(suffix));
    }
    Ok(Cow::Owned(expanded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn expands_only_home_prefix_without_retargeting_literal_paths() {
        let home = Path::new("/bound/home");
        for (raw, expected) in [
            ("~", "/bound/home"),
            ("~/", "/bound/home/"),
            ("~/file", "/bound/home/file"),
            ("~//file", "/bound/home/file"),
            ("~/link/../file", "/bound/home/link/../file"),
            ("~/missing/", "/bound/home/missing/"),
            ("~/missing/.", "/bound/home/missing/."),
            ("~/café 🦀", "/bound/home/café 🦀"),
        ] {
            let expanded = expand_home_path(Path::new(raw), Some(home)).unwrap();
            assert_eq!(
                expanded.as_os_str(),
                std::ffi::OsStr::new(expected),
                "{raw}"
            );
        }
        for raw in [
            "",
            ".",
            "file",
            "/file",
            "./~/file",
            "~someone/file",
            "@~/file",
            "$HOME/file",
            " ~/file",
        ] {
            let path = Path::new(raw);
            assert_eq!(
                expand_home_path(path, None).unwrap().as_os_str(),
                path.as_os_str()
            );
        }
        for home in [None, Some(Path::new("")), Some(Path::new("relative"))] {
            assert!(expand_home_path(Path::new("~/file"), home).is_err());
            assert!(expand_home_path(Path::new("~"), home).is_err());
        }
    }

    #[test]
    fn home_expansion_preserves_non_utf8_names() {
        let home = Path::new(std::ffi::OsStr::from_bytes(b"/home/\xff"));
        let expanded = expand_home_path(Path::new("~/file"), Some(home)).unwrap();
        assert_eq!(expanded.as_os_str().as_bytes(), b"/home/\xff/file");
        let literal = Path::new(std::ffi::OsStr::from_bytes(b"./~/\xff"));
        assert_eq!(
            expand_home_path(literal, Some(home)).unwrap().as_os_str(),
            literal.as_os_str()
        );
    }
}
