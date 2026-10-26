//! Detections that have gone stale: a detected value the rules would no
//! longer write.

use std::collections::BTreeMap;

use crate::config::Config;
use crate::detect::{self, Slot};
use crate::paths::PandoPaths;

use super::config::value_repr;
use super::report::{ConfigReport, Finding, KeyReport, LayerReport, Section};

/// One value a rule would write right now, and the signal behind it.
struct Offer {
    /// The comparison form: quote style, whitespace and the order an
    /// inline table happens to be written in taken out.
    canonical: String,
    /// The value as a file would hold it.
    text: String,
    /// What made it a candidate — `package.json scripts.dev`.
    why: String,
    /// The question whose candidate writes it.
    slot: Slot,
    /// That candidate, as `init --answers` takes it for that question.
    answer: serde_json::Value,
}

/// The command that answers `slot` with `answer` over what config says:
/// `echo '{"port_env":"RCT_METRO_PORT"}' | pando init --answers -
/// --replace`. What a fix prints when the answer it names is one pando
/// can write, so the developer runs one line rather than editing a file.
pub fn answers_command(slot: Slot, answer: serde_json::Value) -> String {
    let name = serde_json::to_value(slot)
        .ok()
        .and_then(|name| name.as_str().map(str::to_string))
        .expect("every slot serialises to its name");
    let file = serde_json::Value::Object(serde_json::Map::from_iter([(name, answer)]));
    let file = crate::process::shell_word(&file.to_string());
    // Not `echo` over a backslash: some shells' `echo` reads `\\` as an
    // escape, and the JSON would arrive with one fewer.
    let print = match file.contains('\\') {
        true => "printf '%s\\n'",
        false => "echo",
    };
    format!("{print} {file} | pando init --answers - --replace")
}

/// Keys pando wrote itself that pando would not write now.
///
/// pando's rule is "ask just in time, once": a slot with an answer in it
/// is never asked again. That is the right rule and it has a cost nothing
/// else here pays — improve a detection rule, and every value the older,
/// worse version of it wrote stays frozen exactly as wrong as the day it
/// was written. pando records that it detected a value itself, re-derives
/// detection on every read path, and until now never compared the two: a
/// developer who upgraded saw the identical failure and had every reason
/// to think the binary was stale.
///
/// **Reported, never fixed.** Silently rewriting a value would change what
/// a developer's commands do with nobody asked, which breaks a larger
/// promise than the one it mends. So this names the key, the file, what
/// was detected, what would be detected now, and the ways to change it:
/// the command that writes what the rules offer now, and deleting the
/// line where that really reopens the question.
///
/// Three things it is careful about:
///
/// - **Only `# detected:`.** An `# answered:` value — a person's, a
///   program's, `--yes`'s — is a decision, and pando does not second-guess
///   decisions. A comment a developer wrote by hand is not one of pando's
///   notes at all and is left alone for the same reason.
/// - **Not among the candidates, rather than not the first.** Several
///   candidates can each be right, and a rule that demotes one has not
///   rejected it. Absent from the list is the signal that the rules have
///   moved — or that the repository has, which fires the same check and is
///   just as worth knowing.
/// - **Keyed by the key, not by the slot.** A dev-command candidate that
///   carries roles writes `dev.ports` beside `dev.cmd`, under the dev
///   command's own note, so "what would be written here" has to be
///   gathered across every proposal rather than from whichever slot owns
///   the key.
pub(super) fn stale_detection_findings(
    paths: &PandoPaths,
    merged: &Config,
    config: &ConfigReport,
    findings: &mut Vec<Finding>,
) {
    let detected: Vec<(&LayerReport, &KeyReport, String)> = effective_keys(config)
        .into_iter()
        .filter_map(|(layer, key)| Some((layer, key, detected_why(key.note.as_deref())?)))
        .collect();
    // Nothing pando wrote: no reason to re-derive detection at all, which
    // reads the repository and shells out to `git check-ignore`.
    if detected.is_empty() {
        return;
    }
    let offers = detectable_now(&paths.project);
    for (layer, key, why) in detected {
        let (Some(raw), Some(text)) = (&key.raw, &key.value) else {
            continue;
        };
        // pando has no opinion about this key any more, and a divergence
        // needs two opinions. A rule that was withdrawn and put nothing in
        // its place has nothing to say about the value it left behind.
        let Some(offers) = offers.get(&key.key) else {
            continue;
        };
        if offers.iter().any(|offer| offer.canonical == canonical(raw)) {
            continue;
        }
        let list = offers
            .iter()
            .map(|offer| format!("{} ({})", offer.text, offer.why))
            .collect::<Vec<_>>()
            .join(", ");
        // The first offer a replacement can write, and whether taking the
        // line out asks the question again — often it does not: a `[dev]`
        // with its command still in it has answered its ports by existing.
        let command = offers
            .iter()
            .find(|offer| replaces(paths, layer, &key.key, offer.slot))
            .map(|offer| {
                let command = answers_command(offer.slot, offer.answer.clone());
                match offer.slot.key() {
                    Some(_) => format!("`{command}` writes {} in its place", offer.text),
                    // The process list's answer is whole tables, and a
                    // replacement takes every one of them away first: a
                    // process added by hand, or a key added to one, goes
                    // with it. Said, so "in its place" is not a promise
                    // about one line.
                    None => format!(
                        "`{command}` replaces every process table in {} with the ones the rules \
                         offer now ({}), and anything added to them by hand goes with them",
                        layer.path,
                        offer
                            .answer
                            .as_object()
                            .map(|tables| tables.keys().cloned().collect::<Vec<_>>().join(", "))
                            .unwrap_or_default()
                    ),
                }
            });
        let delete = reopens(config, layer, &key.key, merged).then(|| {
            format!(
                "delete that line from {} and the next command that needs it asks the question \
                 again",
                layer.path
            )
        });
        let fix = match (command, delete) {
            (Some(command), Some(delete)) => format!("{command}, or {delete}"),
            (Some(one), None) | (None, Some(one)) => one,
            (None, None) => format!("change that line in {} to one of them", layer.path),
        };
        findings.push(
            Finding::note(
                Section::Config,
                format!(
                    "{} = {text} in {} is a value pando detected itself ({why}), and pando \
                     would not detect it now — for this key its rules offer {list}",
                    key.key, layer.path
                ),
            )
            .with_fix(format!(
                "{fix}; keep it if it is what you want — pando will not change a value on its own"
            )),
        );
    }
}

/// Whether `init --answers - --replace` for `slot` writes `key` over the
/// line `layer` holds.
///
/// Only where `key` is that slot's own: the dev command's rule writes
/// `dev.ports` beside `dev.cmd`, and a replaced command leaves a `ports`
/// already there as it is. The process list, whose answer is whole
/// tables, replaces every key under them, but only in pando's own layer —
/// `--replace` refuses tables a file beneath it declares. The prelude it never
/// replaces: only a person changes it.
fn replaces(paths: &PandoPaths, layer: &LayerReport, key: &str, slot: Slot) -> bool {
    if slot.layer() != crate::config::Layer::Project {
        return false;
    }
    if slot.key().is_some() {
        return owns(slot, key);
    }
    // Of the slots whose answer is whole tables, only the process list's
    // candidate is the whole answer. A service or a hook is one entry of a
    // set, and the set replaced by that one entry loses every other.
    if slot != Slot::Processes {
        return false;
    }
    let own_layer = layer.path == paths.config_file().display().to_string();
    own_layer
        && slot.answer_tables().iter().any(|table| {
            let table = table.join(".");
            key == table || key.starts_with(&format!("{table}."))
        })
}

/// Whether deleting `key` from `layer` makes the next command that needs
/// it ask its question again.
///
/// Asked of the merged config with the value gone, through the same test
/// every command asks, `actions::settled`: a key no slot names is part of
/// an answer that stays settled without it, and a key another layer also
/// sets comes back from that layer.
fn reopens(config: &ConfigReport, layer: &LayerReport, key: &str, merged: &Config) -> bool {
    let Some(slot) = crate::actions::ALL_SLOTS
        .into_iter()
        .find(|slot| owns(*slot, key))
    else {
        return false;
    };
    let elsewhere = config.layers.iter().any(|other| {
        !std::ptr::eq(other, layer) && other.keys.iter().any(|k| k.key == key && k.value.is_some())
    });
    if elsewhere {
        return false;
    }
    let mut without = merged.clone();
    match slot {
        Slot::Install => without.project.install = None,
        Slot::VersionFiles => without.runtime.version_files.clear(),
        Slot::Prelude => without.runtime.prelude = None,
        Slot::Provision => without.project.provision = None,
        Slot::Clone => without.project.clone = None,
        Slot::Base => without.project.base = None,
        Slot::DevCmd => {
            if let Some(dev) = without.processes.get_mut(detect::DEV) {
                dev.cmd.clear();
            }
        }
        Slot::PortEnv => {
            if let Some(dev) = without.processes.get_mut(detect::DEV) {
                dev.ports = None;
            }
        }
        _ => return false,
    }
    !crate::actions::settled(slot, &without)
}

/// Whether `key` is the one key `slot` writes its answer to.
fn owns(slot: Slot, key: &str) -> bool {
    slot.key()
        .is_some_and(|(table, own)| key == dotted(table, own))
}

/// A table path and a key, as the report spells a key: `dev.ports`.
fn dotted(table: &[&str], key: &str) -> String {
    match table.is_empty() {
        true => key.to_string(),
        false => format!("{}.{key}", table.join(".")),
    }
}

/// Every key the merged config actually reads, with the layer it reads it
/// from.
///
/// The layers are built lowest precedence first, which is the order
/// `config::load_layers` merges them in, so the last layer to set a key is
/// the one that wins. `merge_tables` merges a table key by key, which is
/// what makes "the last layer that set *this key*" the right question
/// rather than "the last layer that set its table".
///
/// Table headers and array-of-table entries carry a note and no value of
/// their own, and a stripped key is one pando removes from that layer and
/// never reads. Neither is a value to have an opinion about.
fn effective_keys(config: &ConfigReport) -> Vec<(&LayerReport, &KeyReport)> {
    let mut out: Vec<(&LayerReport, &KeyReport)> = Vec::new();
    for layer in &config.layers {
        for key in &layer.keys {
            if key.value.is_none() || key.ignored {
                continue;
            }
            match out.iter_mut().find(|(_, held)| held.key == key.key) {
                Some(entry) => *entry = (layer, key),
                None => out.push((layer, key)),
            }
        }
    }
    out
}

/// Every value detection would write right now, keyed by the config key it
/// would write it to.
///
/// Through [`detect::edits`], which is the one translation from a
/// candidate to the keys it becomes — the same function the answer path
/// writes through. A second reading of what a candidate means would be a
/// second opinion, and the two would drift.
fn detectable_now(project: &crate::project::ProjectRef) -> BTreeMap<String, Vec<Offer>> {
    let root = project.root.as_path();
    let signals = detect::signals_of(project);
    let mut out: BTreeMap<String, Vec<Offer>> = BTreeMap::new();
    for proposal in detect::propose(root, &signals) {
        for candidate in &proposal.candidates {
            for edit in detect::edits(proposal.slot, candidate) {
                let key = match edit.table.is_empty() {
                    true => edit.key.clone(),
                    false => format!("{}.{}", edit.table.join("."), edit.key),
                };
                let offer = Offer {
                    canonical: canonical(&edit.value),
                    text: value_repr(&edit.value),
                    why: candidate.why.clone(),
                    slot: proposal.slot,
                    answer: detect::answers_value(proposal.slot, candidate),
                };
                let held = out.entry(key.clone()).or_default();
                match held
                    .iter_mut()
                    .find(|other| other.canonical == offer.canonical)
                {
                    None => held.push(offer),
                    // The same value from the slot the key belongs to is
                    // the one a replacement can write: `[dev]`'s ports
                    // offered by the dev command's rule and by the port
                    // question are one offer, answered at the second.
                    Some(other) if !owns(other.slot, &key) && owns(offer.slot, &key) => {
                        *other = offer;
                    }
                    Some(_) => {}
                }
            }
        }
    }
    out
}

/// The evidence out of a `# detected:` note, and `None` for anything else.
///
/// `# answered:` in each of its four spellings, and a comment a developer
/// wrote themselves, all come back `None`: this only ever looks at values
/// pando's own rules put there.
fn detected_why(note: Option<&str>) -> Option<String> {
    let body = note?.strip_prefix('#')?.trim_start();
    Some(body.strip_prefix("detected:")?.trim().to_string())
}

/// A value with its formatting taken out, so that two values meaning the
/// same thing compare equal.
///
/// A literal string and a basic string are the same value; so are two
/// inline tables written in a different key order, and an array with a
/// space after its comma. Array *order* is kept, because `version_files`
/// is a precedence list and reordering it means something.
fn canonical(value: &toml_edit::Value) -> String {
    match value {
        toml_edit::Value::String(s) => format!("{:?}", s.value()),
        toml_edit::Value::Array(array) => {
            let items: Vec<String> = array.iter().map(canonical).collect();
            format!("[{}]", items.join(","))
        }
        toml_edit::Value::InlineTable(table) => {
            let mut items: Vec<String> = table
                .iter()
                .map(|(key, value)| format!("{key:?}={}", canonical(value)))
                .collect();
            items.sort();
            format!("{{{}}}", items.join(","))
        }
        other => value_repr(other),
    }
}
