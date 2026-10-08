//! The colour theme: the one in force, the picker `T` opens, and saving a
//! choice to the user layer.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::sync::{Arc, Mutex};

use crate::theme::{
    self, Appearance, AppearanceOrigin, Origin, Palette, Resolved, Settings, Theme,
};

use super::dialogs::Modal;
use super::{App, LogTails};

/// Which theme paints the screen, and what chose it.
pub struct ThemeState {
    /// Every theme there is, built-ins first, read once at startup.
    pub themes: Vec<Theme>,
    pub name: String,
    pub origin: Origin,
    pub appearance: Appearance,
    /// What decided the appearance: the system, or what pins it.
    pub appearance_origin: AppearanceOrigin,
    /// What the config says, shared with the watcher that follows it: a
    /// choice saved here is the one the watcher compares against next.
    pub settings: Arc<Mutex<Settings>>,
}

impl ThemeState {
    pub fn new(settings: Settings, themes_dir: &std::path::Path) -> Self {
        let (themes, _) = theme::themes(Some(themes_dir));
        ThemeState {
            themes,
            name: theme::DEFAULT_THEME.to_string(),
            origin: Origin::Default,
            appearance: Appearance::Dark,
            appearance_origin: AppearanceOrigin::System,
            settings: Arc::new(Mutex::new(settings)),
        }
    }
}

impl App {
    /// Puts a resolved theme on screen: the palette, and the log tails read
    /// again so their lines are coloured by it rather than the last one.
    pub fn adopt_theme(&mut self, resolved: Resolved) {
        theme::set_palette(resolved.palette);
        self.log_tails = LogTails::default();
        self.theme.name = resolved.name;
        self.theme.origin = resolved.origin;
        self.theme.appearance = resolved.appearance;
        self.theme.appearance_origin = resolved.appearance_origin;
        for warning in resolved.warnings {
            self.set_error(warning);
        }
    }

    /// `T`: every theme, the one in force under the cursor.
    pub(super) fn open_theme_picker(&mut self) {
        let selected = self
            .theme
            .themes
            .iter()
            .position(|t| t.name == self.theme.name)
            .unwrap_or(0);
        self.modal = Some(Modal::Theme {
            selected,
            before: theme::palette(),
        });
    }

    /// Moving previews the theme under the cursor on the whole screen;
    /// enter keeps it, esc puts back what was there.
    pub(super) fn handle_theme_key(&mut self, key: KeyEvent, selected: usize, before: Palette) {
        let last = self.theme.themes.len().saturating_sub(1);
        let moved = match key.code {
            KeyCode::Down | KeyCode::Char('j') => Some((selected + 1).min(last)),
            KeyCode::Up | KeyCode::Char('k') => Some(selected.saturating_sub(1)),
            KeyCode::Char('g') | KeyCode::Home => Some(0),
            KeyCode::Char('G') | KeyCode::End => Some(last),
            _ => None,
        };
        if let Some(selected) = moved {
            if let Some(theme) = self.theme.themes.get(selected) {
                theme::set_palette(theme.palette(self.theme.appearance));
            }
            self.modal = Some(Modal::Theme { selected, before });
            return;
        }
        match key.code {
            KeyCode::Enter => self.choose_theme(selected),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('T') => theme::set_palette(before),
            _ => self.modal = Some(Modal::Theme { selected, before }),
        }
    }

    fn choose_theme(&mut self, selected: usize) {
        let Some(chosen) = self.theme.themes.get(selected).cloned() else {
            return;
        };
        theme::set_palette(chosen.palette(self.theme.appearance));
        self.log_tails = LogTails::default();
        self.theme.name = chosen.name.clone();
        if let Ok(mut settings) = self.theme.settings.lock() {
            settings.theme = Some(chosen.name.clone());
        }
        // What would take it back at the next start, said now rather than
        // discovered then.
        let overridden = match &self.theme.origin {
            Origin::Env => Some(format!("{} is set, and wins", theme::THEME_ENV)),
            Origin::File(path) => Some(format!(
                "[ui] theme_from names {}, which wins while it names a theme",
                path.display()
            )),
            Origin::Config | Origin::Default => None,
        };
        self.theme.origin = Origin::Config;
        self.save_theme(chosen.name, overridden);
    }

    /// Written to the user layer on a worker: it is a file write, and a
    /// theme is the developer's, not the project's.
    #[cfg(not(test))]
    fn save_theme(&mut self, name: String, overridden: Option<String>) {
        use super::background::AppEvent;
        use crate::config::{self, Layer};
        let paths = self.paths.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let file = paths.user_config_file();
            let saved = config::patch(&paths, Layer::User, |doc| {
                set_theme(doc, &name);
                Ok(())
            });
            let event = match saved {
                Ok(()) => AppEvent::Notice(match overridden {
                    Some(why) => format!(
                        "theme {name} saved to {} — but {why}",
                        crate::tui::render::home_relative(&file)
                    ),
                    None => format!(
                        "theme {name}, saved to {}",
                        crate::tui::render::home_relative(&file)
                    ),
                }),
                Err(e) => AppEvent::LaunchFailed(format!("could not save the theme: {e:#}")),
            };
            let _ = tx.send(event);
        });
    }

    #[cfg(test)]
    fn save_theme(&mut self, name: String, overridden: Option<String>) {
        self.set_success(match overridden {
            Some(why) => format!("theme {name} saved — but {why}"),
            None => format!("theme {name}"),
        });
    }
}

/// `[ui] theme = "<name>"`, leaving the rest of the file as it was.
pub(super) fn set_theme(doc: &mut toml_edit::DocumentMut, name: &str) {
    let ui = doc
        .entry("ui")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    // `ui = { ... }` and `ui.theme = ...` are the same table to a reader,
    // and a save that only found `[ui]` reported success and wrote nothing.
    if let Some(table) = ui.as_table_like_mut() {
        table.insert("theme", toml_edit::value(name));
    }
}
