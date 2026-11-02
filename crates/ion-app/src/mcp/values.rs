//! Shared MCP value syntax, inert validation and connection-local resolution.
use super::config::valid_env_name;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::Path,
    time::Duration,
};

/// A literal environment template or an explicitly authorized native argv helper.
/// Helper output is data, never another template or command.
#[derive(Clone, Serialize)]
#[serde(untagged)]
pub enum McpValue {
    Text(String),
    Command {
        command: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
    },
}

impl From<String> for McpValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}
impl From<&str> for McpValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for McpValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Helper {
            command: String,
            #[serde(default)]
            args: Vec<String>,
        }
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = McpValue;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string or command/args object")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(McpValue::Text(value.to_owned()))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(McpValue::Text(value))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                let helper =
                    Helper::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(McpValue::Command {
                    command: helper.command,
                    args: helper.args,
                })
            }
        }
        // Decode the helper directly so duplicate structural fields cannot be
        // erased by an intermediate JSON Value. Redact every failure shape.
        deserializer
            .deserialize_any(Visitor)
            .map_err(|_| D::Error::custom("MCP value must be a string or command/args object"))
    }
}

impl McpValue {
    pub(super) fn validate(&self) -> Result<()> {
        match self {
            Self::Text(value) => ensure!(!value.contains('\0'), "invalid MCP value"),
            Self::Command { command, args } => {
                ensure!(
                    !command.trim().is_empty() && !command.contains('\0'),
                    "invalid MCP helper command"
                );
                let mut remaining = MAX_CONFIGURED_BYTES;
                for value in std::iter::once(command).chain(args) {
                    ensure!(!value.contains('\0'), "invalid MCP helper argument");
                    remaining = remaining
                        .checked_sub(value.len())
                        .context("MCP helper argv exceeds byte limit")?;
                }
            }
        }
        Ok(())
    }
}

/// Preflight every literal before admitting helper effects. Overrides never
/// reference siblings, and identical helpers are not globally memoized.
pub(super) async fn resolve(
    values: &BTreeMap<String, McpValue>,
    inherited: &BTreeMap<OsString, OsString>,
    cwd: &Path,
    stop: tokio_util::sync::CancellationToken,
    validate: impl Fn(&str) -> Result<()>,
) -> Result<BTreeMap<String, String>> {
    ensure!(!stop.is_cancelled(), "MCP value preparation cancelled");
    let mut remaining = MAX_CONFIGURED_BYTES;
    let mut resolved = BTreeMap::new();
    for (name, value) in values {
        value.validate()?;
        remaining = remaining
            .checked_sub(name.len())
            .context("MCP configured values exceed byte limit")?;
        if let McpValue::Text(template) = value {
            let value = expand(template, inherited, &mut remaining)
                .with_context(|| format!("cannot resolve MCP value {name}"))?;
            validate(&value)?;
            resolved.insert(name.clone(), value);
        }
    }
    // One shared budget bounds the entire helper phase, not N serial timeouts.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    for (name, value) in values {
        if let McpValue::Command { command, args } = value {
            ensure!(remaining != 0, "MCP configured values exceed byte limit");
            let mut process = tokio::process::Command::new(command);
            process
                .args(args)
                .current_dir(cwd)
                .env_clear()
                .envs(inherited);
            let value = super::value_command::capture(process, remaining, deadline, stop.clone())
                .await
                .with_context(|| format!("cannot resolve MCP value {name}"))?;
            validate(&value)?;
            remaining -= value.len();
            resolved.insert(name.clone(), value);
        }
    }
    ensure!(!stop.is_cancelled(), "MCP value preparation cancelled");
    Ok(resolved)
}

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
    fn helper_structural_duplicates_are_rejected_without_disclosing_values() {
        for helper in [
            r#"{"command":"safe","command":"731982654"}"#,
            r#"{"command":"safe","args":[],"args":["731982654"]}"#,
        ] {
            let raw = format!(r#"{{"command":"server","env":{{"TOKEN":{helper}}}}}"#);
            let error = serde_json::from_str::<super::super::McpStdioServer>(&raw)
                .err()
                .expect("duplicate helper fields are ambiguous");
            assert!(!error.to_string().contains("731982654"));
        }
    }

    #[tokio::test]
    async fn helpers_follow_literal_preflight_and_capture_parent_not_sibling_values() {
        let root = std::env::temp_dir().join(format!("ion-mcp-values-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("helper.py"), "import os,sys\nwith open('calls','a') as f:f.write('x')\nprint(os.environ['SOURCE']+'|'+sys.argv[1])\n").unwrap();
        let inherited = BTreeMap::from([
            ("SOURCE".into(), "parent".into()),
            ("PATH".into(), std::env::var_os("PATH").unwrap()),
        ]);
        let mut values = BTreeMap::from([
            (
                "A_HELPER".into(),
                McpValue::Command {
                    command: "python3".into(),
                    args: vec!["helper.py".into(), "${SOURCE} !literal".into()],
                },
            ),
            ("SOURCE".into(), "sibling".into()),
            ("Z_MISSING".into(), "${ABSENT}".into()),
        ]);
        assert!(
            resolve(
                &values,
                &inherited,
                &root,
                tokio_util::sync::CancellationToken::new(),
                |_| Ok(())
            )
            .await
            .is_err()
        );
        assert!(!root.join("calls").exists());
        values.remove("Z_MISSING");
        let resolved = resolve(
            &values,
            &inherited,
            &root,
            tokio_util::sync::CancellationToken::new(),
            |_| Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(resolved["A_HELPER"], "parent|${SOURCE} !literal");
        assert_eq!(resolved["SOURCE"], "sibling");
        assert_eq!(std::fs::read_to_string(root.join("calls")).unwrap(), "x");
        let mut changed = inherited;
        changed.insert("SOURCE".into(), "new-parent".into());
        assert_eq!(
            resolve(
                &values,
                &changed,
                &root,
                tokio_util::sync::CancellationToken::new(),
                |_| Ok(())
            )
            .await
            .unwrap()["A_HELPER"],
            "new-parent|${SOURCE} !literal"
        );
        assert_eq!(std::fs::read_to_string(root.join("calls")).unwrap(), "xx");
        std::fs::remove_dir_all(root).unwrap();
    }

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
