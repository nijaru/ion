/// A terminal's reported appearance, independent of presentation policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

/// Observed terminal defaults and ANSI palette entries. Missing replies remain
/// unknown; this substrate neither infers an appearance nor supplies fallbacks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalColors {
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
    pub palette: [Option<[u8; 3]>; 16],
    pub appearance: Option<Appearance>,
}

pub(crate) const QUERY: &[u8] = concat!(
    "\x1b]10;?\x07",
    "\x1b]11;?\x07",
    "\x1b]4;0;?\x07",
    "\x1b]4;1;?\x07",
    "\x1b]4;2;?\x07",
    "\x1b]4;3;?\x07",
    "\x1b]4;4;?\x07",
    "\x1b]4;5;?\x07",
    "\x1b]4;6;?\x07",
    "\x1b]4;7;?\x07",
    "\x1b]4;8;?\x07",
    "\x1b]4;9;?\x07",
    "\x1b]4;10;?\x07",
    "\x1b]4;11;?\x07",
    "\x1b]4;12;?\x07",
    "\x1b]4;13;?\x07",
    "\x1b]4;14;?\x07",
    "\x1b]4;15;?\x07",
)
.as_bytes();
pub(crate) const MODE_QUERY: &[u8] = b"\x1b[?2031$p";

/// Parse an xterm `rgb:` specification, scaling each 1–4 digit component to
/// eight bits. Round to nearest, as in Pi's terminal color observation.
fn rgb(value: &str) -> Option<[u8; 3]> {
    let mut parts = value.strip_prefix("rgb:")?.split('/');
    let mut color = [0; 3];
    for component in &mut color {
        let part = parts.next()?;
        if !(1..=4).contains(&part.len()) || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(part, 16).ok()?;
        let maximum = (1u32 << (part.len() * 4)) - 1;
        *component = u8::try_from((value * 255 + maximum / 2) / maximum).ok()?;
    }
    parts.next().is_none().then_some(color)
}

/// Apply a whole OSC body atomically. OSC 4 may contain several index/color
/// pairs, including indices outside the sixteen observed entries.
pub(crate) fn observe_osc(colors: &mut TerminalColors, body: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(body).ok()?;
    let (command, values) = text.split_once(';')?;
    let mut next = *colors;
    let mask = match command {
        "10" => {
            next.foreground = Some(rgb(values)?);
            1 << 16
        }
        "11" => {
            next.background = Some(rgb(values)?);
            1 << 17
        }
        "4" => {
            let mut pairs = values.split(';');
            let mut mask = 0;
            while let Some(index) = pairs.next() {
                if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let index = index.parse::<u8>().ok()?;
                let color = rgb(pairs.next()?)?;
                if index < 16 {
                    next.palette[usize::from(index)] = Some(color);
                    mask |= 1 << index;
                }
            }
            mask
        }
        _ => return None,
    };
    *colors = next;
    Some(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_scales_each_component_width_and_rejects_invalid_specs() {
        for value in [
            "rgb:4/8/f",
            "rgb:44/88/ff",
            "rgb:444/888/fff",
            "rgb:4444/8888/ffff",
        ] {
            assert_eq!(rgb(value), Some([68, 136, 255]), "{value}");
        }
        assert_eq!(rgb("rgb:abc/12/8000"), Some([171, 18, 128]));
        for value in [
            "rgb:/0/0",
            "rgb:fffff/0/0",
            "rgb:+1/0/0",
            "rgb:gg/0/0",
            "rgb:0/0",
            "rgb:0/0/0/0",
        ] {
            assert_eq!(rgb(value), None, "{value}");
        }
    }

    #[test]
    fn multi_pair_palette_is_atomic_and_ignores_unobserved_indices() {
        let mut colors = TerminalColors::default();
        assert_eq!(
            observe_osc(&mut colors, b"4;0;rgb:f/0/0;15;rgb:0/f/0;255;rgb:0/0/f"),
            Some(0x8001)
        );
        assert_eq!(colors.palette[0], Some([255, 0, 0]));
        assert_eq!(colors.palette[15], Some([0, 255, 0]));
        let before = colors;
        for body in [
            b"4;0;rgb:0/0/0;1;invalid".as_slice(),
            b"4;0;rgb:0/0/0;1",
            b"4;256;rgb:0/0/0",
            b"10;rgb:0/0/0;extra",
        ] {
            assert_eq!(observe_osc(&mut colors, body), None);
            assert_eq!(colors, before);
        }
    }
}
