use super::*;

/// #105 : la réponse reprend ce que le souvenir apportait, pas ce que la question
/// disait déjà.
#[test]
fn a_memory_is_useful_when_the_answer_uses_what_it_brought() {
    let entry = "Martin habite à Lyon depuis 2024.";
    assert!(used_in_answer(
        entry,
        "Où habite Martin ?",
        "Martin est à Lyon."
    ));
    assert!(!used_in_answer(
        entry,
        "Où habite Martin ?",
        "Je ne sais pas où habite Martin."
    ));
    let long = "Le déploiement de production passe par CapRover sur le serveur de                     Gravelines, avec une sauvegarde nocturne vers le stockage objet.";
    assert!(!used_in_answer(
        long,
        "comment on déploie ?",
        "Par le serveur."
    ));
    assert!(used_in_answer(
        long,
        "comment on déploie ?",
        "Avec CapRover, sur Gravelines."
    ));
    assert!(!used_in_answer("", "question", "réponse"));
}
use crate::index::{IndexedEntry, simple_entry};
use crate::provenance::Provenance;
use penelope_kernel::clock::TestClock;
use penelope_store::Store;
use std::sync::Arc;

fn index() -> MemoryIndex {
    MemoryIndex::new(
        Store::open_memory().unwrap(),
        Arc::new(TestClock::default()),
    )
}

fn prov() -> Provenance {
    Provenance::owner("s1", "interactive", "2026-09-16T10:00:00Z")
}

const PRACTICE: &str = "---\n\
        type: pratique\n\
        id: langage-backend\n\
        confiance: 0.8\n\
        preuves: 9\n\
        statut: active\n\
        maj: 2026-09-17\n\
        declencheurs: [langage, stack, backend]\n\
        ---\n\
        # Langage backend\n\n\
        ## Défaut\n- Go (stdlib, hexagonal). <!-- uid: 01J9A -->\n\n\
        ## Exceptions\n\
        - Rust si projet critique. <!-- uid: 01J9B --> \
          <!-- quand: tache=code; criticite=haute; codeur=agent --> <!-- confiance: 0.9 -->\n";

#[test]
fn project_keys_are_normalised() {
    assert_eq!(
        project_key(Some("git@github.com:org/repo.git"), "/x"),
        "github.com/org/repo"
    );
    assert_eq!(
        project_key(Some("https://github.com/org/repo"), "/x"),
        "github.com/org/repo"
    );
    assert_eq!(
        project_key(None, "/Users/moi/Code/projet/"),
        "/users/moi/code/projet"
    );
}

#[test]
fn active_projects_are_lru_bounded_to_four() {
    let mut c = CurrentContext::default();
    for i in 0..6 {
        c.touch_project(&format!("p{i}"));
    }
    assert_eq!(c.active_projects.len(), 4);
    assert_eq!(c.active_projects[0], "p5");
    assert!(!c.active_projects.contains(&"p0".to_string()));

    c.touch_project("p2");
    assert_eq!(c.active_projects[0], "p2", "un projet réutilisé remonte");
    assert_eq!(c.active_projects.len(), 4);
}

#[test]
fn task_classification() {
    assert_eq!(
        classify_task("déploie en prod stp").as_deref(),
        Some("deploiement")
    );
    assert_eq!(
        classify_task("relis ma pull request").as_deref(),
        Some("revue")
    );
    assert_eq!(
        classify_task("corrige le bug de TVA").as_deref(),
        Some("code")
    );
    assert_eq!(classify_task("bonjour").as_deref(), None);
}

#[test]
fn recall_intent_detection() {
    assert!(shows_recall_intent("on avait dit quoi pour les branches ?"));
    assert!(shows_recall_intent("rappelle-moi la procédure"));
    assert!(shows_recall_intent("pourquoi on a choisi Go ?"));
    assert!(!shows_recall_intent("corrige ce bug"));
}

#[tokio::test]
async fn path1_injects_triggered_entries_under_budget() {
    let i = index();
    for n in 0..10 {
        let mut e = simple_entry(
            &format!("u{n}"),
            &format!("Le déploiement du service {n} se fait par CapRover."),
            Level::Cure,
            "2026-09-16",
        );
        e.importance = Some(9);
        i.upsert(&e, &prov()).await.unwrap();
    }
    let r = Recall::new(&i, RecallParams::default())
        .path1(
            "comment se fait le déploiement ?",
            &CurrentContext::default(),
            None,
            &[],
        )
        .await;
    assert!(!r.triggered.is_empty());
    assert!(
        r.triggered.len() <= 3,
        "au plus 3 entrées injectées : {}",
        r.triggered.len()
    );
    assert!(r.tokens_used <= 1000);
    assert!(r.degraded, "sans vecteur, la voie 1 est en mode FTS seul");
}

/// Entrée de niveau Cure, importance 5 (défaut d'un candidat), datée de `maj`.
fn aged(uid: &str, text: &str, maj: &str) -> IndexedEntry {
    let mut e = simple_entry(uid, text, Level::Cure, maj);
    e.file = "notes.md".into();
    e.importance = Some(5);
    e
}

/// #86 : l'horloge de test est au 1er janvier 2026. Seule réponse lexicale à la
/// question, une entrée de 30, 90 ou 180 jours est toujours injectée : la récence
/// ordonne, elle ne décide pas seule.
#[tokio::test]
async fn an_old_curated_entry_is_still_recalled() {
    for (maj, age) in [("2025-12-02", 30), ("2025-10-03", 90), ("2025-07-05", 180)] {
        let i = index();
        i.upsert(
            &aged("vieux", "Le code du portail de la résidence est 4812.", maj),
            &prov(),
        )
        .await
        .unwrap();
        let r = Recall::new(&i, RecallParams::default())
            .path1(
                "quel est le code du portail ?",
                &CurrentContext::default(),
                None,
                &[],
            )
            .await;
        assert_eq!(r.triggered.len(), 1, "âgée de {age} jours");
        assert!(r.seen.is_empty());
    }
}

/// #86 : à pertinence égale, la récente passe avant l'ancienne.
#[tokio::test]
async fn a_recent_entry_ranks_before_an_old_equivalent() {
    let i = index();
    i.upsert(
        &aged(
            "ancien",
            "Le portail de la résidence a pour code 4812.",
            "2025-07-05",
        ),
        &prov(),
    )
    .await
    .unwrap();
    i.upsert(
        &aged(
            "recent",
            "Le portail de la résidence a pour code 7731.",
            "2025-12-30",
        ),
        &prov(),
    )
    .await
    .unwrap();
    let r = Recall::new(&i, RecallParams::default())
        .path1(
            "le code du portail de la résidence",
            &CurrentContext::default(),
            None,
            &[],
        )
        .await;
    let uids: Vec<&str> = r.triggered.iter().map(|t| t.entry.uid.as_str()).collect();
    assert_eq!(uids, vec!["recent", "ancien"]);
}

/// #86 : `memory.half_life_days` change l'ordre entre une entrée ancienne importante
/// et une récente ordinaire.
#[tokio::test]
async fn the_half_life_setting_changes_the_order() {
    let half = Arc::new(std::sync::Mutex::new(10.0));
    let i = {
        let half = half.clone();
        index().with_half_life(move || *half.lock().unwrap())
    };
    let mut ancien = aged(
        "ancien",
        "Le portail de la résidence a pour code 4812.",
        "2025-11-02",
    );
    ancien.importance = Some(10);
    let mut recent = aged(
        "recent",
        "Le portail de la résidence a pour code 7731.",
        "2025-12-31",
    );
    recent.importance = Some(1);
    i.upsert(&ancien, &prov()).await.unwrap();
    i.upsert(&recent, &prov()).await.unwrap();
    let first = |i: MemoryIndex| async move {
        i.search(
            "portail résidence code",
            None,
            &SearchFilter::explicit(),
            &[],
        )
        .await
        .unwrap()[0]
            .entry
            .uid
            .clone()
    };
    assert_eq!(
        first(i.clone()).await,
        "recent",
        "demi-vie courte : la récente"
    );
    *half.lock().unwrap() = 365.0;
    assert_eq!(
        first(i.clone()).await,
        "ancien",
        "demi-vie longue : l'importante"
    );
}

/// #86 : ce qui n'est jamais apparu dans un résultat n'est pas proposé au retrait ;
/// ce qui y est apparu dix fois sans être retenu l'est.
#[tokio::test]
async fn only_entries_that_had_their_chance_are_proposed_for_retirement() {
    let i = index();
    i.upsert(
        &aged(
            "jamais_vu",
            "Le wifi du garage est Garage-5G.",
            "2025-06-01",
        ),
        &prov(),
    )
    .await
    .unwrap();
    i.upsert(
        &aged("vu", "La haie se taille en mars.", "2025-06-01"),
        &prov(),
    )
    .await
    .unwrap();
    for _ in 0..crate::grid::SEEN_BEFORE_RETIRE {
        i.record_seen(&["vu".to_string()]).await.unwrap();
    }
    let proposed: Vec<String> = i
        .unrecalled_since(
            &[Level::Cure],
            "2025-11-01",
            crate::grid::SEEN_BEFORE_RETIRE,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.uid)
        .collect();
    assert_eq!(proposed, vec!["vu"]);
    assert_eq!(i.signals_of("vu").await.unwrap().seen, 10);
}

/// #87 : mille passages de documents ingérés, plus proches de la question que le
/// souvenir, ne le chassent plus du rappel automatique ; leurs vecteurs ne sont même
/// pas décodés. Le même souvenir en épisodique n'est pas injecté, `mem_search`
/// explicite le trouve.
#[tokio::test]
async fn ingested_passages_do_not_evict_memories_from_recall() {
    let i = index();
    for n in 0..1000 {
        let uid = format!("p{n:04}");
        let mut e = simple_entry(
            &uid,
            &format!("Le code du portail de la résidence, le portail, son code : section {n}."),
            Level::Cure,
            "2026-01-01",
        );
        e.etype = crate::ingest::SOURCE_ETYPE.into();
        e.file = format!("sources/reglement-{}.md", n / 100);
        i.upsert(&e, &prov()).await.unwrap();
        i.put_embedding(&uid, "m", &[1.0, 0.01 * (n % 10) as f32, 0.0])
            .await
            .unwrap();
    }
    let mut souvenir = simple_entry(
        "souvenir",
        "Pour entrer chez Paul, taper le code du portail : 4812.",
        Level::Cure,
        "2025-12-20",
    );
    souvenir.file = "notes.md".into();
    i.upsert(&souvenir, &prov()).await.unwrap();
    i.put_embedding("souvenir", "m", &[0.6, 0.8, 0.0])
        .await
        .unwrap();
    let mut journal = simple_entry(
        "journal",
        "Paul a redonné le code du portail de sa résidence.",
        Level::Episodic,
        "2025-12-21",
    );
    journal.file = "journal/2025-12-21.md".into();
    i.upsert(&journal, &prov()).await.unwrap();
    i.put_embedding("journal", "m", &[0.6, 0.8, 0.0])
        .await
        .unwrap();

    let before = i.vectors_decoded();
    let params = RecallParams {
        timeout_ms: 5_000,
        ..RecallParams::default()
    };
    let r = Recall::new(&i, params)
        .path1(
            "quel est le code du portail de la résidence ?",
            &CurrentContext::default(),
            Some(vec![1.0, 0.0, 0.0]),
            &[],
        )
        .await;
    let injected: Vec<&str> = r.triggered.iter().map(|t| t.entry.uid.as_str()).collect();
    assert_eq!(injected, vec!["souvenir"]);
    assert_eq!(
        i.vectors_decoded() - before,
        1,
        "ni passage ni épisodique décodé pour le rappel automatique"
    );

    let explicit = i
        .search("code portail Paul", None, &SearchFilter::explicit(), &[])
        .await
        .unwrap();
    assert!(explicit.iter().any(|h| h.entry.uid == "journal"));
}

#[tokio::test]
async fn path1_never_injects_episodic_or_low_score() {
    let i = index();
    let mut e = simple_entry(
        "u1",
        "note du journal sur le déploiement",
        Level::Episodic,
        "2026-09-16",
    );
    e.file = "journal/2026-09-16.md".into();
    i.upsert(&e, &prov()).await.unwrap();

    let r = Recall::new(&i, RecallParams::default())
        .path1("déploiement", &CurrentContext::default(), None, &[])
        .await;
    assert!(r.triggered.is_empty());
}

/// CA 6 (rappel sans blocage) : backend d'embeddings indisponible ⇒ la voie 1
/// fonctionne en FTS seul et ne retarde pas la réponse.
#[tokio::test]
async fn ca_6_12_recall_never_blocks() {
    let i = index();
    i.upsert(
        &simple_entry(
            "u1",
            "Le déploiement passe par CapRover.",
            Level::Cure,
            "2026-09-16",
        ),
        &prov(),
    )
    .await
    .unwrap();

    let started = std::time::Instant::now();
    let r = Recall::new(
        &i,
        RecallParams {
            timeout_ms: 150,
            ..Default::default()
        },
    )
    .path1("déploiement", &CurrentContext::default(), None, &[])
    .await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(600),
        "le rappel a pris {elapsed:?}"
    );
    assert!(r.degraded);
}

#[tokio::test]
async fn practice_recall_is_context_dependent() {
    let i = index();
    let p = Practice::parse(PRACTICE, "langage-backend").unwrap();
    let recall = Recall::new(&i, RecallParams::default());

    let mut ctx = CurrentContext {
        tache: Some("code".into()),
        criticite: Some("haute".into()),
        codeur: Some("agent".into()),
        ..Default::default()
    };
    ctx.touch_project("penelope");

    let r = recall
        .path1(
            "quel langage pour le backend ?",
            &ctx,
            None,
            std::slice::from_ref(&p),
        )
        .await;
    assert_eq!(r.practices.len(), 1);
    let rendered = r.render();
    assert!(rendered.contains("Défaut : Go"));
    assert!(rendered.contains("S'applique ici : Rust"));

    // Tour de rédaction : le défaut seul.
    let ctx2 = CurrentContext {
        tache: Some("redaction".into()),
        criticite: Some("haute".into()),
        codeur: Some("agent".into()),
        ..Default::default()
    };
    let r2 = recall
        .path1(
            "quel langage pour le backend ?",
            &ctx2,
            None,
            std::slice::from_ref(&p),
        )
        .await;
    assert!(r2.render().contains("Défaut : Go"));
    assert!(!r2.render().contains("S'applique ici"));
}

#[tokio::test]
async fn practice_not_triggered_is_not_injected() {
    let i = index();
    let p = Practice::parse(PRACTICE, "langage-backend").unwrap();
    let r = Recall::new(&i, RecallParams::default())
        .path1(
            "comment va la météo ?",
            &CurrentContext::default(),
            None,
            &[p],
        )
        .await;
    assert!(r.practices.is_empty());
}

#[tokio::test]
async fn escalation_requires_intent_and_weak_path1() {
    let i = index();
    let weak = Recall::new(&i, RecallParams::default())
        .path1("on avait dit quoi ?", &CurrentContext::default(), None, &[])
        .await;
    assert!(weak.should_escalate("on avait dit quoi ?", 0.72));
    assert!(
        !weak.should_escalate("corrige ce bug", 0.72),
        "sans intention de rappel, pas d'escalade"
    );

    let strong = RecallResult {
        max_score: 0.95,
        ..Default::default()
    };
    assert!(
        !strong.should_escalate("on avait dit quoi ?", 0.72),
        "la voie 1 a répondu : pas d'escalade"
    );
}

#[test]
fn snapshots_hash_is_stable() {
    let a = Snapshots::compute("p", "c", "j");
    let b = Snapshots::compute("p", "c", "j");
    assert_eq!(a.hash, b.hash);
    assert_ne!(a.hash, Snapshots::compute("p2", "c", "j").hash);
}

#[test]
fn snapshot_block_respects_budget_and_importance() {
    let entries: Vec<IndexedEntry> = (0..20)
        .map(|i| {
            let mut e = simple_entry(
                &format!("u{i:02}"),
                &format!("entrée numéro {i:02} avec un texte de longueur raisonnable"),
                Level::Profil,
                "2026-09-16",
            );
            // Importance croissante : la dernière est la plus importante.
            e.importance = Some(i as u8);
            e
        })
        .collect();
    let block = Snapshots::build_block(&entries, 30);
    let tokens: u64 = block
        .lines()
        .map(|l| (l.chars().count() as u64 / 4).max(1))
        .sum();
    assert!(tokens <= 35, "budget dépassé : {tokens}");
    assert!(
        block.lines().next().unwrap().contains("numéro 19"),
        "la plus importante d'abord : {block}"
    );
    assert!(
        !block.contains("numéro 00"),
        "la moins importante est écartée par le budget"
    );
}

#[test]
fn predicates_are_lowercased_and_sparse() {
    let c = CurrentContext {
        tache: Some("Code".into()),
        criticite: Some("Haute".into()),
        ..Default::default()
    };
    let p = c.predicates();
    assert_eq!(p["tache"], "code");
    assert_eq!(p["criticite"], "haute");
    assert!(
        !p.contains_key("client"),
        "les clés absentes ne sont pas posées"
    );
    let w = crate::vault::When::parse("tache=code").unwrap();
    assert_eq!(w.evaluate(&p), crate::vault::WhenMatch::Satisfied);
}
