/// #59 : un candidat reporté trois nuits de suite est rejeté avec sa raison, sinon la
/// file ne décroît jamais.
#[tokio::test]
async fn three_deferrals_reject_the_candidate() {
    let cs = cs();
    let c = Candidate::new(
        CandidateType::Fait,
        "Le bureau ferme à 18 h",
        Origin::Owner,
        "interactive",
        "2026-09-17T10:00:00Z",
    );
    cs.record(vec![c], 5).await.unwrap();
    let id = cs.pending(None).await.unwrap()[0].id.clone();

    for _ in 0..2 {
        cs.set_state(
            std::slice::from_ref(&id),
            "deferred",
            Some("sans verdict de la grille"),
        )
        .await
        .unwrap();
        assert_eq!(
            cs.pending(None).await.unwrap().len(),
            1,
            "encore en attente"
        );
    }
    cs.set_state(
        std::slice::from_ref(&id),
        "deferred",
        Some("sans verdict de la grille"),
    )
    .await
    .unwrap();
    assert!(
        cs.pending(None).await.unwrap().is_empty(),
        "le troisième report le rejette"
    );
}
use super::*;
use penelope_kernel::clock::TestClock;
use std::sync::Arc;

fn cs() -> CandidateStore {
    CandidateStore::new(
        Store::open_memory().unwrap(),
        Arc::new(TestClock::default()),
    )
}

fn cand(text: &str, day: &str, session: &str) -> Candidate {
    let mut c = Candidate::new(
        CandidateType::Ecart,
        text,
        Origin::Owner,
        "interactive",
        &format!("{day}T10:00:00Z"),
    )
    .in_session(session)
    .with_subject("langage-backend");
    c.quand = When::parse("client=client-x").ok();
    c
}

#[test]
fn jaccard_similarity() {
    assert!(
        jaccard(
            "le déploiement passe par caprover",
            "le déploiement passe par caprover"
        ) > 0.99
    );
    assert!(
        jaccard(
            "le déploiement passe par CapRover",
            "Le déploiement passe par caprover !"
        ) > 0.99
    );
    assert!(jaccard("le déploiement", "la revue de code") < 0.3);
}

#[test]
fn subject_keys_ignore_stop_words() {
    let a = subject_from_text("Toujours utiliser Go pour le backend");
    let b = subject_from_text("utiliser Go pour le backend, toujours");
    assert_eq!(a, b, "l'ordre et les mots vides ne comptent pas");
    assert!(a.contains("backend"));
}

#[test]
fn grouping_counts_sessions_and_days() {
    let candidates = vec![
        cand("langage imposé par l'existant", "2026-09-10", "s1"),
        cand("langage imposé par l'existant", "2026-09-11", "s2"),
        cand("langage imposé par l'existant", "2026-09-12", "s3"),
    ];
    let groups = group(candidates, 0.9);
    assert_eq!(groups.len(), 1);
    let g = &groups[0];
    assert_eq!(g.occurrences, 3);
    assert_eq!(g.distinct_sessions, 3);
    assert_eq!(g.distinct_days, 3);
    assert!(g.has_owner_origin());
    assert_eq!(g.common_when().unwrap().render(), "client=client-x");
}

#[test]
fn duplicates_in_the_same_session_and_day_collapse() {
    let candidates = vec![
        cand("langage imposé par l'existant", "2026-09-10", "s1"),
        cand("langage imposé par l'existant !", "2026-09-10", "s1"),
    ];
    let groups = group(candidates, 0.9);
    assert_eq!(
        groups[0].occurrences, 1,
        "même session, même jour : une occurrence"
    );
}

#[test]
fn different_context_signatures_are_different_groups() {
    let mut a = cand("langage imposé", "2026-09-10", "s1");
    a.quand = When::parse("client=client-x").ok();
    let mut b = cand("langage imposé", "2026-09-10", "s2");
    b.quand = When::parse("client=client-y").ok();
    assert_eq!(group(vec![a, b], 0.9).len(), 2);
}

#[tokio::test]
async fn at_most_five_candidates_per_turn() {
    let s = cs();
    let candidates: Vec<Candidate> = (0..12)
        .map(|i| {
            Candidate::new(
                CandidateType::Fait,
                &format!("fait numéro {i}"),
                Origin::Owner,
                "interactive",
                "2026-09-16T10:00:00Z",
            )
            .with_importance(i as u8)
        })
        .collect();
    let n = s.record(candidates, 5).await.unwrap();
    assert_eq!(n, 5);
    assert_eq!(s.count_by_state("new").await.unwrap(), 5);
    // Les plus importants sont gardés.
    let pending = s.pending(None).await.unwrap();
    assert!(pending.iter().all(|c| c.importance >= 7));
}

#[tokio::test]
async fn memory_echoes_are_never_recorded() {
    let s = cs();
    let mut c = Candidate::new(
        CandidateType::Fait,
        "un fait rappelé depuis la mémoire",
        Origin::Owner,
        "interactive",
        "2026-09-16T10:00:00Z",
    );
    c.from_memory = true;
    assert_eq!(s.record(vec![c], 5).await.unwrap(), 0);
}

#[tokio::test]
async fn background_sessions_record_nothing() {
    let s = cs();
    let c = Candidate::new(
        CandidateType::Fait,
        "un fait vu par un cron",
        Origin::Agent,
        "scheduled",
        "2026-09-16T10:00:00Z",
    );
    assert_eq!(s.record(vec![c], 5).await.unwrap(), 0);
}

#[tokio::test]
async fn state_transitions_and_expiry() {
    let clock = TestClock::default();
    let s = CandidateStore::new(Store::open_memory().unwrap(), Arc::new(clock.clone()));
    let c = cand("un écart", "2026-01-01", "s1");
    let id = c.id.clone();
    s.record(vec![c], 5).await.unwrap();

    s.set_state(std::slice::from_ref(&id), "grouped", None)
        .await
        .unwrap();
    assert_eq!(s.count_by_state("grouped").await.unwrap(), 1);

    // 100 jours plus tard, un écart non promu expire.
    clock.advance_days(100);
    assert_eq!(s.expire_stale(90).await.unwrap(), 1);
    assert_eq!(s.count_by_state("expired").await.unwrap(), 1);
}

#[test]
fn correction_detection() {
    assert!(looks_like_correction("non, ici on fait autrement"));
    assert!(looks_like_correction("Plutôt en Rust pour ce projet"));
    assert!(!looks_like_correction("ajoute un test d'intégration"));
}

#[test]
fn rule_phrasing_detection() {
    assert!(stated_as_a_rule("Toujours répondre en français"));
    assert!(stated_as_a_rule("désormais on passe par CapRover"));
    assert!(!stated_as_a_rule("cette fois-ci on fait autrement"));
}

/// Un candidat d'origine externe rejeté, puis confirmé par le propriétaire, repart
/// en consolidation sous l'origine `owner`, sans sa raison de rejet.
#[tokio::test]
async fn an_owner_confirmation_requeues_the_candidate_as_owned() {
    let cs = cs();
    let c = Candidate::new(
        CandidateType::Fait,
        "Le fournisseur livre le mardi",
        Origin::Untrusted,
        "interactive",
        "2026-09-17T10:00:00Z",
    );
    cs.record(vec![c], 5).await.unwrap();
    let id = cs.pending(None).await.unwrap()[0].id.clone();
    cs.set_state(
        std::slice::from_ref(&id),
        "rejected",
        Some("origine externe"),
    )
    .await
    .unwrap();
    assert!(cs.pending(None).await.unwrap().is_empty());

    let n = cs
        .confirm_by_owner(&[id.clone(), "absent".into()])
        .await
        .unwrap();
    assert_eq!(n, 1, "seul le candidat existant est confirmé");
    let back = cs.pending(None).await.unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].id, id);
    assert_eq!(back[0].origin, Origin::Owner);
    assert_eq!(back[0].state, "new");
}

/// #285 : les rejets « ni dit ni confirmé » sans phrase se relisent ; repassé avec la
/// phrase du propriétaire, un candidat repart au tri sous son origine, reports remis
/// à zéro. Un rejet d'un autre motif, ou déjà porteur d'une phrase, n'est pas relu.
#[tokio::test]
async fn a_requeued_rejection_becomes_the_owners_word() {
    let cs = cs();
    let reason = crate::grid::NOT_ENDORSED;
    let mk = |text: &str| {
        Candidate::new(
            CandidateType::Fait,
            text,
            Origin::Agent,
            "interactive",
            "2026-09-20T13:34:57.595Z",
        )
        .in_session("s1")
    };
    let said = mk("La fille a piano le samedi matin.");
    let other = mk("Le dépôt openfox compte 302 étoiles.");
    let mut quoted = mk("Le propriétaire veut ses devis en PDF.");
    quoted.owner_quote = Some("je veux mes devis en PDF".into());
    let (said_id, other_id, quoted_id) = (said.id.clone(), other.id.clone(), quoted.id.clone());
    cs.record(vec![said, other, quoted], 5).await.unwrap();
    // Deux nuits reportées avant le rejet : le compte doit repartir de zéro.
    cs.set_state(
        std::slice::from_ref(&said_id),
        "deferred",
        Some("sans verdict"),
    )
    .await
    .unwrap();
    cs.set_state(std::slice::from_ref(&said_id), "rejected", Some(reason))
        .await
        .unwrap();
    cs.set_state(
        std::slice::from_ref(&other_id),
        "rejected",
        Some("retrouvable ailleurs (code, docs, tracker, git)"),
    )
    .await
    .unwrap();
    cs.set_state(std::slice::from_ref(&quoted_id), "rejected", Some(reason))
        .await
        .unwrap();

    let relu = cs.rejected_for(reason).await.unwrap();
    assert_eq!(
        relu.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        vec![said_id.as_str()],
        "seul le rejet à ce motif sans phrase est relu"
    );

    let n = cs
        .requeue_as_owner(&[
            (said_id.clone(), "ma fille a piano le samedi matin".into()),
            ("absent".into(), "rien".into()),
        ])
        .await
        .unwrap();
    assert_eq!(n, 1);
    let back = cs.pending(None).await.unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].id, said_id);
    assert_eq!(back[0].origin, Origin::Owner);
    assert_eq!(back[0].state, "new");
    assert_eq!(back[0].reject_reason, None);
    assert_eq!(
        back[0].owner_quote.as_deref(),
        Some("ma fille a piano le samedi matin")
    );
    let deferrals: i64 = cs
        .store
        .read({
            let id = said_id.clone();
            move |c| {
                Ok(c.query_row(
                    "SELECT deferrals FROM mem_candidates WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )?)
            }
        })
        .await
        .unwrap();
    assert_eq!(
        deferrals, 0,
        "les reports d'avant le rejet ne comptent plus"
    );
    assert!(cs.rejected_for(reason).await.unwrap().is_empty());
    assert_eq!(cs.count_by_state("rejected").await.unwrap(), 2);
}

/// #285 : les textes déjà notés depuis une fiche, quel que soit leur état.
#[tokio::test]
async fn texts_from_a_source_are_listed_whatever_their_state() {
    let cs = cs();
    let mut a = Candidate::new(
        CandidateType::Fait,
        "Le propriétaire vit à La Réunion depuis 2019.",
        Origin::Owner,
        "interactive",
        "2026-09-30T10:00:00Z",
    );
    a.source_ref = Some("source:sources/export.md".into());
    let mut b = a.clone();
    b.id = "c_b".into();
    b.text = "Il joue du piano.".into();
    let b_id = b.id.clone();
    cs.record(vec![a, b], 5).await.unwrap();
    cs.set_state(&[b_id], "promoted", None).await.unwrap();
    let texts = cs.texts_from("source:sources/export.md").await.unwrap();
    assert_eq!(texts.len(), 2);
    assert!(texts.contains("Il joue du piano."));
    assert!(cs.texts_from("source:autre.md").await.unwrap().is_empty());
}
