//! La compaction de fond vue du daemon : elle vit dans `penelope-conversation` (épopée
//! #208, T23). Le daemon garde la construction de son contexte, qui lit des champs du
//! `Daemon` absents de `Services` (providers, bus, état des compactions), et les tests
//! qui jouent un tour réel.

use penelope_conversation::compaction::Context;

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
