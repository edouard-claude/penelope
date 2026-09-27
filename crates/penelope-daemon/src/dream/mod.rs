//! Tests du rêve qui ont besoin d'un daemon : la consolidation nocturne, le digest du
//! matin et l'entretien du vault vivent dans `penelope-dream` (T26) ; ce que le digest
//! lit au-dessus du rêve est calculé par l'ordonnanceur
//! (`penelope_orchestrator::scheduler::digest_inputs`, T27).

mod tests;
