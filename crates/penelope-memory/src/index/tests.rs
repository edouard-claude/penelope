use super::*;
use penelope_kernel::clock::TestClock;
use std::sync::Arc;

fn index(clock: TestClock) -> MemoryIndex {
    MemoryIndex::new(Store::open_memory().unwrap(), Arc::new(clock))
}

fn prov() -> Provenance {
    Provenance::owner("s1", "interactive", "2026-09-16T10:00:00Z")
}

#[test]
fn scoring_factors_follow_the_prd_table() {
    assert!((decay(0.0, 30.0, false) - 1.0).abs() < 1e-9);
    assert!((decay(30.0, 30.0, false) - 0.5).abs() < 1e-9);
    assert!((decay(60.0, 30.0, false) - 0.25).abs() < 1e-9);
    assert_eq!(decay(365.0, 30.0, true), 1.0, "un épinglé ne décroît pas");

    assert!((importance_factor(Some(10)) - 1.0).abs() < 1e-9);
    assert!((importance_factor(Some(0)) - 0.5).abs() < 1e-9);
    assert_eq!(importance_factor(None), 1.0);

    let active = vec!["penelope".to_string()];
    assert!((project_factor(Some("penelope"), &active) - 1.3).abs() < 1e-9);
    assert!((project_factor(Some("autre"), &active) - 0.85).abs() < 1e-9);
    assert_eq!(project_factor(None, &active), 1.0);

    assert!((confidence_factor(Some(0.8)) - 0.9).abs() < 1e-9);
    assert_eq!(confidence_factor(None), 1.0);
}

#[test]
fn rrf_rewards_being_in_both_lists() {
    let both = rrf(Some(0), Some(0), 60.0);
    let one = rrf(Some(0), None, 60.0);
    assert!(both > one);
    assert!(rrf(Some(0), None, 60.0) > rrf(Some(10), None, 60.0));
    assert_eq!(rrf(None, None, 60.0), 0.0);
}

#[tokio::test]
async fn upsert_and_get() {
    let i = index(TestClock::default());
    let e = simple_entry(
        "u1",
        "Le déploiement passe par CapRover.",
        Level::Coeur,
        "2026-09-16",
    );
    i.upsert(&e, &prov()).await.unwrap();
    let back = i.get("u1").await.unwrap().unwrap();
    assert_eq!(back.text, e.text);
    assert!(back.pinned);
    assert_eq!(i.origin_of("u1").await.unwrap(), Some(Origin::Owner));
}

#[tokio::test]
async fn provenance_is_written_once_and_never_upgraded() {
    let i = index(TestClock::default());
    let e = simple_entry("u1", "texte", Level::Cure, "2026-09-16");
    let untrusted = Provenance {
        origin: Origin::Untrusted,
        session_kind: "interactive".into(),
        observed_at: "t".into(),
        supersedes_uid: None,
        source_ref: None,
        session_id: Some("s1".into()),
    };
    i.upsert(&e, &untrusted).await.unwrap();
    // Une seconde écriture prétendant `owner` ne doit pas écraser la provenance.
    i.upsert(&e, &prov()).await.unwrap();
    assert_eq!(
        i.origin_of("u1").await.unwrap(),
        Some(Origin::Untrusted),
        "la provenance est fixée à la première écriture"
    );
}

#[tokio::test]
async fn fts_search_finds_entries() {
    let i = index(TestClock::default());
    for (uid, text) in [
        ("u1", "Le déploiement se fait par CapRover en production."),
        ("u2", "Les revues de code passent par une pull request."),
    ] {
        i.upsert(&simple_entry(uid, text, Level::Cure, "2026-09-16"), &prov())
            .await
            .unwrap();
    }
    let hits = i
        .search("deploiement", None, &SearchFilter::explicit(), &[])
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entry.uid, "u1");
    assert!(hits[0].fts_rank.is_some());
}

#[tokio::test]
async fn vector_search_complements_fts() {
    let i = index(TestClock::default());
    i.upsert(
        &simple_entry("u1", "mise en ligne du service", Level::Cure, "2026-09-16"),
        &prov(),
    )
    .await
    .unwrap();
    i.put_embedding("u1", "m", &[1.0, 0.0, 0.0]).await.unwrap();

    // Le mot « déploiement » n'apparaît pas : seul le vecteur peut trouver.
    let hits = i
        .search(
            "déploiement",
            Some(vec![0.95, 0.1, 0.0]),
            &SearchFilter::explicit(),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].vec_rank.is_some());
    assert!(hits[0].similarity > 0.9);
}

#[tokio::test]
async fn active_project_entries_rank_higher() {
    let clock = TestClock::default();
    let i = index(clock);
    for (uid, projet) in [("u1", "penelope"), ("u2", "autre")] {
        let mut e = simple_entry(
            uid,
            "la convention de nommage des branches",
            Level::Cure,
            "2026-09-16",
        );
        e.projet = Some(projet.to_string());
        i.upsert(&e, &prov()).await.unwrap();
    }
    let hits = i
        .search(
            "convention nommage",
            None,
            &SearchFilter::explicit(),
            &["penelope".to_string()],
        )
        .await
        .unwrap();
    assert_eq!(hits[0].entry.uid, "u1", "le projet actif remonte");
}

#[tokio::test]
async fn episodic_is_excluded_unless_explicitly_requested() {
    let i = index(TestClock::default());
    i.upsert(
        &{
            let mut e = simple_entry(
                "u1",
                "note du journal sur le déploiement",
                Level::Episodic,
                "2026-09-16",
            );
            e.file = "journal/2026-09-16.md".into();
            e
        },
        &prov(),
    )
    .await
    .unwrap();

    let implicit = SearchFilter {
        limit: 10,
        ..Default::default()
    };
    assert!(
        i.search("deploiement", None, &implicit, &[])
            .await
            .unwrap()
            .is_empty(),
        "l'épisodique n'est jamais injecté automatiquement"
    );
    assert_eq!(
        i.search("deploiement", None, &SearchFilter::explicit(), &[])
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn retire_removes_from_search_but_keeps_the_row() {
    let i = index(TestClock::default());
    i.upsert(
        &simple_entry(
            "u1",
            "une entrée sur le déploiement",
            Level::Cure,
            "2026-09-16",
        ),
        &prov(),
    )
    .await
    .unwrap();
    i.retire("u1").await.unwrap();
    assert!(
        i.search("deploiement", None, &SearchFilter::explicit(), &[])
            .await
            .unwrap()
            .is_empty()
    );
    let row = i.get("u1").await.unwrap().unwrap();
    assert_eq!(row.statut, "retiree");
    assert!(
        row.retired_at.is_some(),
        "le signal reste annulable 30 jours"
    );
}

/// #105 : le facteur d'usage monte avec les rappels utiles, descend quand les rappels
/// ne servent pas, et reste borné.
#[test]
fn usage_factor_is_bounded_and_rewards_useful_recalls_only() {
    let sig = |recalls, useful_recalls, successes, contradictions| Signals {
        recalls,
        useful_recalls,
        successes,
        contradictions,
        ..Default::default()
    };
    assert_eq!(usage_factor(&Signals::default()), 1.0);
    let ten_useful = usage_factor(&sig(10, 10, 0, 0));
    assert!(
        ten_useful > 1.15 && ten_useful <= 1.0 + USAGE_MAX_GAIN,
        "{ten_useful}"
    );
    assert!(
        usage_factor(&sig(20, 0, 0, 0)) < 1.0,
        "vingt rappels inutiles"
    );
    assert!((usage_factor(&sig(20, 0, 0, 0)) - (1.0 - USAGE_MAX_LOSS)).abs() < 1e-9);
    assert_eq!(usage_factor(&sig(3, 0, 0, 0)), 1.0, "trop peu pour juger");
    assert!(usage_factor(&sig(0, 0, 0, 4)) < 1.0, "contredite");
    let huge = usage_factor(&sig(10_000, 10_000, 10_000, 0));
    assert!(huge <= 1.0 + USAGE_MAX_GAIN + 1e-12, "{huge}");
}

/// #105 : à pertinence égale, dix rappels utiles font passer devant ; vingt rappels
/// inutiles ne font pas monter ; une correspondance exacte jamais rappelée reste
/// devant une entrée populaire qui ne correspond qu'à moitié.
#[tokio::test]
async fn usage_orders_equal_matches_without_beating_a_better_one() {
    let i = index(TestClock::default());
    for uid in ["neuf", "servi", "ignore"] {
        i.upsert(
            &simple_entry(
                uid,
                "la sauvegarde nocturne du serveur",
                Level::Cure,
                "2026-09-16",
            ),
            &prov(),
        )
        .await
        .unwrap();
    }
    for _ in 0..10 {
        i.record_recall("servi", "sauvegarde", true).await.unwrap();
    }
    for _ in 0..20 {
        i.record_served("ignore", "sauvegarde", true).await.unwrap();
    }
    let hits = i
        .search("sauvegarde nocturne", None, &SearchFilter::explicit(), &[])
        .await
        .unwrap();
    let order: Vec<&str> = hits.iter().map(|h| h.entry.uid.as_str()).collect();
    // Textes identiques : leurs rangs FTS ne diffèrent que par l'ordre d'insertion
    // (moins de 4 % de pertinence), l'usage décide.
    assert_eq!(order, ["servi", "neuf", "ignore"], "{hits:?}");

    // Correspondance exacte (mots et sens) jamais rappelée, contre une entrée
    // populaire trouvée par un seul des deux chemins.
    let i = index(TestClock::default());
    i.upsert(
        &simple_entry(
            "exacte",
            "le code du portail est 4521",
            Level::Cure,
            "2026-09-16",
        ),
        &prov(),
    )
    .await
    .unwrap();
    i.upsert(
        &simple_entry(
            "populaire",
            "la boîte aux lettres du voisin",
            Level::Cure,
            "2026-09-16",
        ),
        &prov(),
    )
    .await
    .unwrap();
    i.put_embedding("exacte", "m", &[1.0, 0.0, 0.0])
        .await
        .unwrap();
    i.put_embedding("populaire", "m", &[0.6, 0.8, 0.0])
        .await
        .unwrap();
    for _ in 0..50 {
        i.record_recall("populaire", "portail", true).await.unwrap();
    }
    let hits = i
        .search(
            "code du portail",
            Some(vec![1.0, 0.0, 0.0]),
            &SearchFilter::explicit(),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(hits[0].entry.uid, "exacte", "{hits:?}");
    assert!(hits[1].usage > 1.15);
}

#[tokio::test]
async fn signals_accumulate() {
    let i = index(TestClock::default());
    i.upsert(
        &simple_entry("u1", "texte", Level::Cure, "2026-09-16"),
        &prov(),
    )
    .await
    .unwrap();
    i.record_recall("u1", "Comment on déploie ?", true)
        .await
        .unwrap();
    i.record_recall("u1", "comment on déploie ?", true)
        .await
        .unwrap();
    i.record_recall("u1", "quelle est la procédure ?", false)
        .await
        .unwrap();
    let s = i.signals_of("u1").await.unwrap();
    assert_eq!(s.recalls, 3);
    assert_eq!(s.useful_recalls, 2);
    assert_eq!(
        s.distinct_queries.len(),
        2,
        "deux requêtes distinctes seulement (casse normalisée)"
    );

    i.record_outcome("u1", true).await.unwrap();
    i.record_outcome("u1", false).await.unwrap();
    let s = i.signals_of("u1").await.unwrap();
    assert_eq!(s.successes, 1);
    assert_eq!(s.contradictions, 1);

    // #105 : servi hors conversation, rien ne compte ; servi en conversation, le
    // rappel compte et son utilité se juge après coup, jamais au-delà des rappels.
    i.record_served("u1", "hors conversation", false)
        .await
        .unwrap();
    let s = i.signals_of("u1").await.unwrap();
    assert_eq!((s.recalls, s.useful_recalls), (3, 2));
    assert!(
        s.distinct_queries
            .contains(&"hors conversation".to_string())
    );
    i.record_served("u1", "où ?", true).await.unwrap();
    i.mark_useful("u1").await.unwrap();
    i.mark_useful("u1").await.unwrap();
    i.mark_useful("u1").await.unwrap();
    let s = i.signals_of("u1").await.unwrap();
    assert_eq!((s.recalls, s.useful_recalls), (4, 4));
}

/// CA 6 (reconstruction) : l'index dérivé peut être effacé et reconstruit ; la
/// provenance et les signaux survivent.
#[tokio::test]
async fn ca_6_9_reindex_keeps_provenance_and_signals() {
    let i = index(TestClock::default());
    let e = simple_entry(
        "u1",
        "Le déploiement passe par CapRover.",
        Level::Cure,
        "2026-09-16",
    );
    i.upsert(&e, &prov()).await.unwrap();
    i.record_recall("u1", "deploiement", true).await.unwrap();

    i.clear_derived().await.unwrap();
    assert_eq!(i.count().await.unwrap(), 0);
    assert_eq!(
        i.origin_of("u1").await.unwrap(),
        Some(Origin::Owner),
        "la provenance n'est pas effacée"
    );
    assert_eq!(i.signals_of("u1").await.unwrap().recalls, 1);

    // Reconstruction depuis le vault : mêmes uid, mêmes résultats.
    i.upsert(&e, &prov()).await.unwrap();
    let hits = i
        .search("deploiement", None, &SearchFilter::explicit(), &[])
        .await
        .unwrap();
    assert_eq!(hits[0].entry.uid, "u1");
    assert_eq!(i.signals_of("u1").await.unwrap().recalls, 1);
}

#[tokio::test]
async fn forget_session_retires_its_entries() {
    let i = index(TestClock::default());
    i.upsert(
        &simple_entry(
            "u1",
            "entrée de la session s1 sur le déploiement",
            Level::Cure,
            "2026-09-16",
        ),
        &Provenance::owner("s1", "interactive", "t"),
    )
    .await
    .unwrap();
    i.upsert(
        &simple_entry(
            "u2",
            "entrée de la session s2 sur le déploiement",
            Level::Cure,
            "2026-09-16",
        ),
        &Provenance::owner("s2", "interactive", "t"),
    )
    .await
    .unwrap();

    let forgotten = i.forget_session("s1").await.unwrap();
    assert_eq!(forgotten, vec!["u1"]);
    let hits = i
        .search("deploiement", None, &SearchFilter::explicit(), &[])
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entry.uid, "u2");
}

#[tokio::test]
async fn links_are_indexed() {
    let i = index(TestClock::default());
    i.upsert(
        &simple_entry(
            "u1",
            "voir [[client-x]] et [[projet-a]]",
            Level::Cure,
            "2026-09-16",
        ),
        &prov(),
    )
    .await
    .unwrap();
    let n: i64 = i
        .store()
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM mem_links WHERE from_uid='u1'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(n, 2);
}

#[test]
fn age_is_computed_from_dates_and_timestamps() {
    // 2026-09-16T00:00:00Z
    let now = 1_789_516_800_000i64;
    assert!((age_days("2026-09-16", now) - 0.0).abs() < 0.01);
    assert!((age_days("2026-09-06", now) - 10.0).abs() < 0.01);
    assert_eq!(age_days("pas une date", now), 0.0);
}

#[test]
fn fts_query_drops_short_words() {
    assert_eq!(fts_query("le déploiement"), "\"déploiement\"*");
    assert_eq!(fts_query("a b"), "");
}

/// Les entrées d'une fiche se relisent par son slug, dans l'ordre des uid, sans les
/// entrées retirées ni celles d'une autre fiche.
#[tokio::test]
async fn entries_are_read_back_by_slug_without_retired_ones() {
    let i = index(TestClock::default());
    for (uid, slug) in [
        ("u2", "acme"),
        ("u1", "acme"),
        ("u3", "autre"),
        ("u4", "acme"),
    ] {
        let mut e = simple_entry(uid, &format!("fait {uid}"), Level::Cure, "2026-09-16");
        e.slug = Some(slug.into());
        i.upsert(&e, &prov()).await.unwrap();
    }
    i.retire("u4").await.unwrap();
    let uids: Vec<String> = i
        .by_slug("acme")
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.uid)
        .collect();
    assert_eq!(uids, ["u1", "u2"]);
    assert!(i.by_slug("inconnue").await.unwrap().is_empty());
}
