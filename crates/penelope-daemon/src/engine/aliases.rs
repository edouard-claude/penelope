//! Alias de modèle proposés pour la conversation (`/model`).

/// Alias proposés pour la conversation : les étages du routage et le rôle
/// `chat_default`, puis les alias ajoutés à la main, jamais ceux réservés à la
/// compaction, aux images, aux embeddings ou à la transcription.
pub fn conversation_aliases(cfg: &penelope_kernel::config::Config) -> Vec<String> {
    const NOT_CHAT: &[&str] = &[
        "compaction",
        "image_generate",
        "image_describe",
        "embedding",
        "stt",
    ];
    let reserved: std::collections::BTreeSet<&str> = cfg
        .models
        .roles
        .iter()
        .filter(|(role, _)| NOT_CHAT.contains(&role.as_str()))
        .map(|(_, alias)| alias.as_str())
        .collect();
    let routing = &cfg.models.routing;
    let mut out: Vec<String> = Vec::new();
    for a in [
        routing.low.clone(),
        routing.medium.clone(),
        routing.high.clone(),
        cfg.role_alias("chat_default"),
    ] {
        if cfg.alias_model(&a).is_some() && !out.contains(&a) {
            out.push(a);
        }
    }
    for a in cfg.models.aliases.keys() {
        if !reserved.contains(a.as_str()) && !out.contains(a) {
            out.push(a.clone());
        }
    }
    out
}
