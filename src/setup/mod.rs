//! A project's setup: whether pando is set up and tested for it, and the
//! one line that hands the job to the developer's own coding agent.
//!
//! Read from files pando owns — the config layers, the last check's
//! record, and what the setup screen remembers — never from the
//! repository. [`read`] decides one of seven [`SetupState`]s, in order:
//! a check running, the last one interrupted, nothing to run yet, then the
//! last check against today's run settings, compared by [`fingerprint`].
//! What a check stores of a failed process's log goes through
//! [`redact_line`] first. [`memory_block`] is what an agent keeps about
//! running the project once it is set up, and pando's own copies of it.

mod fingerprint;
mod record;
mod redact;
mod remember;
mod state;

pub use fingerprint::{FINGERPRINT_VERSION, fingerprint};
pub use record::{
    CheckMode, CheckOutcome, CheckRecord, DiskCost, DiskTree, FailureKind, ProcessResult, RanBy,
    SetupMemory,
};
pub use redact::redact_line;
pub use remember::{MEMORY_FILE_HEADER, device_note, memory_block, write_memory_files};
pub use state::{Setup, SetupState, check_running, decide, read};

/// The prompt a developer pastes into their coding agent: one line,
/// identical on the setup screen, in the header hint, in the CLI tip and
/// in the README. The rules live in the job `pando init --agent` prints,
/// so the prompt never changes with them.
pub const SETUP_PROMPT: &str =
    "Set up pando here: run `pando init --agent` and follow what it says.";

#[cfg(test)]
mod tests;
