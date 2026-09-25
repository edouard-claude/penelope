//! Compaction de fond : sortie dans `penelope-conversation` (épopée #208, T23),
//! réexportée sous son ancien chemin jusqu'à T30. Le daemon ne garde que la construction
//! du contexte, et les tests qui jouent un tour réel.

pub use penelope_conversation::compaction::*;

/// Ce que la compaction lit du daemon (T23 : `State` et le daemon ne se tiennent plus).
pub fn context_of(d: &crate::runtime::Daemon) -> Context {
    Context {
        services: d.services.clone(),
        providers: d.providers.clone(),
        bus: d.bus.clone(),
        compaction: d.compaction.clone(),
    }
}

#[cfg(test)]
mod tests;
