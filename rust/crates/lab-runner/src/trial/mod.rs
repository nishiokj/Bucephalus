pub(crate) mod artifacts;
pub(crate) mod env;
pub(crate) mod events;
pub(crate) mod execution;
pub(crate) mod grade;
pub(crate) mod layout;
pub(crate) mod materialization;
pub(crate) mod plan;
pub(crate) mod preflight;
pub(crate) mod prepare;
pub(crate) mod schedule;
pub(crate) mod sidecar;
pub(crate) mod spec;
pub(crate) mod state;

pub(crate) fn agent_output_id(output: &str) -> &str {
    output.strip_prefix("agent.").unwrap_or(output)
}
