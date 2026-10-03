use super::*;
use penelope_context::store::KIND_SKILL_LOADED;
use penelope_kernel::clock::TestClock;
use penelope_kernel::event::EventDraft;
use penelope_llm::types::ChatMessage;
use serde_json::json;
use std::sync::Arc;

struct World {
    _dir: tempfile::TempDir,
    clock: Arc<TestClock>,
    s: Arc<Services>,
    sid: String,
}

async fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock.clone())
            .await
            .unwrap(),
    );
    let sid = s
        .sessions
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string();
    World {
        _dir: dir,
        clock,
        s,
        sid,
    }
}

impl World {
    /// Un appel de conversation vient de partir : le cache est chaud.
    async fn called(&self) {
        self.s
            .budget
            .record(penelope_kernel::budget::UsageRecord {
                session_id: Some(self.sid.clone()),
                role: Some("chat".into()),
                model: "m".into(),
                provider: "mock".into(),
                ..Default::default()
            })
            .await
            .unwrap();
    }

    /// Un message du propriétaire, puis la préparation de son tour avec ces tuiles.
    async fn turn(&self, text: &str, mut tiers: Tiers) -> Tiers {
        self.s
            .context
            .history
            .append_queued(
                &self.sid,
                &ChatMessage::user(text),
                10,
                0,
                &penelope_context::journal::Provenance::queued(
                    penelope_context::journal::UserSource::Owner,
                    &format!("t-{text}"),
                    "2026-01-01T00:00:00Z",
                ),
            )
            .await
            .unwrap();
        let held = held_prefix(&self.s, &self.sid).await.unwrap();
        settle(&self.s, &self.sid, held, &mut tiers).await.unwrap();
        tiers
    }

    async fn events(&self, kind: &str) -> Vec<serde_json::Value> {
        let events = self.s.events.session_events(&self.sid, 0).await.unwrap();
        events
            .into_iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.payload)
            .collect()
    }

    /// Les blocs de contexte figés, dans l'ordre des messages.
    async fn blocks(&self) -> Vec<String> {
        self.events("conv.context")
            .await
            .iter()
            .map(|p| p["block"].as_str().unwrap().to_string())
            .collect()
    }
}

fn tiers(index: &str, context: &str) -> Tiers {
    Tiers {
        identity: "Tu es Pénélope.".into(),
        index: index.into(),
        context: context.into(),
        volatile: "Date et heure : lundi.".into(),
    }
}

/// T6 (épopée #208) : un préfixe modifié à cache chaud attend, sans entrer au
/// journal ; à cache froid, il y entre avec la raison `cold`.
#[tokio::test]
async fn a_changed_prefix_is_journaled_only_when_the_cache_is_cold() {
    let w = world().await;
    let systems = || async {
        w.events("conv.system")
            .await
            .iter()
            .map(|p| p["reason"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    w.turn("un", tiers("revue", "")).await;
    w.called().await;
    // Skill rechargée à cache chaud : le préfixe d'avant est gardé, rien de neuf.
    let warm = w.turn("deux", tiers("revue, deploiement", "")).await;
    assert_eq!(warm.index, "revue");
    assert_eq!(systems().await, ["first"]);
    // À cache froid, le nouveau préfixe sort et entre au journal.
    w.clock.advance_ms(CACHE_TTL_MS + 1);
    let cold = w.turn("trois", tiers("revue, deploiement", "")).await;
    assert_eq!(cold.index, "revue, deploiement");
    assert_eq!(systems().await, ["first", "cold"]);
}

/// #236 : AGENTS.md modifié entre deux tours à cache chaud. Le préfixe retenu part
/// inchangé, et le message suivant porte la différence, une seule fois ; un second
/// changement n'envoie que ce qui est nouveau depuis la première différence.
#[tokio::test]
async fn a_warm_prefix_change_is_sent_once_at_the_end() {
    let w = world().await;
    let fr = "## Contexte du workspace\nRéponds en français.\nTests obligatoires.\n";
    let en = "## Contexte du workspace\nRéponds en anglais.\nTests obligatoires.\n";
    let first = w.turn("un", tiers("idx", fr)).await;
    w.called().await;

    let second = w.turn("deux", tiers("idx", en)).await;
    assert_eq!(second.prefix_hash(), first.prefix_hash(), "préfixe gardé");
    let blocks = w.blocks().await;
    assert_eq!(blocks.len(), 2);
    assert!(!blocks[0].contains("<mise-a-jour>"), "{}", blocks[0]);
    assert!(blocks[1].starts_with("<contexte>\nDate et heure : lundi.\n\n<mise-a-jour>"));
    assert!(
        blocks[1].contains("- Réponds en français.\n+ Réponds en anglais.\n"),
        "{}",
        blocks[1]
    );
    let updates = w.events("prompt.updated").await;
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["changed"], json!(["T2"]));
    assert_eq!(updates[0]["base"], json!(first.prefix_hash()));

    // Même contenu au tour suivant : rien de plus.
    w.called().await;
    w.turn("trois", tiers("idx", en)).await;
    assert!(!w.blocks().await[2].contains("<mise-a-jour>"));
    assert_eq!(w.events("prompt.updated").await.len(), 1);

    // Un second changement : seule la ligne nouvelle part.
    let de = format!("{en}Commits signés.\n");
    w.called().await;
    let fourth = w.turn("quatre", tiers("idx", &de)).await;
    assert_eq!(fourth.prefix_hash(), first.prefix_hash());
    let last = w.blocks().await.pop().unwrap();
    assert!(last.contains("T2, contexte (AGENTS.md, mémoire) :\n+ Commits signés.\n"));
    assert!(!last.contains("français"), "{last}");
    assert_eq!(
        w.events("conv.system").await.len(),
        1,
        "pas de nouveau préfixe"
    );

    // À cache froid, le nouveau préfixe part en entier, sans différence.
    w.clock.advance_ms(CACHE_TTL_MS + 1);
    let cold = w.turn("cinq", tiers("idx", &de)).await;
    assert_ne!(cold.prefix_hash(), first.prefix_hash());
    assert!(!w.blocks().await.pop().unwrap().contains("<mise-a-jour>"));
}

/// Une reprise sans nouveau message (le dernier est déjà figé) ne compte pas la
/// différence comme envoyée : elle part avec le message suivant.
#[tokio::test]
async fn an_update_waits_for_a_message_to_carry_it() {
    let w = world().await;
    w.turn("un", tiers("idx", "A\n")).await;
    w.called().await;
    let held = held_prefix(&w.s, &w.sid).await.unwrap();
    let mut resumed = tiers("idx", "B\n");
    settle(&w.s, &w.sid, held, &mut resumed).await.unwrap();
    assert!(w.events("prompt.updated").await.is_empty());
    w.turn("deux", tiers("idx", "B\n")).await;
    assert_eq!(w.events("prompt.updated").await.len(), 1);
    assert!(w.blocks().await[1].contains("- A\n+ B\n"));
}

/// Une skill chargée puis modifiée se dit une fois, cache chaud ou froid.
#[tokio::test]
async fn a_loaded_skill_whose_body_changed_is_named_once() {
    let w = world().await;
    let root = w._dir.path().join("skills-test");
    let write = |body: &str| {
        std::fs::create_dir_all(root.join("revue")).unwrap();
        std::fs::write(
            root.join("revue/SKILL.md"),
            format!("---\nname: revue\ndescription: relire\n---\n{body}\n"),
        )
        .unwrap();
    };
    write("Relis tout.");
    w.s.skills.reload(None, &root, None).await.unwrap();
    let loaded = w.s.skills.get("revue").unwrap().body_hash;
    w.s.events
        .append(
            EventDraft::new(
                KIND_SKILL_LOADED,
                json!({"name": "revue", "body_hash": loaded}),
            )
            .session(&w.sid),
        )
        .await
        .unwrap();
    w.turn("un", tiers("idx", "")).await;
    assert!(!w.blocks().await[0].contains("<mise-a-jour>"));

    write("Relis tout, deux fois.");
    w.s.skills.reload(None, &root, None).await.unwrap();
    w.clock.advance_ms(CACHE_TTL_MS + 1);
    w.turn("deux", tiers("idx", "")).await;
    assert!(w.blocks().await[1].contains("Skill `revue` modifiée"));
    w.turn("trois", tiers("idx", "")).await;
    assert!(
        !w.blocks().await[2].contains("<mise-a-jour>"),
        "une seule fois"
    );
}

/// #302 : une note laissée hors tour (un run lancé depuis la carte de plan) part dans le
/// contexte volatil du message suivant, une seule fois.
#[tokio::test]
async fn a_pending_notice_is_sent_once_with_the_next_message() {
    let w = world().await;
    penelope_app::notices::push(&w.s, &w.sid, "Run `r_plan_1` lancé.")
        .await
        .unwrap();
    w.turn("un", tiers("idx", "")).await;
    let blocks = w.blocks().await;
    assert!(
        blocks[0].contains("<evenements>") && blocks[0].contains("- Run `r_plan_1` lancé."),
        "{}",
        blocks[0]
    );
    assert!(
        penelope_app::notices::peek(&w.s, &w.sid)
            .await
            .unwrap()
            .is_empty(),
        "retirée une fois partie"
    );
    w.turn("deux", tiers("idx", "")).await;
    assert!(!w.blocks().await[1].contains("<evenements>"));
}
