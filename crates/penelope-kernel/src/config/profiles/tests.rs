use super::*;

const FLASH: &str = "openrouter:deepseek/deepseek-v4.1-flash";
const SOL: &str = "codex:gpt-5.6-sol";
const GEMINI: &str = "openrouter:google/gemini-3.1-flash-image";
const EMBED: &str = "openrouter:openai/text-embedding-3-small";
const PARAKEET: &str = "openai_compat:mlx-community/parakeet-tdt-0.6b-v3";
const QWEN: &str = "local:mlx-community/Qwen3-1.7B-4bit";

/// La configuration de l'instance du propriétaire au 07/10, telle que le fichier la porte :
/// `main` sur Codex, les autres alias sur DeepSeek Flash, images et vision sur Gemini,
/// embeddings sur OpenRouter, transcription sur l'endpoint `extra.mlx`, narrateur local,
/// aucun repli pour `main`, synthèse Voxtral par défaut.
pub(crate) fn owner_config() -> Config {
    let (cfg, unknown) = Config::parse(&owner_toml()).expect("configuration lisible");
    assert!(unknown.is_empty(), "{unknown:?}");
    cfg.validate().expect("configuration valide");
    cfg
}

fn owner_toml() -> String {
    format!(
        r#"
[owner]
telegram_user_id = 1

[providers.local]
enabled = false

[providers.extra.mlx]
enabled = true
base_url = "http://127.0.0.1:8081/v1"
models = ["mlx-community/parakeet-tdt-0.6b-v3", "mlx-community/Qwen3-1.7B-4bit"]

[models.aliases]
main = "{SOL}"
fast = "{FLASH}"
reasoning = "{FLASH}"
summarizer = "{FLASH}"
juge = "{FLASH}"
memoire = "{FLASH}"
pointage = "{GEMINI}"
vision = "{GEMINI}"
image = "{GEMINI}"
embedding = "{EMBED}"
stt = "{PARAKEET}"
tts = "{DEFAULT_TTS_MODEL}"
narrateur = "{QWEN}"

[models.roles]
chat_default = "main"
classifier = "fast"
compaction = "summarizer"
memory_review = "fast"
approval_judge = "fast"
code = "reasoning"
image_generate = "image"
image_describe = "vision"
image_locate = "vision"
embedding = "embedding"
stt = "stt"
tts = "tts"
trace = "narrateur"

[models.routing]
classifier = true
low = "fast"
medium = "main"
high = "reasoning"

[models.routing.fallback]
main = []
"#
    )
}

fn table(cfg: &Config) -> Vec<(String, String, ModelReason)> {
    cfg.resolve_all(&NoCaps)
        .into_iter()
        .map(|r| (r.role, r.model.unwrap_or_default(), r.reason))
        .collect()
}

fn row(role: &str, model: &str, reason: ModelReason) -> (String, String, ModelReason) {
    (role.to_string(), model.to_string(), reason)
}

/// #332 : la migration est transparente. La configuration du propriétaire, sans aucun
/// profil écrit, donne rôle par rôle le modèle que l'ancien code choisissait : rien ne
/// change avant qu'il touche à ses profils, et la garde Codex reste `deny`.
#[test]
fn the_owner_config_resolves_as_before_the_profiles() {
    use ModelReason::*;
    let cfg = owner_config();
    assert_eq!(cfg.models.active_name(), DEFAULT_PROFILE);
    assert!(cfg.models.is_derived(DEFAULT_PROFILE));
    assert_eq!(
        table(&cfg),
        vec![
            row("chat_default", SOL, Primary),
            row("classifier", FLASH, Override),
            // Avant : `models.roles.title`, sinon `fast`.
            row("title", FLASH, Override),
            // Avant : `chat_default` pour une étape sans modèle ni rôle connu.
            row("workflow", SOL, Primary),
            row("code", FLASH, Override),
            // Avant : le rôle `compaction` servait la consolidation.
            row("dream", FLASH, Override),
            row("memory_review", FLASH, Override),
            row("compaction", FLASH, Override),
            row("approval_judge", FLASH, Override),
            row("image_generate", GEMINI, Capability),
            row("image_describe", GEMINI, Capability),
            row("image_locate", GEMINI, Capability),
            row("embedding", EMBED, Capability),
            row("stt", PARAKEET, Local),
            row("tts", DEFAULT_TTS_MODEL, Local),
            row("trace", QWEN, Local),
        ]
    );
    // Le classifieur garde ses trois étages, et `main` reste sans repli.
    assert!(cfg.adaptive_routing());
    assert_eq!(cfg.routing_label(Tier::Low), "fast");
    assert_eq!(cfg.routing_label(Tier::Medium), "main");
    assert_eq!(cfg.routing_label(Tier::High), "reasoning");
    assert!(cfg.fallback_labels("main").is_empty());
    assert!(!cfg.codex_background_allowed());
    // `juge`, `memoire` et `pointage` ne sont lus par rien : signalés, pas convertis.
    assert_eq!(
        cfg.models.unused_aliases(&cfg.voice),
        ["juge", "memoire", "pointage"]
    );
}

/// Le profil que le propriétaire veut : Codex partout, la voix en local, les images sur
/// OpenRouter, la garde levée.
fn codex_profile() -> ModelProfile {
    ModelProfile {
        primary: SOL.into(),
        codex_background: CODEX_ALLOW.into(),
        capabilities: [("image_generate", GEMINI), ("vision", GEMINI)]
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
        ..Default::default()
    }
}

/// #332 : un profil « Codex (crédits) » fait tout passer par le principal ; seules les
/// capacités qu'il ne sait pas et la voix vont ailleurs.
#[test]
fn a_codex_profile_sends_everything_to_the_primary() {
    use ModelReason::*;
    let mut cfg = owner_config();
    cfg.models
        .profiles
        .insert("Codex (crédits)".into(), codex_profile());
    cfg.models.profile = "Codex (crédits)".into();
    cfg.validate().unwrap();
    assert_eq!(
        table(&cfg),
        vec![
            row("chat_default", SOL, Primary),
            row("classifier", SOL, Primary),
            row("title", SOL, Primary),
            row("workflow", SOL, Primary),
            row("code", SOL, Primary),
            row("dream", SOL, Primary),
            row("memory_review", SOL, Primary),
            row("compaction", SOL, Primary),
            row("approval_judge", SOL, Primary),
            row("image_generate", GEMINI, Capability),
            row("image_describe", GEMINI, Capability),
            row("image_locate", GEMINI, Capability),
            // Aucun principal ne sait faire d'embeddings : le modèle livré.
            row("embedding", DEFAULT_EMBEDDING_MODEL, Capability),
            row("stt", PARAKEET, Local),
            row("tts", DEFAULT_TTS_MODEL, Local),
            row("trace", QWEN, Local),
        ]
    );
    // Les trois étages tombent sur le principal : plus d'appel au classifieur.
    assert!(!cfg.adaptive_routing());
    assert!(cfg.codex_background_allowed());

    // Bascule : le profil déduit revient tel quel, sans redémarrage ni perte.
    cfg.models.profile = DEFAULT_PROFILE.into();
    assert_eq!(table(&cfg), table(&owner_config()));
}

/// #332 : une surcharge prime sur tout ; une capacité non posée revient au principal
/// quand le catalogue dit qu'il sait faire, au modèle livré sinon.
#[test]
fn overrides_win_and_capabilities_ask_the_catalog() {
    struct Sees;
    impl ModelCaps for Sees {
        fn supports(&self, model: &str, cap: Capability) -> Option<bool> {
            Some(model == SOL && cap == Capability::Vision)
        }
    }
    let mut cfg = owner_config();
    let mut p = ModelProfile {
        primary: SOL.into(),
        ..Default::default()
    };
    p.overrides.insert("classifier".into(), "fast".into());
    cfg.models.profiles.insert("x".into(), p);
    cfg.models.profile = "x".into();
    let r = cfg.resolve_role_with("classifier", &Sees);
    assert_eq!(
        (r.label.as_str(), r.reason),
        ("fast", ModelReason::Override)
    );
    assert_eq!(r.model.as_deref(), Some(FLASH));
    let r = cfg.resolve_role_with("image_describe", &Sees);
    assert_eq!(
        (r.model.as_deref(), r.reason),
        (Some(SOL), ModelReason::Primary)
    );
    let r = cfg.resolve_role("image_describe");
    assert_eq!(
        (r.model.as_deref(), r.reason),
        (Some(DEFAULT_IMAGE_MODEL), ModelReason::Capability)
    );
    // Un rôle que personne ne connaît suit le principal : plus de repli codé en dur.
    assert_eq!(cfg.role_model("veille").as_deref(), Some(SOL));
}

/// #332 : modifier le profil déduit l'écrit en entier ; le fichier garde alors tout ce
/// qui était déduit, et la relecture donne la même table.
#[test]
fn materializing_the_derived_profile_writes_it_whole() {
    let before = owner_config();
    let mut after = before.clone();
    after
        .models
        .materialize(DEFAULT_PROFILE)
        .unwrap()
        .codex_background = CODEX_ALLOW.into();
    let text = crate::config::edit_toml(&owner_toml(), &before, &after).unwrap();
    assert!(text.contains("[models.profiles.defaut]"), "{text}");
    let (reread, _) = Config::parse(&text).unwrap();
    assert!(!reread.models.is_derived(DEFAULT_PROFILE));
    assert!(reread.codex_background_allowed());
    assert_eq!(table(&reread), table(&before));
}

/// Un profil qui cite un modèle illisible, une garde inconnue ou un profil actif absent
/// est refusé en le nommant.
#[test]
fn a_bad_profile_is_refused() {
    let mut cfg = owner_config();
    cfg.models.profile = "absent".into();
    assert!(cfg.validate().unwrap_err().to_string().contains("absent"));
    let mut cfg = owner_config();
    let mut p = codex_profile();
    p.codex_background = "peut-être".into();
    cfg.models.profiles.insert("x".into(), p);
    assert!(
        cfg.validate()
            .unwrap_err()
            .to_string()
            .contains("codex_background")
    );
    let mut cfg = owner_config();
    let mut p = codex_profile();
    p.overrides.insert("classifier".into(), "fantome".into());
    cfg.models.profiles.insert("x".into(), p);
    assert!(cfg.validate().unwrap_err().to_string().contains("fantome"));
}
