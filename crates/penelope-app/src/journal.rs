//! Le vocabulaire du journal que parlent les ports de la boucle (épopée #208, lot J).
//!
//! `Conversation::push_with` prend déjà une [`Provenance`] ; la boucle écrit aussi ses
//! tentatives (`conv.attempt`) et les bornes de ses tours (`turn.started`,
//! `turn.finished`). Ces charges restent définies dans `penelope-context`, qui les relit
//! et les plie : elles s'appuient sur les types de `penelope-llm` et ne peuvent pas
//! descendre dans `penelope-kernel`. Elles sont réexportées ici, à l'identique, pour que
//! la boucle n'importe que `penelope-app` (`design/v1/README.md` §3.2).

pub use penelope_context::journal::{
    AssistantPayload, AttemptCause, AttemptPayload, ConvEvent, KIND_TURN_FINISHED,
    KIND_TURN_STARTED, Provenance, TurnCall, TurnEnd, TurnIdentity, finished_payload,
    interrupted_payload, is_purged, started_payload,
};
