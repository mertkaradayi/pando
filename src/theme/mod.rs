//! Colour themes: the palette the interface paints with, which theme it
//! comes from, and whether its dark or light half.
//!
//! A theme is data — a TOML file in `theme/builtin/`, or the developer's
//! own under `<pando home>/themes/`, which replaces a built-in of the same
//! name. It gives a background, a foreground and seven accents for each
//! appearance, and every other colour is mixed from those
//! ([`palette`](self::palette)) — an eighth accent, `namespaced`, included
//! — so a new theme is nine lines a side.
//!
//! Which one applies: `PANDO_THEME`, then the file `[ui] theme_from`
//! names (a terminal theme switcher's state file, followed while pando
//! runs), then `[ui] theme`, then `pando`. Dark or light: `PANDO_APPEARANCE`,
//! then `[ui] appearance`, then the system.
//!
//! Each colour means one thing, everywhere, so a colour learnt in one place
//! reads the same in the next. A new use picks the row it belongs to:
//!
//! | colour       | means                                              |
//! |--------------|----------------------------------------------------|
//! | `green`      | up, working, done: running, a success, a public    |
//! |              | URL, an open pull request                          |
//! | `yellow`     | in flight, or needs a look: starting, uncommitted  |
//! | `red`        | failed, down, destructive                          |
//! | `cyan`       | an address: a URL, a port                          |
//! | `magenta`    | isolated: private copies of the services; a merged |
//! |              | pull request, as GitHub colours it                 |
//! | `namespaced` | namespaced: a namespace of the worktree's own in   |
//! |              | the project's servers. Mixed from magenta and blue |
//! |              | when a theme does not name it                      |
//! | `blue`       | where you are: the cursor, the checked-out branch  |
//! | `orange`     | a key to press                                     |
//! | `hint_key`   | a key a hint names, which a click presses: orange  |
//! |              | to the eye, one step off every other colour to the |
//! |              | TUI, which finds hints by it. Never set by a theme |
//! | `text`       | what the row is about: a branch, a title           |
//! | `text_dim`   | a value that is there but not news                 |
//! | `text_muted` | labels, titles, hints: read once, then skipped     |
//! | `border`     | lines: borders and the grid                        |
//!
//! Colour never carries a meaning alone: a glyph or a word says it too,
//! for whoever cannot tell two of them apart.
//!
//! Log lines are coloured as they are read, so after a switch the lines
//! already on screen keep the old theme's colours until they are read
//! again.

mod catalog;
mod palette;
mod select;

pub use catalog::{BUILT_IN, DEFAULT_THEME, Source, Theme, find, themes};
pub use palette::{Palette, Rgb, Variant};
pub use select::{
    APPEARANCE_ENV, Appearance, AppearanceOrigin, Origin, Settings, THEME_ENV, appearance, choose,
};

use ratatui::style::Color;
use std::path::Path;

/// The palette in force. Process-wide in a build; per thread under test,
/// so a test that switches theme does not repaint its neighbours.
#[cfg(not(test))]
static ACTIVE: std::sync::RwLock<Option<Palette>> = std::sync::RwLock::new(None);

#[cfg(test)]
thread_local! {
    static ACTIVE: std::cell::Cell<Option<Palette>> = const { std::cell::Cell::new(None) };
}

#[cfg(not(test))]
fn active() -> Option<Palette> {
    *ACTIVE.read().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
fn active() -> Option<Palette> {
    ACTIVE.with(|a| a.get())
}

/// Makes `palette` the one every colour below returns.
#[cfg(not(test))]
pub fn set_palette(palette: Palette) {
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = Some(palette);
}

#[cfg(test)]
pub fn set_palette(palette: Palette) {
    ACTIVE.with(|a| a.set(Some(palette)));
}

/// What colours before anything chose a theme: the default theme's dark
/// half.
///
/// Nobody sees it, so the system is not asked which half: a CLI verb
/// prints its log lines as plain text, and the TUI chooses its theme
/// before its first frame and reads its tails again when it does. Asking
/// was a `defaults` run on macOS for every `pando logs`, and for every
/// `status` or `doctor` that read a failure's log.
fn fallback() -> Palette {
    static FALLBACK: std::sync::OnceLock<Palette> = std::sync::OnceLock::new();
    *FALLBACK.get_or_init(|| {
        let (name, text) = BUILT_IN
            .iter()
            .find(|(name, _)| *name == DEFAULT_THEME)
            .expect("the default theme is built in");
        Theme::parse(name, text, Source::BuiltIn)
            .unwrap_or_else(|e| panic!("built-in theme {name} does not parse: {e}"))
            .palette(Appearance::Dark)
    })
}

pub fn palette() -> Palette {
    active().unwrap_or_else(fallback)
}

/// A theme resolved for this run: which one, why, dark or light and why,
/// and what could not be honoured along the way.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub name: String,
    pub origin: Origin,
    pub appearance: Appearance,
    pub appearance_origin: AppearanceOrigin,
    pub palette: Palette,
    pub warnings: Vec<String>,
}

/// Picks the theme `settings` and the environment ask for, from the
/// built-ins and `themes_dir`, falling back to the default with a warning
/// when it names one that does not exist. Asks the system for its
/// appearance, so it blocks for a moment: startup or a watcher only.
pub fn resolve(settings: &Settings, themes_dir: Option<&Path>) -> Resolved {
    let (all, mut warnings) = themes(themes_dir);
    let (name, origin) = choose(settings);
    let (appearance, appearance_origin) = appearance(settings);
    let theme = match find(&all, &name) {
        Some(theme) => theme,
        None => {
            warnings.push(format!(
                "no theme called {name} — using {DEFAULT_THEME}; T lists them"
            ));
            find(&all, DEFAULT_THEME).expect("the default theme is built in")
        }
    };
    Resolved {
        name: theme.name.clone(),
        origin,
        appearance,
        appearance_origin,
        palette: theme.palette(appearance),
        warnings,
    }
}

pub fn blue() -> Color {
    palette().blue
}

pub fn orange() -> Color {
    palette().orange
}

/// The colour a hint draws its key in: orange, as the eye sees it, and a
/// colour of its own to the TUI, which reads hints off a painted frame.
pub fn hint_key() -> Color {
    palette().hint_key
}

pub fn magenta() -> Color {
    palette().magenta
}

pub fn namespaced() -> Color {
    palette().namespaced
}

pub fn green() -> Color {
    palette().green
}

pub fn red() -> Color {
    palette().red
}

pub fn yellow() -> Color {
    palette().yellow
}

pub fn cyan() -> Color {
    palette().cyan
}

pub fn text() -> Color {
    palette().text
}

pub fn text_dim() -> Color {
    palette().text_dim
}

/// The faintest text, for labels and key hints — still text, so it has to
/// stay readable on `surface()`, not only on the terminal's background.
pub fn text_muted() -> Color {
    palette().text_muted
}

pub fn surface() -> Color {
    palette().surface
}

pub fn highlight_bg() -> Color {
    palette().highlight_bg
}

pub fn border() -> Color {
    palette().border
}

pub fn search_cursor_bg() -> Color {
    palette().search_cursor_bg
}

pub fn search_match_bg() -> Color {
    palette().search_match_bg
}

#[cfg(test)]
mod tests;
