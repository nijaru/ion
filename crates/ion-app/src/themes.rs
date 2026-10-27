//! Terminal-local selection, immutable palette preparation and user theme files.
use crate::presentation_style::{Role, Theme, is_dark};
use anyhow::{Context, Result, ensure};
use ion_terminal::{Appearance, TerminalColors};
use ratatui::style::Color;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};

const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Builtin {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum ColorValue {
    Index(u8),
    Text(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(rename = "$schema")]
    _schema: Option<String>,
    name: String,
    #[serde(default)]
    base: Builtin,
    #[serde(default)]
    vars: BTreeMap<String, ColorValue>,
    colors: BTreeMap<Role, ColorValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Definition {
    base: Builtin,
    colors: BTreeMap<Role, Color>,
    custom: bool,
}

impl Definition {
    fn load(root: &Path, name: &str) -> Result<Self> {
        validate_name(name)?;
        let base = match name {
            "system" => Some(Builtin::System),
            "dark" => Some(Builtin::Dark),
            "light" => Some(Builtin::Light),
            _ => None,
        };
        if let Some(base) = base {
            return Ok(Self {
                base,
                colors: BTreeMap::new(),
                custom: false,
            });
        }
        let bytes = ion_host::file_io::read_bounded(
            &root.join("themes").join(format!("{name}.json")),
            MAX_BYTES,
        )
        .with_context(|| format!("cannot read theme {name}"))?;
        let document: Document = serde_json::from_slice(&bytes).map_err(|error| {
            anyhow::anyhow!(
                "invalid theme {name} at line {}, column {}",
                error.line(),
                error.column()
            )
        })?;
        ensure!(document.name == name, "theme name must match its filename");
        let mut colors = BTreeMap::new();
        for (role, value) in &document.colors {
            colors.insert(
                *role,
                resolve_color(value, &document.vars, &mut BTreeSet::new())?,
            );
        }
        // Validate unused variables too: errors must not depend on which role uses them.
        for value in document.vars.values() {
            resolve_color(value, &document.vars, &mut BTreeSet::new())?;
        }
        Ok(Self {
            base: document.base,
            colors,
            custom: true,
        })
    }

    fn resolve(&self, colors: TerminalColors) -> Theme {
        let mut theme = match self.base {
            Builtin::System => Theme::system(colors.foreground, colors.background, &colors.palette),
            Builtin::Dark => Theme::fixed(true),
            Builtin::Light => Theme::fixed(false),
        };
        for (role, color) in &self.colors {
            theme.set_color(*role, *color);
        }
        theme
    }
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name != "."
            && name != ".."
            && !name
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '/' | '\\')),
        "theme names must be 1–128 bytes without controls or path separators"
    );
    Ok(())
}

fn resolve_color(
    value: &ColorValue,
    vars: &BTreeMap<String, ColorValue>,
    visiting: &mut BTreeSet<String>,
) -> Result<Color> {
    let ColorValue::Text(text) = value else {
        let ColorValue::Index(index) = value else {
            unreachable!()
        };
        return Ok(Color::Indexed(*index));
    };
    if text.is_empty() {
        return Ok(Color::Reset);
    }
    if let Some(hex) = text.strip_prefix('#') {
        ensure!(
            matches!(hex.len(), 3 | 6) && hex.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid theme hex color"
        );
        let full = if hex.len() == 3 {
            hex.chars().flat_map(|ch| [ch, ch]).collect::<String>()
        } else {
            hex.into()
        };
        let value = u32::from_str_radix(&full, 16).expect("validated hex");
        return Ok(Color::Rgb(
            (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
        ));
    }
    ensure!(
        visiting.len() < 32 && visiting.insert(text.clone()),
        "cyclic or overly deep theme variable"
    );
    let result = resolve_color(
        vars.get(text).context("unknown theme variable")?,
        vars,
        visiting,
    );
    visiting.remove(text);
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Selection {
    name: String,
    light: Definition,
    dark: Option<Definition>,
}

impl Selection {
    fn load(root: &Path, name: &str) -> Result<Self> {
        let (light, dark) = match name.split_once('/') {
            Some((light, dark)) => (
                Definition::load(root, light)?,
                Some(Definition::load(root, dark)?),
            ),
            None => (Definition::load(root, name)?, None),
        };
        Ok(Self {
            name: name.into(),
            light,
            dark,
        })
    }
    fn resolve(&self, colors: TerminalColors) -> Theme {
        let dark = terminal_is_dark(colors);
        self.dark
            .as_ref()
            .filter(|_| dark)
            .unwrap_or(&self.light)
            .resolve(colors)
    }
    fn custom(&self) -> bool {
        self.light.custom || self.dark.as_ref().is_some_and(|dark| dark.custom)
    }
}

fn terminal_is_dark(colors: TerminalColors) -> bool {
    colors
        .background
        .map(|bg| is_dark(bg, colors.foreground))
        .unwrap_or_else(|| match colors.appearance {
            Some(Appearance::Dark) => true,
            Some(Appearance::Light) => false,
            None => colorfgbg_dark().unwrap_or(true),
        })
}

fn colorfgbg_dark() -> Option<bool> {
    let value = std::env::var("COLORFGBG").ok()?;
    let index: u8 = value.rsplit(';').next()?.parse().ok()?;
    match index {
        0..=6 | 8 => Some(true),
        7 | 9..=15 => Some(false),
        _ => None,
    }
}

#[derive(Clone)]
pub(crate) struct Themes {
    root: PathBuf,
    invocation: Option<String>,
    selected: Selection,
    preview: Option<Selection>,
    colors: TerminalColors,
    theme: Theme,
    file_error: Option<String>,
}

impl Default for Themes {
    fn default() -> Self {
        Self::prepare(Path::new(""), "system", None).expect("builtin theme")
    }
}

impl Themes {
    pub(crate) fn load(root: &Path, invocation: Option<&str>) -> Result<Self> {
        let saved = ion_host::user_settings::UserSettings::load(root)?.theme()?;
        Self::prepare(
            root,
            invocation.or(saved.as_deref()).unwrap_or("system"),
            invocation.map(str::to_owned),
        )
    }
    fn prepare(root: &Path, name: &str, invocation: Option<String>) -> Result<Self> {
        let selected = Selection::load(root, name)?;
        let colors = TerminalColors::default();
        let theme = selected.resolve(colors);
        Ok(Self {
            root: root.into(),
            invocation,
            selected,
            preview: None,
            colors,
            theme,
            file_error: None,
        })
    }
    pub(crate) fn theme(&self) -> &Theme {
        &self.theme
    }
    pub(crate) fn name(&self) -> &str {
        &self.selected.name
    }
    pub(crate) fn is_dark(&self) -> bool {
        terminal_is_dark(self.colors)
    }
    pub(crate) fn watches_file(&self) -> bool {
        self.selected.custom() || self.preview.as_ref().is_some_and(Selection::custom)
    }
    fn resolve(&mut self) {
        self.theme = self
            .preview
            .as_ref()
            .unwrap_or(&self.selected)
            .resolve(self.colors);
    }
    pub(crate) fn observe(&mut self, colors: TerminalColors) {
        if self.colors != colors {
            self.colors = colors;
            self.resolve();
        }
    }
    pub(crate) fn preview(&mut self, name: &str) -> Result<()> {
        let next = Selection::load(&self.root, name)?;
        self.preview = Some(next);
        self.resolve();
        Ok(())
    }
    pub(crate) fn cancel_preview(&mut self) {
        if self.preview.take().is_some() {
            self.resolve();
        }
    }
    pub(crate) fn confirm(&mut self, name: &str) -> Result<Option<String>> {
        let next = Selection::load(&self.root, name)?;
        let warning = ion_host::user_settings::UserSettings::save_theme(&self.root, name)?;
        self.selected = next;
        self.invocation = None;
        self.preview = None;
        self.file_error = None;
        self.resolve();
        Ok(warning)
    }
    pub(crate) fn reloaded(&self) -> Result<Self> {
        let mut next = Self::load(&self.root, self.invocation.as_deref())?;
        next.observe(self.colors);
        Ok(next)
    }
    pub(crate) fn check_files(&mut self) -> Option<String> {
        let current = self.preview.as_ref().unwrap_or(&self.selected);
        if !current.custom() {
            return None;
        }
        match Selection::load(&self.root, &current.name) {
            Ok(next) => {
                let recovered = self.file_error.take().is_some();
                if *current != next {
                    if self.preview.is_some() {
                        self.preview = Some(next);
                    } else {
                        self.selected = next;
                    }
                    self.resolve();
                    return Some("Theme file reloaded · native history unchanged".into());
                }
                recovered.then(|| "Theme file is valid again".into())
            }
            Err(error) => {
                let message = format!("Theme unchanged: {error:#}");
                if self.file_error.as_ref() == Some(&message) {
                    None
                } else {
                    self.file_error = Some(message.clone());
                    Some(message)
                }
            }
        }
    }
    pub(crate) fn names(&self) -> Result<Vec<String>> {
        let mut names = vec!["system".into(), "dark".into(), "light".into()];
        let entries = match std::fs::read_dir(self.root.join("themes")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !names.contains(&self.selected.name) {
                    names.push(self.selected.name.clone());
                }
                return Ok(names);
            }
            Err(error) => return Err(error).context("list user themes"),
        };
        let mut custom = BTreeSet::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .context("theme filename is not UTF-8")?;
            validate_name(name)?;
            ensure!(
                !names.iter().any(|builtin| builtin == name),
                "user theme collides with a builtin name"
            );
            custom.insert(name.to_owned());
            ensure!(
                custom.len() <= 256,
                "user theme catalogue exceeds 256 files"
            );
        }
        names.extend(custom);
        if !names.contains(&self.selected.name) {
            names.push(self.selected.name.clone());
        }
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("ion-themes-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(root.join("themes")).unwrap();
        root
    }

    #[test]
    fn preview_cancel_confirm_and_bad_hot_reload_keep_one_valid_binding() {
        let root = root();
        let path = root.join("themes/test.json");
        std::fs::write(&path, r##"{"name":"test","base":"dark","vars":{"accent":"#f20"},"colors":{"error":"accent"}}"##).unwrap();
        let mut themes = Themes::load(&root, None).unwrap();
        let original = themes.theme().clone();
        themes.preview("test").unwrap();
        assert_eq!(
            themes.theme().style(Role::Error).fg,
            Some(Color::Rgb(255, 34, 0))
        );
        assert_eq!(themes.name(), "system");
        themes.cancel_preview();
        assert_eq!(themes.theme(), &original);
        assert!(!root.join("settings.json").exists());
        themes.confirm("test").unwrap();
        assert_eq!(Themes::load(&root, None).unwrap().name(), "test");
        let good = themes.theme().clone();
        std::fs::write(
            &path,
            r#"{"name":"test","colors":{"unknown":"PRIVATE_INVALID_VALUE"}}"#,
        )
        .unwrap();
        let error = themes.check_files().unwrap();
        assert!(!error.contains("PRIVATE_INVALID_VALUE"));
        assert_eq!(themes.theme(), &good);
        assert!(themes.check_files().is_none());
        std::fs::write(&path, r##"{"name":"test","colors":{"error":"#00ff00"}}"##).unwrap();
        assert!(themes.check_files().is_some());
        assert_eq!(
            themes.theme().style(Role::Error).fg,
            Some(Color::Rgb(0, 255, 0))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn strict_variables_paths_and_appearance_pairs() {
        let root = root();
        for json in [
            r#"{"name":"test","colors":{"error":256}}"#,
            r#"{"name":"test","vars":{"a":"b","b":"a"},"colors":{}}"#,
            r#"{"name":"test","colors":{"error":"missing"}}"#,
            r#"{"name":"different","colors":{}}"#,
            r#"{"name":"test","colors":{},"extra":true}"#,
        ] {
            std::fs::write(root.join("themes/test.json"), json).unwrap();
            assert!(Selection::load(&root, "test").is_err(), "{json}");
        }
        for name in [
            "../secret",
            "/dark",
            "dark/",
            "dark/light/system",
            "bad\\name",
        ] {
            assert!(Selection::load(&root, name).is_err());
        }
        let mut themes = Themes::load(&root, Some("light/dark")).unwrap();
        themes.observe(TerminalColors {
            appearance: Some(Appearance::Light),
            ..Default::default()
        });
        assert_eq!(themes.theme(), &Theme::fixed(false));
        themes.observe(TerminalColors {
            background: Some([0; 3]),
            appearance: Some(Appearance::Light),
            ..Default::default()
        });
        assert_eq!(themes.theme(), &Theme::fixed(true));
        std::fs::remove_dir_all(root).unwrap();
    }
}
