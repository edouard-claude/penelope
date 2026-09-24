//! Contrôles de la mémoire : taille, index du coffre, embeddings.

use super::*;

/// #145 : une entrée fourre-tout fausse la consolidation (elle « contredit » tout ce
/// qu'elle approche) et déborde dans le digest ; un Cœur au-delà de son budget n'est pas
/// injecté en entier.
pub async fn memory_size_check(s: &Services) -> DoctorCheck {
    const ID: &str = "memory.size";
    const LABEL: &str = "Taille des entrées de mémoire";
    let max = penelope_memory::quality::MAX_ENTRY_CHARS;
    let long: Vec<String> = crate::mem_split::oversized(s)
        .await
        .iter()
        .map(|e| {
            format!(
                "`{}` ({} caractères, {})",
                e.uid,
                e.text.chars().count(),
                e.file
            )
        })
        .collect();
    let budget = s.config.config().memory.core_budget_tokens as u64;
    let overflow = crate::dream::core_overflow(s, budget).await;

    if long.is_empty() && overflow.is_none() {
        return DoctorCheck::ok(
            ID,
            LABEL,
            format!("toutes sous {max} caractères, Cœur dans son budget de {budget} jetons"),
        );
    }
    let mut detail = Vec::new();
    if !long.is_empty() {
        detail.push(format!(
            "{} entrée(s) au-delà de {max} caractères : {}",
            long.len(),
            long.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(w) = overflow {
        detail.push(w);
    }
    DoctorCheck::fail(
        ID,
        LABEL,
        detail.join(" ; "),
        Some("`penelope mem split <uid>` propose le découpage en un fait par entrée".into()),
    )
}

/// Contenu du vault hors de l'index (issue #15).
pub async fn vault_index_check(s: &Services) -> DoctorCheck {
    const ID: &str = "vault_index";
    const LABEL: &str = "Vault indexé";
    match crate::vault_inventory::inventory(s).await {
        Ok(inv) if inv.not_indexed.is_empty() => DoctorCheck::ok(
            ID,
            LABEL,
            format!(
                "{} fichier(s), {} entrée(s) indexée(s)",
                inv.files, inv.entries
            ),
        ),
        Ok(inv) => {
            let names: Vec<&str> = inv
                .not_indexed
                .iter()
                .take(5)
                .map(|g| g.path.as_str())
                .collect();
            DoctorCheck::fail(
                ID,
                LABEL,
                format!(
                    "{} fichier(s) présents mais hors index : {}{}",
                    inv.not_indexed.len(),
                    names.join(", "),
                    if inv.not_indexed.len() > 5 { "…" } else { "" }
                ),
                Some("penelope vault check".into()),
            )
        }
        Err(e) => DoctorCheck::fail(ID, LABEL, e.to_string(), None),
    }
}

/// Alias `embedding` joignable (issue #11) : sans lui, la recherche reste lexicale.
pub async fn embedding_check(d: &crate::runtime::Daemon) -> DoctorCheck {
    const ID: &str = "embedding";
    const LABEL: &str = "Embeddings (recherche par le sens)";
    let fix = Some(format!(
        "penelope config set models.aliases.embedding {}",
        penelope_kernel::config::DEFAULT_EMBEDDING_MODEL
    ));
    let Some(model) = crate::embeddings::model(&d.services) else {
        return DoctorCheck::fail(ID, LABEL, "aucun modèle pour le rôle `embedding`", fix);
    };
    let texts = ["penelope doctor".to_string()];
    let emb = d.embedder();
    let probe = crate::embeddings::embed_texts(&emb, &texts);
    match tokio::time::timeout(std::time::Duration::from_secs(15), probe).await {
        Ok(Ok((_, v))) if v.first().is_some_and(|x| !x.is_empty()) => {
            DoctorCheck::ok(ID, LABEL, format!("`{model}`, {} dimensions", v[0].len()))
        }
        Ok(Ok(_)) => DoctorCheck::fail(ID, LABEL, format!("`{model}` : vecteur vide"), fix),
        Ok(Err(e)) => DoctorCheck::fail(
            ID,
            LABEL,
            format!("`{model}` injoignable ({e}) : recherche lexicale seule"),
            fix,
        ),
        Err(_) => DoctorCheck::fail(
            ID,
            LABEL,
            format!("`{model}` ne répond pas en 15 s : recherche lexicale seule"),
            fix,
        ),
    }
}
