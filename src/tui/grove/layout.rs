//! The layout the developer changes with a key and finds again next time:
//! the column shown or hidden (`w`), its width (`{` `}`), and where the
//! detail pane sits (`|`) and how much it takes (`<` `>`). Each is saved
//! in the user layer's `[ui]`, never a project's, the way `,` saves the
//! list's order: how somebody likes their screen is theirs.
//!
//! The saves go through one worker, in the order the keys were pressed,
//! so a burst of `}` leaves the width the last one set; and each writes
//! only the key it changed, so `w` never pins the column's width.

use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};

use crate::tui::app::DetailPlacement;

use super::{COLUMN_MAX_WIDTH, COLUMN_MIN_WIDTH, Grove, GroveEvent, Shown};

impl Shown {
    /// Every mode, in the order [`crate::config::GROVE_MODES`] lists them.
    pub const ALL: [Shown; 3] = [Shown::Auto, Shown::On, Shown::Off];

    /// Its spelling in `[ui] grove`.
    pub fn word(self) -> &'static str {
        match self {
            Shown::Auto => "auto",
            Shown::On => "on",
            Shown::Off => "off",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.word() == word)
    }
}

/// One change of the layout, as it is written to `[ui]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Saved {
    Grove(Shown),
    GroveWidth(u16),
    Detail(DetailPlacement),
    DetailSize(u16),
}

impl Saved {
    /// The `[ui]` key it writes.
    pub fn key(self) -> &'static str {
        match self {
            Saved::Grove(_) => "grove",
            Saved::GroveWidth(_) => "grove_width",
            Saved::Detail(_) => "detail",
            Saved::DetailSize(_) => "detail_size",
        }
    }

    fn value(self) -> toml_edit::Item {
        match self {
            Saved::Grove(shown) => toml_edit::value(shown.word()),
            Saved::GroveWidth(width) => toml_edit::value(i64::from(width)),
            Saved::Detail(placement) => toml_edit::value(placement.word()),
            Saved::DetailSize(size) => toml_edit::value(i64::from(size)),
        }
    }
}

/// Each change into `[ui]`, leaving the rest of the file as it was.
pub fn set_layout(doc: &mut toml_edit::DocumentMut, changes: &[Saved]) {
    let ui = doc
        .entry("ui")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    // `ui = { ... }` is the same table to a reader, as for `[ui] sort`.
    if let Some(table) = ui.as_table_like_mut() {
        for change in changes {
            table.insert(change.key(), change.value());
        }
    }
}

/// The worker every save goes through: what piled up while it wrote is
/// one write, each key at the value it was given last. A save that fails
/// comes back as [`GroveEvent::NotSaved`].
pub fn spawn_layout_saver(home: PathBuf, tx: Sender<GroveEvent>) -> Sender<Saved> {
    let (saver, saves) = mpsc::channel::<Saved>();
    std::thread::spawn(move || {
        while let Ok(first) = saves.recv() {
            let mut changes = vec![first];
            while let Ok(more) = saves.try_recv() {
                changes.retain(|c| c.key() != more.key());
                changes.push(more);
            }
            let saved = crate::config::patch_user(&home, |doc| {
                set_layout(doc, &changes);
                Ok(())
            });
            if let Err(e) = saved
                && tx
                    .send(GroveEvent::NotSaved(format!(
                        "could not save the layout: {e:#}"
                    )))
                    .is_err()
            {
                return;
            }
        }
    });
    saver
}

impl Grove {
    /// Hands a change to the worker that saves it; a test keeps it.
    fn save(&mut self, change: Saved) {
        #[cfg(test)]
        self.saves.push(change);
        if let Some(saver) = &self.saver {
            let _ = saver.send(change);
        }
    }

    /// `w`: the column hidden, or shown again. Hidden, the keys go back
    /// to the list.
    pub(super) fn toggle_column(&mut self) {
        self.shown = match self.column_shown() {
            true => Shown::Off,
            false => Shown::On,
        };
        if !self.column_shown() && self.front.is_some() {
            self.focus = super::Focus::List;
        }
        self.save(Saved::Grove(self.shown));
    }

    /// `{` and `}`: the column narrower or wider, within what it takes.
    pub(super) fn resize_column(&mut self, delta: i32) {
        let width = (i32::from(self.width) + delta)
            .clamp(i32::from(COLUMN_MIN_WIDTH), i32::from(COLUMN_MAX_WIDTH))
            as u16;
        if width == self.width {
            return;
        }
        self.width = width;
        self.save(Saved::GroveWidth(width));
    }

    /// `|`: the detail pane right of the list, under it, or not drawn,
    /// for every project.
    pub(super) fn cycle_detail(&mut self) {
        self.detail.placement = self.detail.placement.next();
        self.share_detail();
        self.say(
            match self.detail.placement {
                DetailPlacement::Right => "detail pane on the right",
                DetailPlacement::Bottom => "detail pane at the bottom",
                DetailPlacement::Off => "detail pane hidden — | shows it again",
            },
            false,
        );
        self.save(Saved::Detail(self.detail.placement));
    }

    /// `<` and `>`: the detail pane smaller or bigger, from the share it
    /// had on the last frame, stacked or beside the list.
    pub(super) fn resize_detail(&mut self, delta: i32) {
        if self.detail.placement == DetailPlacement::Off {
            self.say("the detail pane is hidden — | shows it", false);
            return;
        }
        let stacked = self.front_app().is_some_and(|app| app.detail_stacked);
        let resized = self.detail.resized(delta, stacked);
        if resized == self.detail {
            return;
        }
        self.detail = resized;
        self.share_detail();
        self.save(Saved::DetailSize(resized.share(stacked)));
    }

    /// The detail pane's layout, the same in every open project.
    pub(super) fn share_detail(&mut self) {
        for opened in self.open.values_mut() {
            opened.app.detail = self.detail;
        }
    }
}
