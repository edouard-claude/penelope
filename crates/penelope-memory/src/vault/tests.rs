use super::*;

fn ctx(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn when_parsing_and_rendering() {
    let w = When::parse("tache=code; criticite=haute; codeur=agent").unwrap();
    assert_eq!(w.clauses.len(), 3);
    assert_eq!(w.clauses["tache"], vec!["code"]);
    assert_eq!(w.render(), "codeur=agent; criticite=haute; tache=code");
    let multi = When::parse("tache=code|revue").unwrap();
    assert_eq!(multi.clauses["tache"], vec!["code", "revue"]);
}

#[test]
fn when_rejects_unknown_keys_and_empty() {
    assert!(When::parse("inconnue=x").is_err());
    assert!(When::parse("tache").is_err());
    assert!(When::parse("").is_err());
    assert!(When::parse("tache=").is_err());
}

#[test]
fn when_evaluation() {
    let w = When::parse("tache=code; criticite=haute").unwrap();
    assert_eq!(
        w.evaluate(&ctx(&[("tache", "code"), ("criticite", "haute")])),
        WhenMatch::Satisfied
    );
    assert_eq!(
        w.evaluate(&ctx(&[("tache", "redaction"), ("criticite", "haute")])),
        WhenMatch::Contradicted
    );
    assert_eq!(
        w.evaluate(&ctx(&[("tache", "code")])),
        WhenMatch::Unknown(vec!["criticite".into()])
    );
}

#[test]
fn when_compatibility_and_intersection() {
    let a = When::parse("tache=code|revue; client=x").unwrap();
    let b = When::parse("tache=code; criticite=haute").unwrap();
    assert!(a.compatible_with(&b));
    let i = a.intersect(&b);
    assert_eq!(i.clauses["tache"], vec!["code"]);
    assert!(i.clauses.contains_key("criticite"));

    let c = When::parse("tache=deploiement").unwrap();
    assert!(!a.compatible_with(&c));
}

#[test]
fn annotations_roundtrip() {
    let line = "Toujours répondre en français. <!-- uid: 01J8A --> <!-- importance: 9 --> \
                    <!-- depuis: 2026-01-10 --> <!-- declencheurs: a, b -->";
    let a = Annotations::parse(line);
    assert_eq!(a.uid.as_deref(), Some("01J8A"));
    assert_eq!(a.importance, Some(9));
    assert_eq!(a.declencheurs, vec!["a", "b"]);
    assert_eq!(a.depuis.as_deref(), Some("2026-01-10"));
    assert_eq!(strip_annotations(line), "Toujours répondre en français.");

    let rendered = a.render();
    let back = Annotations::parse(&rendered);
    assert_eq!(back.uid, a.uid);
    assert_eq!(back.importance, a.importance);
    assert_eq!(back.declencheurs, a.declencheurs);
}

/// Issue #29 : l'uid devient un identifiant de bloc, les autres annotations
/// restent des commentaires, et un encadré suivi de `^uid` est une entrée.
#[test]
fn entries_use_block_ids() {
    let a = Annotations {
        uid: Some("01J9ABC".into()),
        importance: Some(7),
        sensible: true,
        ..Default::default()
    };
    let line = crate::edit::entry_line("Le serveur est à Lyon", &a);
    assert_eq!(
        line,
        "- Le serveur est à Lyon <!-- importance: 7 --> <!-- sensible: oui --> ^01J9ABC"
    );
    let back = Annotations::parse(&line);
    assert_eq!(back.uid.as_deref(), Some("01J9ABC"));
    assert!(back.sensible);
    assert_eq!(strip_annotations(&line), "- Le serveur est à Lyon");
    assert_eq!(line_uid("- Calcul de x^2 au tableau"), None);
    assert_eq!(block_id("- Formule e = mc ^2"), Some("2".into()));
    assert!(!is_valid_block_id("src_1"));

    let raw = "# Journal\n\n> [!abstract] Épisode 2 · Migration DNS\n                   > Bascule faite, TTL remis à 3600.\n\n^EP2\n\n- Une ligne ^L1\n";
    let (entries, rewritten) = parse_entries(raw);
    assert!(rewritten.is_none(), "format déjà valide");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].uid, "EP2");
    assert_eq!(
        entries[0].text,
        "Épisode 2 · Migration DNS : Bascule faite, TTL remis à 3600."
    );
    assert_eq!(entries[1].uid, "L1");
}

#[test]
fn missing_annotations_are_neutral() {
    let a = Annotations::parse("une ligne toute simple");
    assert!(a.uid.is_none());
    assert!(a.importance.is_none());
    assert!(a.declencheurs.is_empty());
}

#[test]
fn uids_are_added_automatically() {
    let raw = "---\ntype: profil\n---\n# Profil\n\n\
                   - Toujours répondre en français. <!-- uid: 01J8A -->\n\
                   - Préférer Go pour le backend.\n";
    let (entries, rewritten) = parse_entries(raw);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].uid, "01J8A");
    assert_eq!(entries[1].uid.len(), 26, "un ULID est généré");
    let rewritten = rewritten.expect("le fichier doit être réécrit");
    assert!(rewritten.contains(&format!(
        "- Préférer Go pour le backend. ^{}",
        entries[1].uid
    )));
    assert!(
        rewritten.contains("- Toujours répondre en français. ^01J8A"),
        "ancien uid en commentaire migré en identifiant de bloc : {rewritten}"
    );
    // Une seconde passe ne change plus rien.
    let (again, changed) = parse_entries(&rewritten);
    assert!(changed.is_none());
    assert_eq!(again[1].uid, entries[1].uid);
}

#[test]
fn sections_are_tracked() {
    let raw = "# T\n\n## Défaut\n- le défaut\n\n## Exceptions\n- une exception\n";
    let (entries, _) = parse_entries(raw);
    assert_eq!(entries[0].section, "Défaut");
    assert_eq!(entries[1].section, "Exceptions");
}

const PRACTICE: &str = "---\n\
        type: pratique\n\
        id: langage-backend\n\
        scope: global\n\
        confiance: 0.8\n\
        preuves: 9\n\
        statut: active\n\
        maj: 2026-09-17\n\
        declencheurs: [langage, stack, nouveau projet, backend]\n\
        ---\n\
        # Langage backend\n\n\
        ## Défaut\n\
        - Go (stdlib, architecture hexagonale, binaire unique). <!-- uid: 01J9A -->\n\n\
        ## Exceptions\n\
        - **Rust** si l'agent code seul sur un projet critique. <!-- uid: 01J9B --> \
          <!-- quand: tache=code; criticite=haute; codeur=agent --> <!-- confiance: 0.9 -->\n\n\
        ## Écarts observés\n\
        - 2026-09-12 · [[client-x]] : langage imposé par l'existant. <!-- uid: 01J9C --> \
          <!-- quand: client=client-x --> <!-- occurrences: 1 -->\n";

#[test]
fn practice_parses_all_sections() {
    let p = Practice::parse(PRACTICE, "langage-backend").unwrap();
    assert_eq!(p.id, "langage-backend");
    assert_eq!(p.confiance, 0.8);
    assert_eq!(p.preuves, 9);
    assert_eq!(p.statut, PracticeStatus::Active);
    assert!(p.default_entry.is_some());
    assert_eq!(p.exceptions.len(), 1);
    assert_eq!(p.ecarts.len(), 1);
    assert!(p.invalid_entries().is_empty());
}

/// CA 6 (règle défaisable) : un tour de code critique mené par l'agent injecte le
/// défaut **et** l'exception ; un tour de rédaction n'injecte que le défaut.
#[test]
fn ca_6_1_defeasible_rule_recall() {
    let p = Practice::parse(PRACTICE, "langage-backend").unwrap();

    let code = p.recall(
        &ctx(&[
            ("tache", "code"),
            ("criticite", "haute"),
            ("codeur", "agent"),
        ]),
        0.8,
    );
    assert_eq!(code.applicable.len(), 1, "l'exception doit s'appliquer");
    let rendu = code.render();
    assert!(rendu.contains("[pratique: langage-backend · confiance 0.8]"));
    assert!(rendu.contains("Défaut : Go"));
    assert!(rendu.contains("S'applique ici : **Rust**"));
    assert!(rendu.contains("confiance 0.9"));

    let redaction = p.recall(
        &ctx(&[
            ("tache", "redaction"),
            ("criticite", "haute"),
            ("codeur", "agent"),
        ]),
        0.8,
    );
    assert!(
        redaction.applicable.is_empty(),
        "un tour de rédaction ne déclenche pas l'exception"
    );
    assert!(redaction.render().contains("Défaut : Go"));
    assert!(!redaction.render().contains("S'applique ici"));
}

#[test]
fn unknown_predicate_is_not_satisfied_but_may_be_listed() {
    let p = Practice::parse(PRACTICE, "x").unwrap();
    // `codeur` absent du contexte : 2 clés sur 3 connues = 0,67 < 0,8 ⇒ non listé.
    let r = p.recall(&ctx(&[("tache", "code"), ("criticite", "haute")]), 0.8);
    assert!(r.applicable.is_empty());
    assert!(r.to_verify.is_empty());
    // Avec un seuil plus bas, l'exception est signalée « À vérifier ».
    let r = p.recall(&ctx(&[("tache", "code"), ("criticite", "haute")]), 0.6);
    assert_eq!(r.to_verify.len(), 1);
    assert!(r.render().contains("À vérifier"));
}

#[test]
fn deviations_are_never_injected() {
    let p = Practice::parse(PRACTICE, "x").unwrap();
    let r = p.recall(&ctx(&[("client", "client-x")]), 0.0);
    assert!(
        !r.render().contains("langage imposé"),
        "un écart n'est jamais injecté automatiquement"
    );
}

#[test]
fn exception_without_when_is_reported_and_ignored() {
    let raw = PRACTICE.replace(
        "<!-- quand: tache=code; criticite=haute; codeur=agent -->",
        "",
    );
    let p = Practice::parse(&raw, "x").unwrap();
    assert_eq!(p.invalid_entries().len(), 1);
    let r = p.recall(&ctx(&[("tache", "code")]), 0.0);
    assert!(r.applicable.is_empty());
}

#[test]
fn practice_render_roundtrip() {
    let p = Practice::parse(PRACTICE, "x").unwrap();
    let rendered = p.render();
    let back = Practice::parse(&rendered, "x").unwrap();
    assert_eq!(back.id, p.id);
    assert_eq!(back.confiance, p.confiance);
    assert_eq!(back.exceptions.len(), 1);
    assert_eq!(
        back.exceptions[0].annotations.quand,
        p.exceptions[0].annotations.quand
    );
}

#[test]
fn confidence_formula() {
    assert!((Practice::confidence(0, 0) - 0.5).abs() < 1e-9);
    assert!((Practice::confidence(9, 0) - 10.0 / 11.0).abs() < 1e-9);
    assert!((Practice::confidence(1, 4) - 2.0 / 7.0).abs() < 1e-9);
    assert_eq!(
        Practice::derived_status(Practice::confidence(1, 4), 5, 0.5, 4),
        PracticeStatus::Contestee
    );
    assert_eq!(
        Practice::derived_status(0.3, 2, 0.5, 4),
        PracticeStatus::Active,
        "moins de 4 observations : pas encore contestée"
    );
}

#[test]
fn levels_from_paths() {
    assert_eq!(Level::from_path("profil.md"), Level::Profil);
    assert_eq!(Level::from_path("memoire.md"), Level::Coeur);
    assert_eq!(Level::from_path("journal/2026-09-16.md"), Level::Episodic);
    assert_eq!(Level::from_path("pratiques/x.md"), Level::Cure);
    assert_eq!(Level::from_path("AGENTS.md"), Level::Instruction);
    assert_eq!(Level::from_path("DREAMS.md"), Level::Revue);
    assert!(Level::Profil.auto_injected());
    assert!(!Level::Episodic.auto_injected());
    assert!(!Level::Cure.auto_injected());
}

#[test]
fn directives_have_the_required_prefixes() {
    assert!(is_directive("Toujours répondre en français."));
    assert!(is_directive("Éviter les micro-services."));
    assert!(!is_directive("On fait comme ça d'habitude."));
}

#[test]
fn wiki_links_are_extracted() {
    assert_eq!(
        links("voir [[client-x]] et [[projet-a]]"),
        vec!["client-x", "projet-a"]
    );
    assert!(links("aucun lien").is_empty());
}
#[test]
fn a_rendered_practice_reads_back_identically() {
    let raw = "---\ntype: pratique\nid: langage-backend\nconfiance: 0.8\n---\n# Langage backend\n\n## Défaut\n- Go <!-- uid: D1 -->\n\n## Exceptions\n- Rust <!-- uid: E1 --> <!-- quand: tache=code -->\n\n## Écarts observés\n";
    let p = Practice::parse(raw, "langage-backend").unwrap();
    let again = Practice::parse(&p.render(), "langage-backend").unwrap();
    assert_eq!(
        again.default_entry.as_ref().map(|e| e.text.as_str()),
        Some("Go")
    );
    assert_eq!(again.exceptions.len(), 1);
    assert_eq!(again.default_entry.unwrap().uid, "D1");
}
