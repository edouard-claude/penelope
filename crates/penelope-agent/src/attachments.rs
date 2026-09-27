//! Images refusées par le fournisseur (issue #231).
//!
//! Un 400 causé par une image (`AttachmentRejected`) ne se répare pas en réessayant : la
//! même requête reprend le même refus, et comme la photo reste dans l'historique, le
//! tour suivant aussi. La boucle retire donc les images de la **copie envoyée** et relance
//! une fois ; l'historique et les `conv.*` les gardent. Les images retirées sont nommées
//! par leur empreinte dans un `llm.attachment_rejected` de la session, que le pliage du
//! journal lit aussi : chaque requête suivante les retire de même, à la même place et avec
//! le même texte, ce qui garde son préfixe stable d'un appel à l'autre.

use super::*;
use penelope_kernel::journal::{AttachmentRejectedPayload, KIND_ATTACHMENT_REJECTED};
pub(super) use penelope_llm::attachment::RejectedImages;

/// Les refus déjà écrits pour la session.
pub(super) async fn load(s: &AgentServices, session_id: &str) -> anyhow::Result<RejectedImages> {
    let mut rejected = RejectedImages::default();
    for e in s
        .events
        .session_events_of_kind(session_id, KIND_ATTACHMENT_REJECTED)
        .await?
    {
        if let Ok(p) = serde_json::from_value::<AttachmentRejectedPayload>(e.payload) {
            rejected.insert(&p.images, &p.motif);
        }
    }
    Ok(rejected)
}

/// Écrit le refus des images `sent` et les retire des requêtes suivantes. Faux s'il n'y
/// avait aucune image : rien à retirer, la relance serait la même requête.
pub(super) async fn reject(
    s: &AgentServices,
    spec: &TurnSpec,
    rejected: &mut RejectedImages,
    sent: Vec<String>,
    motif: &str,
    error: &str,
) -> anyhow::Result<bool> {
    if sent.is_empty() {
        return Ok(false);
    }
    tracing::warn!(
        session = %spec.session_id,
        model = %spec.model_id,
        images = sent.len(),
        motif,
        "images refusées par le fournisseur : relance sans elles"
    );
    let payload = AttachmentRejectedPayload {
        model: spec.model_id.clone(),
        motif: motif.to_string(),
        error: penelope_observe::redact(error),
        images: sent,
    };
    s.events
        .append(
            TurnEventKind::AttachmentRejected
                .draft(serde_json::to_value(&payload)?)
                .session(&spec.session_id),
        )
        .await?;
    rejected.insert(&payload.images, motif);
    Ok(true)
}

/// Ce que le propriétaire lit sous la réponse, hors historique.
pub(super) fn owner_note(motif: &str) -> String {
    format!(
        "\n\n_Image non lue : refusée par le fournisseur ({motif}). La renvoyer plus légère \
         ou dans un autre format (JPEG, PNG)._"
    )
}
