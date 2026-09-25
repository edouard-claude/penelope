//! `penelope-orchestrator` : le moteur de workflows et l'ordonnanceur (épopée #208, T27).
//!
//! Au-dessus de la boucle d'agent (`penelope-agent`), de l'exécuteur des outils natifs
//! (`penelope-executor`), de la conversation et du rêve, qu'il déclenche ; sous le daemon,
//! qui le compose. Il ne connaît le daemon que par un [`Context`] (services, providers,
//! état des runs, services de la boucle), et le canal que par les ports de
//! `penelope-app` (`Messenger`, `ChannelDelivery`), jamais `penelope-telegram` ni la
//! passerelle.

#![forbid(unsafe_code)]

pub mod scheduler;
pub mod workflow;

pub use workflow::{Context, State, WorkflowOrchestrator};
