//! A palette: the colour of every role, derived from a theme's few base
//! colours unless the theme names the role itself.

use ratatui::style::Color;
use serde::Deserialize;

/// A 24-bit colour, as a theme file writes it: `"#89b4fa"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn parse(text: &str) -> Option<Rgb> {
        let hex = text.strip_prefix('#')?;
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Rgb(byte(0)?, byte(2)?, byte(4)?))
    }

    /// `self` moved `amount` of the way towards `other`: `0.0` is `self`,
    /// `1.0` is `other`.
    pub fn mix(self, other: Rgb, amount: f32) -> Rgb {
        let channel = |a: u8, b: u8| {
            let t = amount.clamp(0.0, 1.0);
            (a as f32 + (b as f32 - a as f32) * t).round() as u8
        };
        Rgb(
            channel(self.0, other.0),
            channel(self.1, other.1),
            channel(self.2, other.2),
        )
    }

    pub fn color(self) -> Color {
        Color::Rgb(self.0, self.1, self.2)
    }

    /// Relative luminance, as WCAG defines it: `0.0` is black, `1.0` white.
    fn luminance(self) -> f32 {
        let linear = |c: u8| {
            let v = c as f32 / 255.0;
            if v <= 0.039_28 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.0) + 0.7152 * linear(self.1) + 0.0722 * linear(self.2)
    }

    /// The WCAG contrast ratio between two colours, from `1.0` (the same)
    /// to `21.0` (black on white).
    pub fn contrast(self, other: Rgb) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let text = String::deserialize(de)?;
        Rgb::parse(&text).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "`{text}` is not a colour — write it as \"#rrggbb\""
            ))
        })
    }
}

/// How far each derived role sits between the background (`0.0`) and the
/// foreground (`1.0`). One table, so every theme's greys stand in the same
/// relation to its own background and text.
const TEXT_DIM: f32 = 0.55;
const TEXT_MUTED: f32 = 0.42;

/// The least contrast a derived text colour is allowed, whatever the
/// theme: a theme whose own text is soft would otherwise hand its softness
/// on to every grey mixed from it. `TEXT_DIM` against the background is
/// WCAG's floor for text; `TEXT_MUTED` against the surface is its floor
/// for large or incidental text, which labels and hints are.
const TEXT_DIM_CONTRAST: f32 = 4.5;
const TEXT_MUTED_CONTRAST: f32 = 3.0;
const SURFACE: f32 = 0.06;
const HIGHLIGHT: f32 = 0.13;
const BORDER: f32 = 0.22;
/// A search match is the theme's blue, washed into the background far
/// enough that the text on it stays readable.
const SEARCH_MATCH: f32 = 0.35;
/// Namespaced, when a theme does not name it: this share of the way from
/// its magenta to its blue. Nearer magenta, so it reads as isolated's
/// sibling — both are data of the worktree's own — and never as the
/// cursor's blue.
const NAMESPACED: f32 = 0.4;
/// And it is text, a word in a column, so it is held to the floor the
/// dimmer text is.
const NAMESPACED_CONTRAST: f32 = TEXT_DIM_CONTRAST;

/// One appearance of a theme, as its file writes it: the base colours every
/// theme has, and any role it would rather set than have derived.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub background: Rgb,
    pub foreground: Rgb,
    pub red: Rgb,
    pub green: Rgb,
    pub yellow: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    pub cyan: Rgb,
    pub orange: Rgb,
    /// The eighth accent, which a theme may leave to be mixed from two of
    /// the others.
    #[serde(default)]
    pub namespaced: Option<Rgb>,
    #[serde(default)]
    pub text_dim: Option<Rgb>,
    #[serde(default)]
    pub text_muted: Option<Rgb>,
    #[serde(default)]
    pub surface: Option<Rgb>,
    #[serde(default)]
    pub highlight: Option<Rgb>,
    #[serde(default)]
    pub border: Option<Rgb>,
    #[serde(default)]
    pub search_match: Option<Rgb>,
    #[serde(default)]
    pub search_cursor: Option<Rgb>,
}

/// Every colour the interface paints with, by what it means. What each
/// role is for is the table in the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
    pub orange: Color,
    /// Orange, one step off it and off every other colour text is drawn
    /// in: a key a hint names, which the TUI reads off a painted frame to
    /// know what a click presses. No eye tells it from orange.
    pub hint_key: Color,
    pub namespaced: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_muted: Color,
    pub surface: Color,
    pub highlight_bg: Color,
    pub border: Color,
    pub search_cursor_bg: Color,
    pub search_match_bg: Color,
}

impl Variant {
    /// The palette this variant paints: its own colours where it gives
    /// them, and the rest mixed from its background and foreground.
    pub fn palette(&self) -> Palette {
        let (bg, fg) = (self.background, self.foreground);
        let between = |set: Option<Rgb>, amount: f32| set.unwrap_or(bg.mix(fg, amount)).color();
        let surface = self.surface.unwrap_or(bg.mix(fg, SURFACE));
        // A text grey starts at its share of the way to the foreground and
        // moves on towards it until it reads against what it sits on.
        let readable = |set: Option<Rgb>, amount: f32, on: Rgb, floor: f32| {
            set.unwrap_or_else(|| {
                let mut t = amount;
                while t < 1.0 && bg.mix(fg, t).contrast(on) < floor {
                    t += 0.02;
                }
                bg.mix(fg, t)
            })
            .color()
        };
        // Between magenta and blue, then lighter on a dark background, or
        // darker on a light one, until it reads — the way a grey is held to
        // its floor, but towards white or black rather than the text, so
        // the hue survives a theme whose text is itself a colour.
        let namespaced = self.namespaced.unwrap_or_else(|| {
            let hue = self.magenta.mix(self.blue, NAMESPACED);
            let away = match bg.luminance() < 0.5 {
                true => Rgb(255, 255, 255),
                false => Rgb(0, 0, 0),
            };
            let mut t = 0.0;
            while t < 1.0 && hue.mix(away, t).contrast(bg) < NAMESPACED_CONTRAST {
                t += 0.02;
            }
            hue.mix(away, t)
        });
        let mut palette = Palette {
            red: self.red.color(),
            green: self.green.color(),
            yellow: self.yellow.color(),
            blue: self.blue.color(),
            magenta: self.magenta.color(),
            cyan: self.cyan.color(),
            orange: self.orange.color(),
            hint_key: self.orange.color(),
            namespaced: namespaced.color(),
            text: fg.color(),
            text_dim: readable(self.text_dim, TEXT_DIM, bg, TEXT_DIM_CONTRAST),
            text_muted: readable(self.text_muted, TEXT_MUTED, surface, TEXT_MUTED_CONTRAST),
            surface: surface.color(),
            highlight_bg: between(self.highlight, HIGHLIGHT),
            border: between(self.border, BORDER),
            search_cursor_bg: self.search_cursor.unwrap_or(self.orange).color(),
            search_match_bg: self
                .search_match
                .unwrap_or(bg.mix(self.blue, SEARCH_MATCH))
                .color(),
        };
        // Two themes give orange another accent's value — colorblind's is
        // its red, Rosé Pine's its cyan — and a theme file may too.
        palette.hint_key = apart(self.orange, &palette.text_colours()).color();
        palette
    }
}

impl Palette {
    /// Every colour text is drawn in but the key's own.
    pub(super) fn text_colours(&self) -> [Color; 12] {
        [
            self.red,
            self.green,
            self.yellow,
            self.blue,
            self.magenta,
            self.cyan,
            self.orange,
            self.namespaced,
            self.text,
            self.text_dim,
            self.text_muted,
            self.border,
        ]
    }
}

/// The colour nearest `colour` that is none of `taken`, a step at a time
/// on its blue channel: a step no eye sees, and one a frame's cells tell
/// apart.
fn apart(colour: Rgb, taken: &[Color]) -> Rgb {
    let Rgb(r, g, b) = colour;
    (0..=u8::MAX)
        .flat_map(|step| [b.checked_add(step), b.checked_sub(step)])
        .flatten()
        .map(|b| Rgb(r, g, b))
        .find(|candidate| !taken.contains(&candidate.color()))
        .unwrap_or(colour)
}
