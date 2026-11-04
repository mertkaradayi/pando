//! How projects of one name read apart in the room each label has: the
//! name, then the part of its folder that differs from every other
//! project of that name, and a number in the column's order when not even
//! one character of that part fits. Names of their own that would read
//! alike once cut are cut where they differ instead.

use crate::actions::{ProjectSummary, TwinFolder};

use super::{text_width, truncate};

/// The cells a twin's name keeps when it gives way to its folder.
const NAME_HEAD: usize = 6;

/// The cells of its folder a twin's name gives way to, at most.
const FOLDER_WANT: usize = 10;

/// A name as it reads in its room: the name, then what is said beside it
/// dimmed, with the space before it — the folder of a project another
/// has the name of, or its number among them.
pub(crate) type NameParts = (String, String);

/// Every project's name in `rooms[i]` cells, for each project with a room;
/// `None` for one without. A project whose name another has carries the
/// part of its folder that tells it from each of them: its name keeps
/// what that leaves, and gives way to a short head when the folder would
/// not show where it differs otherwise. Where even that does not fit, or
/// two would still read alike, every one of that name carries its number
/// instead, `·1` `·2` in the column's order. Projects of different names
/// whose labels would read alike are each cut where the name differs
/// from the others', rather than at its end, so no two labels ever read
/// the same.
pub(crate) fn names_apart(
    summaries: &[ProjectSummary],
    rooms: &[Option<usize>],
) -> Vec<Option<NameParts>> {
    // Each label, `Some(None)` for a twin no cut tells apart.
    let fitted: Vec<Option<Option<NameParts>>> = summaries
        .iter()
        .zip(rooms)
        .map(|(s, room)| {
            let room = (*room)?;
            Some(match s.twin_folder(summaries) {
                None => Some((truncate(&s.name, room), String::new())),
                Some(twin) => fit_twin(&s.name, &twin, room),
            })
        })
        .collect();
    let mut parts: Vec<Option<NameParts>> = fitted.iter().map(|f| f.clone().flatten()).collect();
    for (i, summary) in summaries.iter().enumerate() {
        let of_name: Vec<usize> = (0..summaries.len())
            .filter(|&j| summaries[j].name == summary.name)
            .collect();
        // Each name once, from its first project.
        if of_name.len() < 2 || of_name[0] != i {
            continue;
        }
        let labels: Vec<Option<String>> = of_name
            .iter()
            .filter_map(|&j| fitted[j].as_ref())
            .map(|f| f.as_ref().map(|(name, aside)| format!("{name}{aside}")))
            .collect();
        let apart = labels.iter().enumerate().all(|(a, label)| {
            label.as_ref().is_some_and(|label| {
                labels[a + 1..]
                    .iter()
                    .all(|other| other.as_ref() != Some(label))
            })
        });
        if apart {
            continue;
        }
        for (k, &j) in of_name.iter().enumerate() {
            if let Some(room) = rooms[j] {
                parts[j] = Some(numbered(&summaries[j].name, k + 1, room));
            }
        }
    }
    unalike(summaries, rooms, parts)
}

/// The labels of `parts` with no two of different names reading alike:
/// a name cut at its end to the same label as another's is cut where it
/// differs from those instead, `pando…alp…` and `pando…beta` for
/// `pando-fixture-alpha` and `pando-fixture-beta`.
fn unalike(
    summaries: &[ProjectSummary],
    rooms: &[Option<usize>],
    parts: Vec<Option<NameParts>>,
) -> Vec<Option<NameParts>> {
    let labels: Vec<Option<String>> = parts
        .iter()
        .map(|p| p.as_ref().map(|(name, aside)| format!("{name}{aside}")))
        .collect();
    let mut apart = parts.clone();
    for (i, label) in labels.iter().enumerate() {
        let (Some(label), Some(room), Some((_, aside))) = (label, rooms[i], &parts[i]) else {
            continue;
        };
        let name = &summaries[i].name;
        let alike: Vec<&str> = (0..summaries.len())
            .filter(|&j| labels[j].as_ref() == Some(label) && summaries[j].name != *name)
            .map(|j| summaries[j].name.as_str())
            .collect();
        if alike.is_empty() {
            continue;
        }
        if let Some(cut) = cut_apart(name, &alike, room.saturating_sub(text_width(aside))) {
            apart[i] = Some((cut, aside.clone()));
        }
    }
    apart
}

/// The cells of a name's start kept before where it differs, at most.
const APART_HEAD: usize = 5;

/// The cells kept from where it differs on, its trailing `…` included,
/// before the head is given any.
const APART_REST: usize = 4;

/// `name` in `room` cells, cut where it differs from every one of
/// `others`: a head of its start, `…`, then its end where what follows
/// that point fits, or the name from that point cut at its end. `None` where the name fits whole,
/// is wide, holds no more than they share, or the room holds no such cut.
fn cut_apart(name: &str, others: &[&str], room: usize) -> Option<String> {
    let chars: Vec<char> = name.chars().collect();
    let len = chars.len();
    if text_width(name) <= room || text_width(name) != len {
        return None;
    }
    let differs = others
        .iter()
        .map(|o| {
            chars
                .iter()
                .zip(o.chars())
                .take_while(|(a, b)| **a == *b)
                .count()
        })
        .max()
        .unwrap_or(0);
    let rest = len.checked_sub(differs).filter(|&r| r > 0)?;
    // A character of the head, the `…`, and one of the rest.
    if differs == 0 || room < 3 {
        return None;
    }
    let head = differs
        .min(APART_HEAD)
        .min(room.saturating_sub(1 + rest.min(APART_REST)))
        .max(1);
    let left = room.checked_sub(head + 1).filter(|&l| l > 0)?;
    // Its end where the rest fits, as much of it as the room holds;
    // from where it differs, cut at its end, where it does not.
    let (from, tail) = match rest <= left {
        true => (len - left, chars[len - left..].iter().collect::<String>()),
        false if left >= 2 => {
            let kept: String = chars[differs..differs + left - 1].iter().collect();
            (differs, format!("{kept}…"))
        }
        false => return None,
    };
    let head: String = chars[..head].iter().collect();
    // `…` where it skips some of the name.
    Some(match head.chars().count() == from {
        true => format!("{head}{tail}"),
        false => format!("{head}…{tail}"),
    })
}

/// The cells of its name a numbered twin keeps at least: its number
/// only ever follows part of its name.
const NUMBERED_HEAD: usize = 3;

/// A twin's name with its number among the projects of its name, after
/// a head of the name of [`NUMBERED_HEAD`] cells at least; the name
/// alone, cut to the room, where not even that fits beside the number.
fn numbered(name: &str, number: usize, room: usize) -> NameParts {
    let mark = format!(" ·{number}");
    let head = text_width(name).min(NUMBERED_HEAD);
    match room >= head + text_width(&mark) {
        true => (truncate(name, room - text_width(&mark)), mark),
        false => (truncate(name, room), String::new()),
    }
}

/// A twin's name and the part of its folder that differs in `room`
/// cells: the whole name where the folder still shows where it differs
/// beside it, a head of it otherwise, or `None` when no cut shows that.
pub(crate) fn fit_twin(name: &str, twin: &TwinFolder, room: usize) -> Option<NameParts> {
    let whole = text_width(name);
    if let Some(folder) = room
        .checked_sub(whole + 1)
        .and_then(|left| fit_folder(twin, left))
    {
        return Some((name.to_string(), format!(" {folder}")));
    }
    let head = whole.min(NAME_HEAD);
    let want = text_width(&twin.folder).min(FOLDER_WANT);
    let most = room.saturating_sub(want + 1).min(whole).max(head);
    (head..=most).rev().find_map(|cells| {
        let name = truncate(name, cells);
        let left = room.checked_sub(text_width(&name) + 1)?;
        let folder = fit_folder(twin, left)?;
        Some((name, format!(" {folder}")))
    })
}

/// A twin's folder in `room` cells, cut so that what tells it from the
/// others survives: at its end, as any label, when the cut keeps where it
/// differs; at its front when the end differs — `client-project-a` and
/// `client-project-b` read `…ect-a` and `…ect-b` — and at both ends when
/// even what differs is longer than the room. `None` when no cut shows a
/// character of where it differs from every other twin's folder.
pub(crate) fn fit_folder(twin: &TwinFolder, room: usize) -> Option<String> {
    let folder: Vec<char> = twin.folder.chars().collect();
    let len = folder.len();
    let others: Vec<Vec<char>> = twin.others.iter().map(|o| o.chars().collect()).collect();
    // Whether the characters `from..to` differ from every other folder
    // somewhere.
    let apart = |from: usize, to: usize| {
        others
            .iter()
            .all(|o| (from..to).any(|p| o.get(p) != folder.get(p)))
    };
    if text_width(&twin.folder) <= room {
        return apart(0, len).then(|| twin.folder.clone());
    }
    // The offsets are in characters, a cell apiece only when nothing in
    // the folder is wide: a folder that is not is only cut at its end.
    let narrow = text_width(&twin.folder) == len;
    if room < 2 || !narrow {
        let cut = truncate(&twin.folder, room);
        let shown = cut.chars().count().saturating_sub(1);
        return (room >= 2 && apart(0, shown)).then_some(cut);
    }
    let keep = room - 1;
    let shared = twin.shared.min(len);
    let tail = len - keep;
    let from = |at: usize, cells: usize| -> String { folder[at..at + cells].iter().collect() };
    // Its start, as any label is cut; its end; from where it first
    // differs, both ends cut.
    let start = (0, keep, format!("{}…", from(0, keep)));
    let end = (tail, len, format!("…{}", from(tail, keep)));
    let inner = room.saturating_sub(2);
    let middle = |at: usize| (at, at + inner, format!("…{}…", from(at, inner)));
    // The end first when it holds where it first differs: `…ect-a`, not
    // `client-…`.
    let end_first = tail <= shared;
    let mut windows = Vec::new();
    if end_first {
        windows.push(end.clone());
    }
    windows.push(start);
    if inner > 0 && shared > 0 && shared + inner < len {
        windows.push(middle(shared));
    }
    if !end_first {
        windows.push(end);
    }
    // Then anywhere else it differs from every other.
    if inner > 0 {
        windows.extend((1..len - inner).map(middle));
    }
    windows
        .into_iter()
        .find(|(from, to, _)| apart(*from, *to))
        .map(|(_, _, cut)| cut)
}
