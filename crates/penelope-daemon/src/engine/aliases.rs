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
    let reserved: std::collections::BTreeSet<String> =
        NOT_CHAT.iter().map(|role| cfg.role_alias(role)).collect();
    use penelope_kernel::config::Tier;
    let mut out: Vec<String> = Vec::new();
    for a in [
        cfg.routing_label(Tier::Low),
        cfg.routing_label(Tier::Medium),
        cfg.routing_label(Tier::High),
        cfg.primary_label(),
    ] {
        if cfg.alias_model(&a).is_some() && !out.contains(&a) {
            out.push(a);
        }
    }
    for a in cfg.models.aliases.keys() {
        if !reserved.contains(a) && !out.contains(a) {
            out.push(a.clone());
        }
    }
    out
}
