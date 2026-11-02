//! Immutable semantic styles. Literal observations and native programs are not themed.
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
#[repr(usize)]
pub(crate) enum Role {
    Heading,
    Secondary,
    UserInput,
    Error,
    Warning,
    Running,
    Code,
    SyntaxKeyword,
    SyntaxString,
    SyntaxNumber,
    SyntaxFunction,
    SyntaxType,
    SyntaxComment,
    Link,
    Added,
    Removed,
    Hunk,
    Text,
    Selected,
}

const ROLES: [Role; 19] = [
    Role::Heading,
    Role::Secondary,
    Role::UserInput,
    Role::Error,
    Role::Warning,
    Role::Running,
    Role::Code,
    Role::SyntaxKeyword,
    Role::SyntaxString,
    Role::SyntaxNumber,
    Role::SyntaxFunction,
    Role::SyntaxType,
    Role::SyntaxComment,
    Role::Link,
    Role::Added,
    Role::Removed,
    Role::Hunk,
    Role::Text,
    Role::Selected,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Theme {
    styles: [Style; ROLES.len()],
}

impl Default for Theme {
    fn default() -> Self {
        Self::system(None, None, &[None; 16])
    }
}

impl Theme {
    pub(crate) fn style(&self, role: Role) -> Style {
        self.styles[role as usize]
    }

    pub(crate) fn set_color(&mut self, role: Role, color: Color) {
        self.styles[role as usize].fg = Some(color);
    }

    pub(crate) fn fixed(dark: bool) -> Self {
        let colors = if dark {
            [
                [216, 222, 233],
                [191, 97, 106],
                [163, 190, 140],
                [235, 203, 139],
                [129, 161, 193],
                [180, 142, 173],
                [136, 192, 208],
                [216, 222, 233],
            ]
        } else {
            [
                [37, 42, 53],
                [166, 33, 58],
                [39, 104, 45],
                [127, 86, 0],
                [38, 77, 161],
                [119, 55, 141],
                [0, 100, 119],
                [37, 42, 53],
            ]
        };
        Self::build(|role| {
            let [r, g, b] = colors[slot(role).unwrap_or(0)];
            Color::Rgb(r, g, b)
        })
    }

    /// Use observed terminal hues, correcting text contrast only when its
    /// background is known. With no answer, let the terminal render ANSI/default
    /// colors itself; faint is deliberately not used as a readability fallback.
    pub(crate) fn system(
        foreground: Option<[u8; 3]>,
        background: Option<[u8; 3]>,
        palette: &[Option<[u8; 3]>; 16],
    ) -> Self {
        let fallback = Self::fixed(background.is_none_or(|bg| is_dark(bg, foreground)));
        Self::build(|role| {
            let source = slot(role)
                .and_then(|slot| palette[slot])
                .or_else(|| slot(role).is_none().then_some(foreground).flatten());
            match (source, background) {
                (Some(source), Some(bg)) => {
                    let [r, g, b] = readable(source, bg);
                    Color::Rgb(r, g, b)
                }
                (None, Some(bg)) => {
                    let Color::Rgb(r, g, b) = fallback.style(role).fg.expect("fixed foreground")
                    else {
                        unreachable!()
                    };
                    let [r, g, b] = readable([r, g, b], bg);
                    Color::Rgb(r, g, b)
                }
                (_, None) => slot(role).map_or(Color::Reset, |slot| Color::Indexed(slot as u8)),
            }
        })
    }

    fn build(mut color: impl FnMut(Role) -> Color) -> Self {
        let mut styles = [Style::default(); ROLES.len()];
        for role in ROLES {
            let modifier = match role {
                Role::Heading
                | Role::Error
                | Role::Warning
                | Role::Running
                | Role::SyntaxKeyword => Modifier::BOLD,
                Role::SyntaxComment => Modifier::ITALIC,
                Role::Selected => Modifier::REVERSED,
                _ => Modifier::empty(),
            };
            styles[role as usize] = Style::default().fg(color(role)).add_modifier(modifier);
        }
        Self { styles }
    }
}

fn slot(role: Role) -> Option<usize> {
    match role {
        Role::UserInput | Role::SyntaxNumber => Some(4),
        Role::Error | Role::Removed => Some(1),
        Role::Warning | Role::SyntaxType => Some(3),
        Role::Running | Role::Link | Role::Hunk | Role::SyntaxFunction => Some(6),
        Role::Code | Role::SyntaxKeyword => Some(5),
        Role::SyntaxString | Role::Added => Some(2),
        _ => None,
    }
}

pub(crate) fn is_dark(background: [u8; 3], foreground: Option<[u8; 3]>) -> bool {
    if let Some(fg) = foreground {
        let lighter = luminance(fg) > luminance(background);
        let extreme = if lighter { [255; 3] } else { [0; 3] };
        if contrast(extreme, background) >= 4.5 {
            return lighter;
        }
    }
    contrast([255; 3], background) >= contrast([0; 3], background)
}

fn luminance(rgb: [u8; 3]) -> f64 {
    let linear = rgb.map(|component| {
        let c = f64::from(component) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722
}

fn contrast(first: [u8; 3], second: [u8; 3]) -> f64 {
    let a = luminance(first);
    let b = luminance(second);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn readable(source: [u8; 3], background: [u8; 3]) -> [u8; 3] {
    if contrast(source, background) >= 4.5 {
        return source;
    }
    let extreme = if contrast([255; 3], background) >= contrast([0; 3], background) {
        255.0
    } else {
        0.0
    };
    let blend = |amount: f64| {
        source.map(|c| (f64::from(c) + (extreme - f64::from(c)) * amount).round() as u8)
    };
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..24 {
        let middle = (low + high) / 2.0;
        if contrast(blend(middle), background) >= 4.5 {
            high = middle;
        } else {
            low = middle;
        }
    }
    blend(high)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_backgrounds_keep_all_semantic_text_readable() {
        // Independent WCAG endpoints: black/white 21:1; identical colors 1:1.
        assert!((contrast([0; 3], [255; 3]) - 21.0).abs() < 1e-10);
        assert_eq!(contrast([127; 3], [127; 3]), 1.0);
        for background in [[0; 3], [255; 3], [119; 3], [80, 90, 100], [224, 208, 180]] {
            let theme = Theme::system(Some(background), Some(background), &[Some(background); 16]);
            for role in ROLES {
                let Color::Rgb(r, g, b) = theme.style(role).fg.unwrap() else {
                    panic!("observed RGB expected")
                };
                assert!(
                    contrast([r, g, b], background) >= 4.5,
                    "{role:?} {background:?}"
                );
            }
        }
    }

    #[test]
    fn system_preserves_readable_terminal_colors_and_safe_unknown_fallback() {
        let mut palette = [None; 16];
        palette[1] = Some([255, 150, 160]);
        let theme = Theme::system(Some([245; 3]), Some([16; 3]), &palette);
        assert_eq!(theme.style(Role::Error).fg, Some(Color::Rgb(255, 150, 160)));
        assert_eq!(theme.style(Role::Text).fg, Some(Color::Rgb(245, 245, 245)));
        let fallback = Theme::default();
        assert_eq!(fallback.style(Role::Secondary).fg, Some(Color::Reset));
        assert!(
            !fallback
                .style(Role::Secondary)
                .add_modifier
                .contains(Modifier::DIM)
        );
    }
}
