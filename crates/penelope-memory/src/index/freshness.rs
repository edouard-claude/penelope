//! Date de fraîcheur d'une entrée (`maj`) : comment elle est posée, comment elle se lit.
//!
//! `maj` est le jour où le contenu de l'entrée a bougé pour la dernière fois, ou celui où
//! le propriétaire l'a validée ; ce n'est pas le jour de la dernière réindexation (#451).
//! La récence du classement (`memory.half_life_days`) et le repérage des entrées jamais
//! rappelées en dépendent : une réindexation qui rajeunit tout les annule.

/// Insère ou met à jour une entrée. `?14` est le jour à poser quand l'entrée est nouvelle
/// ou a changé, `?19` la date de validation par le propriétaire (annotation `revue`).
///
/// - Entrée déjà indexée : `maj` n'avance que si le texte ou une annotation indexée a
///   changé. Un déplacement (fichier, section, niveau) ou un retour après retrait ne
///   rajeunit pas.
/// - Entrée absente de l'index mais déjà connue (index reconstruit, la provenance et
///   l'historique survivent) : la dernière écriture de l'historique, sinon la première
///   observation, jamais plus tard que le jour donné.
/// - Dans les deux cas une validation plus récente sert de plancher.
pub(super) const UPSERT: &str =
    "INSERT INTO mem_entries(uid, file, anchor, level, etype, slug, text, quand,
        importance, projet, confiance, statut, depuis, maj, pinned, declencheurs,
        content_hash, retired_at)
     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,
        MAX(MIN(?14, COALESCE(
                (SELECT substr(MAX(ts), 1, 10) FROM mem_history
                  WHERE uid = ?1 AND op IN ('add_entry','replace_entry','supersede_entry')),
                (SELECT substr(observed_at, 1, 10) FROM mem_provenance WHERE uid = ?1),
                ?14)),
            MIN(?14, COALESCE(?19, ''))),
        ?15,?16,?17,?18)
     ON CONFLICT(uid) DO UPDATE SET
        file=excluded.file, anchor=excluded.anchor, level=excluded.level,
        etype=excluded.etype, slug=excluded.slug, text=excluded.text,
        quand=excluded.quand, importance=excluded.importance,
        projet=excluded.projet, confiance=excluded.confiance,
        statut=excluded.statut, depuis=excluded.depuis,
        maj=MAX(CASE WHEN mem_entries.content_hash IS NOT excluded.content_hash
                       OR mem_entries.quand IS NOT excluded.quand
                       OR mem_entries.importance IS NOT excluded.importance
                       OR mem_entries.projet IS NOT excluded.projet
                       OR mem_entries.confiance IS NOT excluded.confiance
                       OR mem_entries.depuis IS NOT excluded.depuis
                       OR mem_entries.declencheurs IS NOT excluded.declencheurs
                     THEN ?14 ELSE mem_entries.maj END,
                MIN(?14, COALESCE(?19, ''))),
        pinned=excluded.pinned, declencheurs=excluded.declencheurs,
        content_hash=excluded.content_hash, retired_at=excluded.retired_at";

/// L'annotation `revue` d'une entrée du vault, quand c'est un jour lisible : une valeur
/// saisie de travers ne doit pas devenir une date de fraîcheur.
pub(super) fn validated_on(revue: Option<&str>) -> Option<String> {
    let day = revue?.trim();
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .is_ok()
        .then(|| day.to_string())
}

/// Âge en jours d'une date de fraîcheur (`AAAA-MM-JJ` ou RFC 3339) ; 0 si elle est
/// illisible ou dans le futur.
pub(super) fn age_days(maj: &str, now_ms: i64) -> f64 {
    let parsed = chrono::NaiveDate::parse_from_str(maj, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc().timestamp_millis())
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(maj)
                .ok()
                .map(|d| d.timestamp_millis())
        });
    match parsed {
        Some(ms) => ((now_ms - ms).max(0) as f64) / 86_400_000.0,
        None => 0.0,
    }
}
