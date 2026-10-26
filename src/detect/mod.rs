//! Tier 1: what the repository says about how to run itself.
//!
//! Everything here is read-only over the main checkout, and everything it
//! produces is a *proposal* — a list of candidates a rule found, in rule
//! order, with the reason each one is a candidate. Nothing starts anything.
//! That split is Invariant 2: detection only ever writes `pando.toml`, and
//! the runtime only ever reads it.
//!
//! The rules enumerate; they never invent. A script that is not in
//! `package.json` cannot be proposed, so the worst a wrong rule can do is
//! pick the wrong one of the developer's own commands — one visible edit to
//! a file with a `# detected:` comment next to the line.
//!
//! **Where things are.** `signals` reads the repository: manifests,
//! scripts, make targets, version files, env examples and what is
//! gitignored — at the root, and in the app directories below a root that
//! has no manifest of its own. `frameworks` picks the framework rule those signals match;
//! the rules themselves live in `catalog::frameworks`. `proposal` holds
//! the types a proposal is made of and [`propose`], which asks every slot;
//! `dev` has the per-slot proposals for install, version files, the dev
//! command, make targets, ports and provision, `workspaces` the apps of a
//! monorepo, and `services` the services a project talks to and its schema
//! hook. `base` asks which branch work starts from, from the refs alone.
//! `apply` turns a chosen candidate into config and into edits.

pub use crate::catalog::frameworks::{FrameworkRule, PortMechanism, RULES};

mod apply;
mod base;
mod dev;
mod frameworks;
mod proposal;
mod services;
mod signals;
#[cfg(test)]
mod tests;
mod workspaces;

pub use apply::{
    DEV, Edit, answers_value, apply, apply_native_services, apply_services, array_edits, custom,
    edits, fills_one_dev_process, is_env_name, join_list, may_fill_dev, native_entry, roles_in,
    service_entry, snippet, still_needed, typed_ports,
};
pub use dev::{clone_proposal, unlocked_command};
pub use frameworks::{Served, framework, served_by};
pub use proposal::{
    Candidate, ComposeResolver, Proposal, ServiceHint, Slot, propose, propose_with,
};
pub use services::{
    MachineEvidence, NO_PREFERENCE_EVIDENCE, SCHEMA_HOOK, ServiceChoice, ServiceSource,
    addressed_engines, service_choice, service_choice_for,
};
pub use signals::{AppDir, Signals, Target, is_gitignored, is_ignored_in, signals, signals_of};
pub use workspaces::{UNSTARTED, WorkspaceApp, workspace_apps};
