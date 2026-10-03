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
    let overflow = penelope_vault::snapshot::core_overflow(s, budget).await;

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
    // Le conseil suit le constat : le découpage pour les entrées longues ; pour le Cœur
    // plein, ce que la nuit fait seule et le réglage qui relève le budget (issue #298).
    let mut fix = Vec::new();
    if !long.is_empty() {
        fix.push("`penelope mem split <uid>` propose le découpage en un fait par entrée");
    }
    if let Some(w) = overflow {
        detail.push(w);
        fix.push(
            "la nuit range les nouveautés en notes et rétrograde une entrée par passe ; \
             `memory.core_budget_tokens` relève le budget",
        );
    }
    DoctorCheck::fail(ID, LABEL, detail.join(" ; "), Some(fix.join(" ; ")))
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

/// #256 : notes de forme vault dans le premier workspace, là où atterrit un chemin
/// relatif (`sources/…`). Zone morte : ni indexées ni versionnées. Les autres workspaces
/// (dépôts du propriétaire) ne sont pas parcourus : un `sources/` y est légitime.
pub async fn vault_dead_zone_check(s: &Services) -> DoctorCheck {
    const ID: &str = "vault.dead_zone";
    const LABEL: &str = "Notes du vault hors du vault";
    let vault = crate::helpers::canonical_workspace(&crate::helpers::vault_dir(s));
    let Some(ws) = crate::helpers::default_workspaces(s).into_iter().next() else {
        return DoctorCheck::ok(ID, LABEL, "aucun workspace");
    };
    if ws.starts_with(&vault) {
        return DoctorCheck::ok(ID, LABEL, "le workspace est dans le vault");
    }
    let found = {
        let (ws, vault) = (ws.clone(), vault.clone());
        tokio::task::spawn_blocking(move || dead_zone(&ws, &vault))
            .await
            .unwrap_or_default()
    };
    if found.is_empty() {
        return DoctorCheck::ok(ID, LABEL, format!("aucune dans {}", ws.display()));
    }
    let in_dir = |f: &String, d: &str| f.starts_with(&format!("{d}/"));
    let dirs = penelope_memory::wiki::VAULT_ONLY_DIRS;
    let mut fix: Vec<String> = dirs
        .iter()
        .filter(|d| found.iter().any(|f| in_dir(f, d)))
        .map(|d| {
            format!(
                "rsync -a --ignore-existing --remove-source-files {}/{d}/ {}/{d}/",
                ws.display(),
                vault.display()
            )
        })
        .collect();
    if found.iter().any(|f| !dirs.iter().any(|d| in_dir(f, d))) {
        fix.push("déplacer les autres notes nommées dans le vault".into());
    }
    fix.push("penelope mem reindex".into());
    DoctorCheck::fail(
        ID,
        LABEL,
        format!(
            "{} fichier(s) dans {}, ni indexés ni versionnés : {}{}",
            found.len(),
            ws.display(),
            found.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
            if found.len() > 5 { "…" } else { "" }
        ),
        Some(fix.join(" ; ")),
    )
}

/// Chemins relatifs au workspace des fichiers de forme vault, triés. Parcours borné, sans
/// dossiers cachés, ni le vault s'il est dedans.
fn dead_zone(ws: &std::path::Path, vault: &std::path::Path) -> Vec<String> {
    const MAX_DEPTH: usize = 8;
    const MAX_FILES: usize = 20_000;
    const MAX_NOTE_BYTES: u64 = 1024 * 1024;
    let mut found = Vec::new();
    let mut seen = 0usize;
    let mut stack = vec![(ws.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                if depth < MAX_DEPTH && !p.starts_with(vault) {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            seen += 1;
            if seen > MAX_FILES {
                stack.clear();
                break;
            }
            let Ok(rel) = p.strip_prefix(ws) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            let content = if rel.ends_with(".md") && meta.len() <= MAX_NOTE_BYTES {
                std::fs::read_to_string(&p).unwrap_or_default()
            } else {
                String::new()
            };
            if penelope_memory::wiki::vault_shaped(&rel, &content) {
                found.push(rel);
            }
        }
    }
    found.sort();
    found
}

/// Alias `embedding` joignable (issue #11) : sans lui, la recherche reste lexicale.
pub async fn embedding_check(emb: &crate::embeddings::Embedder) -> DoctorCheck {
    const ID: &str = "embedding";
    const LABEL: &str = "Embeddings (recherche par le sens)";
    let fix = Some(format!(
        "penelope config set models.aliases.embedding {}",
        penelope_kernel::config::DEFAULT_EMBEDDING_MODEL
    ));
    let Some(model) = crate::embeddings::model(&emb.services) else {
        return DoctorCheck::fail(ID, LABEL, "aucun modèle pour le rôle `embedding`", fix);
    };
    let texts = ["penelope doctor".to_string()];
    let probe = crate::embeddings::embed_texts(emb, &texts);
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
