use palette::{FromColor, Lab, Srgb};
use ratatui::style::Color;
use std::env;

type Rgb = [u8; 3];

// Codex's default syntax themes are Catppuccin Mocha and Latte. These are the
// standalone foreground accents from https://github.com/catppuccin/bat, in theme
// order, excluding the base text, background, comment, and punctuation colors.
// The Catppuccin license is in LICENSES/Catppuccin-MIT.txt.
const MOCHA: [u32; 15] = [
    0xa6e3a1, 0xf5c2e7, 0xfab387, 0xcba6f7, 0x94e2d5, 0x89b4fa, 0xf9e2af, 0xeba0ac, 0xf38ba8,
    0x89dceb, 0xf2cdcd, 0x74c7ec, 0xb4befe, 0xa6adc8, 0xf5e0dc,
];
const LATTE: [u32; 15] = [
    0x40a02b, 0xea76cb, 0xfe640b, 0x8839ef, 0x179299, 0x1e66f5, 0xdf8e1d, 0xe64553, 0xd20f39,
    0x04a5e5, 0xdd7878, 0x209fb5, 0x7287fd, 0x6c6f85, 0xdc8a78,
];
const MIN_CONTRAST: f64 = 4.5;

pub(crate) struct ProfilePalette(Vec<Color>);

impl ProfilePalette {
    /// Call in raw mode before reading crossterm events so the query owns input.
    pub fn detect() -> Self {
        let term = env::var("TERM").unwrap_or_default();
        let truecolor = term.ends_with("-direct")
            || env::var("COLORTERM")
                .is_ok_and(|color| matches!(color.as_str(), "truecolor" | "24bit"));
        if !truecolor && !term.ends_with("-256color") {
            return Self(vec![
                Color::Blue,
                Color::Cyan,
                Color::Green,
                Color::Magenta,
                Color::Red,
                Color::Yellow,
            ]);
        }
        let background = xterm_query::query_osc("\x1b]11;?\x07", 80_u16)
            .ok()
            .and_then(|reply| parse_background(&reply));
        Self::themed(truecolor, background)
    }

    fn themed(truecolor: bool, background: Option<Rgb>) -> Self {
        let accents = if background.is_some_and(|[r, g, b]| {
            0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b) > 128.0
        }) {
            LATTE
        } else {
            MOCHA
        };
        Self(
            accents
                .into_iter()
                .map(|accent| {
                    let [_, r, g, b] = accent.to_be_bytes();
                    let preferred = [r, g, b];
                    if truecolor {
                        let [r, g, b] =
                            background.map_or(preferred, |bg| readable_rgb(preferred, bg));
                        Color::Rgb(r, g, b)
                    } else {
                        indexed_accent(preferred, background)
                    }
                })
                .collect(),
        )
    }

    pub fn color(&self, name: &str) -> Color {
        // Like Codex's thread colors, the assignment stays stable as the list changes.
        let hash = name.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
        self.0[(hash % self.0.len() as u64) as usize]
    }
}

fn parse_background(reply: &str) -> Option<Rgb> {
    let mut channels = reply
        .strip_prefix("]11;rgb:")?
        .trim_end_matches(['\x07', '\x1b'])
        .split('/');
    let mut channel = || {
        let hex = channels.next()?;
        if !(1..=4).contains(&hex.len()) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(hex, 16).ok()?;
        let max = (1 << (4 * hex.len())) - 1;
        Some((value * 255 / max) as u8)
    };
    let rgb = [channel()?, channel()?, channel()?];
    channels.next().is_none().then_some(rgb)
}

fn relative_luminance([r, g, b]: Rgb) -> f64 {
    let rgb = Srgb::new(r, g, b).into_format::<f64>().into_linear();
    0.2126 * rgb.red + 0.7152 * rgb.green + 0.0722 * rgb.blue
}

fn contrast(rgb: Rgb, background_luminance: f64) -> f64 {
    let foreground = relative_luminance(rgb);
    (foreground.max(background_luminance) + 0.05) / (foreground.min(background_luminance) + 0.05)
}

fn readable_rgb(preferred: Rgb, background: Rgb) -> Rgb {
    let luminance = relative_luminance(background);
    let black = [0; 3];
    let white = [255; 3];
    let endpoint = if contrast(black, luminance) >= contrast(white, luminance) {
        black
    } else {
        white
    };
    // Keep the first shade that meets the contrast target as we move from the accent
    // toward the more readable endpoint. This keeps as much of the original hue as possible.
    (0..=255_u16)
        .map(|step| {
            std::array::from_fn(|i| {
                ((u16::from(preferred[i]) * (255 - step) + u16::from(endpoint[i]) * step) / 255)
                    as u8
            })
        })
        .find(|&shade| contrast(shade, luminance) >= MIN_CONTRAST)
        .unwrap_or(endpoint)
}

fn indexed_accent(preferred: Rgb, background: Option<Rgb>) -> Color {
    fn lab([r, g, b]: Rgb) -> Lab {
        Lab::from_color(Srgb::new(r, g, b).into_format::<f32>())
    }
    let preferred = lab(preferred);
    let luminance = background.map(relative_luminance);
    // The first 16 entries are terminal-defined. The fixed xterm cube and gray
    // ramp let us check contrast against the color that will actually be drawn.
    (16..=255)
        .filter_map(|index| {
            let rgb = xterm_rgb(index);
            if luminance.is_some_and(|bg| contrast(rgb, bg) < MIN_CONTRAST) {
                return None;
            }
            let candidate = lab(rgb);
            let distance = (preferred.l - candidate.l).powi(2)
                + (preferred.a - candidate.a).powi(2)
                + (preferred.b - candidate.b).powi(2);
            Some((distance, index))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map_or(Color::Reset, |(_, index)| Color::Indexed(index))
}

fn xterm_rgb(index: u8) -> Rgb {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    if index >= 232 {
        return [8 + 10 * (index - 232); 3];
    }
    let entry = usize::from(index - 16);
    [LEVELS[entry / 36], LEVELS[entry / 6 % 6], LEVELS[entry % 6]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_and_dark_accents_survive_terminal_color_quantization() {
        for background in [[30, 30, 46], [239, 241, 245], [120, 120, 120]] {
            for truecolor in [false, true] {
                let palette = ProfilePalette::themed(truecolor, Some(background));
                for color in palette.0 {
                    let rgb = match color {
                        Color::Rgb(r, g, b) => [r, g, b],
                        Color::Indexed(index) => xterm_rgb(index),
                        _ => panic!("expected a readable accent for {background:?}"),
                    };
                    assert!(contrast(rgb, relative_luminance(background)) >= MIN_CONTRAST);
                }
            }
        }
    }

    #[test]
    fn background_reply_handles_varied_channel_precision() {
        assert_eq!(
            parse_background("]11;rgb:efef/f1f1/f5f5\x1b"),
            Some([239, 241, 245])
        );
        assert_eq!(
            parse_background("]11;rgb:00/80/ff\x07"),
            Some([0, 128, 255])
        );
        assert_eq!(parse_background("]11;rgb:00/☃/ff\x07"), None);
    }
}
