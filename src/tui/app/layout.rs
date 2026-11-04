//! Where the detail pane sits beside the worktree list, and how much of
//! the screen it takes: `[ui] detail` and `[ui] detail_size`, which `|`
//! and `<` `>` change and the grove saves.
//!
//! Nothing set is today's screen exactly: the pane on the right, two
//! fifths of the width, and stacked under the list where the screen is
//! too narrow for both.

use crate::config::{Config, DETAIL_SIZE_MAX, DETAIL_SIZE_MIN, UiSection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailPlacement {
    /// Right of the list, stacked under it on a narrow screen.
    #[default]
    Right,
    /// Under the list, whatever the width.
    Bottom,
    /// Not drawn: the list has the whole body.
    Off,
}

impl DetailPlacement {
    /// Every placement, in the order `|` cycles them.
    pub const ALL: [DetailPlacement; 3] = [
        DetailPlacement::Right,
        DetailPlacement::Bottom,
        DetailPlacement::Off,
    ];

    /// Its spelling in `[ui] detail`, one of
    /// [`crate::config::DETAIL_PLACEMENTS`].
    pub fn word(self) -> &'static str {
        match self {
            DetailPlacement::Right => "right",
            DetailPlacement::Bottom => "bottom",
            DetailPlacement::Off => "off",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.word() == word)
    }

    pub fn next(self) -> Self {
        let at = Self::ALL.iter().position(|&p| p == self).unwrap_or(0);
        Self::ALL[(at + 1) % Self::ALL.len()]
    }
}

/// How far one `<` or `>` moves the detail pane's edge, in percent.
pub const DETAIL_STEP: u16 = 5;

/// The detail pane's share when `[ui] detail_size` says nothing: two
/// fifths of the width beside the list, three fifths of the height under
/// it, where the list is read a row at a time.
pub const DETAIL_SIDE_DEFAULT: u16 = 40;
pub const DETAIL_STACKED_DEFAULT: u16 = 60;

/// The detail pane as the developer left it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DetailLayout {
    pub placement: DetailPlacement,
    /// Percent of the body; `None` is the default for where it sits.
    pub size: Option<u16>,
}

impl DetailLayout {
    /// What `[ui]` says, or the default. Validation already refused a
    /// spelling it does not know or a size out of range; a size is still
    /// held to the range, so a `[ui]` read without it cannot break a frame.
    pub fn from_ui(ui: &UiSection) -> Self {
        DetailLayout {
            placement: ui
                .detail
                .as_deref()
                .and_then(DetailPlacement::from_word)
                .unwrap_or_default(),
            size: ui
                .detail_size
                .map(|size| size.clamp(DETAIL_SIZE_MIN, DETAIL_SIZE_MAX)),
        }
    }

    pub fn from_config(config: &Config) -> Self {
        Self::from_ui(&config.ui)
    }

    /// The pane's share of the body, stacked under the list or beside it.
    pub fn share(self, stacked: bool) -> u16 {
        self.size.unwrap_or(match stacked {
            true => DETAIL_STACKED_DEFAULT,
            false => DETAIL_SIDE_DEFAULT,
        })
    }

    /// `<` or `>`: the pane `delta` percent bigger, from the share it has
    /// where it is now, within the range `[ui] detail_size` takes.
    pub fn resized(self, delta: i32, stacked: bool) -> Self {
        let size = (i32::from(self.share(stacked)) + delta)
            .clamp(i32::from(DETAIL_SIZE_MIN), i32::from(DETAIL_SIZE_MAX));
        DetailLayout {
            size: Some(size as u16),
            ..self
        }
    }
}
