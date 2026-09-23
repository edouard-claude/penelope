//! `penelope-workflow` : schéma, validation, exécuteur durable, triggers (§12).

#![forbid(unsafe_code)]

pub mod bundled;
pub mod conditions;
pub mod model;
pub mod plan;
pub mod registry;
pub mod runs;
pub mod schedules;
pub mod validate;

pub use conditions::{EvalContext, TemplateVars, choose, evaluate, substitute};
pub use model::{BLOCKED, DONE, Step, StepResult, Transition, Workflow};
pub use registry::WorkflowRegistry;
pub use runs::{Control, Run, RunState, RunStore};
pub use schedules::{Schedule, ScheduleStore, TargetKind, TriggerKind};
pub use validate::{Known, Report, validate};
