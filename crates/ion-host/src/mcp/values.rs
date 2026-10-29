//! Bounded, non-executing configuration values shared by local and HTTP servers.
use super::config::valid_env_name;
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
};

pub(super) const MAX_CONFIGURED_BYTES: usize = 1024 * 1024;

pub(super) fn expand(
    mut input: &str,
    inherited: &BTreeMap<OsString, OsString>,
    remaining: &mut usize,
) -> Result<String> {
    let mut output = String::new();
    let mut append = |part: &str| -> Result<()> {
        *remaining = remaining
            .checked_sub(part.len())
            .context("MCP configured values exceed byte limit")?;
        output.push_str(part);
        Ok(())
    };
    while let Some((literal, expression)) = input.split_once("${") {
        append(literal)?;
        let (expression, rest) = expression
            .split_once('}')
            .context("unterminated MCP value reference")?;
        let (name, fallback) = expression
            .split_once(":-")
            .map_or((expression, None), |(name, value)| (name, Some(value)));
        ensure!(valid_env_name(name), "invalid MCP value reference");
        let value = match inherited.get(OsStr::new(name)) {
            Some(value) => value.to_str().context("MCP value reference is not UTF-8")?,
            None => fallback.with_context(|| format!("MCP environment needs {name}"))?,
        };
        append(value)?;
        input = rest;
    }
    append(input)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn interpolation_is_bounded_non_recursive_and_never_executes_commands() {
        let inherited = BTreeMap::from([
            ("KEY".into(), "secret${MISSING}".into()),
            ("EMPTY".into(), "".into()),
            ("BINARY".into(), OsString::from_vec(vec![0xff])),
        ]);
        for (input, expected) in [
            ("literal $KEY !command", "literal $KEY !command"),
            ("before-${KEY}-after", "before-secret${MISSING}-after"),
            ("${ABSENT:-fallback}", "fallback"),
            ("${EMPTY:-fallback}", ""),
            ("${ABSENT:-}", ""),
        ] {
            assert_eq!(expand(input, &inherited, &mut 128).unwrap(), expected);
        }
        for input in ["${MISSING}", "${KEY", "${}", "${1KEY}", "${BINARY}"] {
            assert!(expand(input, &inherited, &mut 128).is_err(), "{input}");
        }
        assert!(expand("${KEY}${KEY}", &inherited, &mut 20).is_err());
        assert!(expand("literal", &inherited, &mut 6).is_err());
        assert_eq!(expand("literal", &inherited, &mut 7).unwrap(), "literal");
    }
}
