//! Vault de référence (issue #29) : un parcours complet simulé (accueil, trois tours, une
//! ingestion PDF, une clôture d'épisode, un rêve) laisse un wiki Markdown valide.
//!
//! ```text
//! accueil ─► tours ×3 ─► PDF ─► épisode clos ─► rêve ─► validateur
//!                                                       ├─ propriétés typées (listes, dates)
//!                                                       ├─ identifiants de bloc valides, uniques
//!                                                       ├─ wikilinks tous résolus, noms uniques
//!                                                       ├─ log.md croissant
//!                                                       └─ dossiers cachés intacts
//! ```

use crate::bus::Origin as Channel;
use crate::runtime::Daemon;
use penelope_kernel::clock::TestClock;
use penelope_llm::mock::MockProvider;
use penelope_llm::types::ChatMessage;
use penelope_memory::candidates::Candidate;
use penelope_memory::{CandidateType, Origin, wiki};
use std::path::Path;
use std::sync::Arc;

/// PDF d'une page, écrit avec lopdf.
fn pdf_with_text(lines: &[&str]) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources_id = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });
    let mut operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 12.into()]),
        Operation::new("Td", vec![50.into(), 700.into()]),
    ];
    for l in lines {
        operations.push(Operation::new("Tj", vec![Object::string_literal(*l)]));
        operations.push(Operation::new("Td", vec![0.into(), (-20).into()]));
    }
    operations.push(Operation::new("ET", vec![]));
    let content = Content { operations };
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

fn hidden_entries(vault: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, hidden: bool, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            let h = hidden || name.starts_with('.');
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, h, out);
            } else if h {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(vault, vault, false, &mut out);
    out.sort();
    out
}

#[tokio::test]
async fn a_full_simulated_journey_leaves_a_valid_markdown_wiki() {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::new(1_789_516_800_000));
    let s = Arc::new(
        crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s.clone()));
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    d.publish_config("test", |c| {
        c.memory.review_max_candidates = 5;
        c.memory.vault_git_autocommit = "0s".into();
        Ok(vec!["memory.review_max_candidates".into()])
    })
    .unwrap();
    let vault = crate::conversation::vault_dir(&s);
    std::fs::create_dir_all(vault.join(".editeur")).unwrap();
    std::fs::write(
        vault.join(".editeur/etat.json"),
        br#"{"ouvert":"profil.md"}"#,
    )
    .unwrap();
    let hidden_before = hidden_entries(&vault);
    let sid = d.chat_session_for(&Channel::Cli).await.unwrap();

    // Accueil complet.
    let mut sitting = crate::onboarding::start(&d, None).await.unwrap();
    let questions: Vec<u32> = sitting.answers.keys().copied().collect();
    for n in questions {
        let q = crate::onboarding::question(n).unwrap();
        let answer = match q.choices.first() {
            Some(c) => c.to_string(),
            None if q.list => "Refonte du site Durand\nMigration Factur-X".to_string(),
            None => "développeur indépendant".to_string(),
        };
        sitting = crate::onboarding::answer(&d, &sitting.rel, n, Some(&answer))
            .await
            .unwrap();
    }
    crate::onboarding::write(&d, &sitting, &sid).await.unwrap();

    // Trois tours relus.
    let episode = s.sessions.require(&sid).await.unwrap().episode_seq;
    for (i, (user, answer, candidate)) in [
        (
            "On passe la facturation en Factur-X",
            "Je note.",
            "La facturation passe en Factur-X",
        ),
        (
            "Désormais les PR restent courtes",
            "D'accord.",
            "Les PR restent courtes",
        ),
        (
            "Le client Durand veut une démo vendredi",
            "Je prépare.",
            "Démo prévue pour le client Durand",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        for m in [ChatMessage::user(user), ChatMessage::assistant(answer)] {
            s.context
                .history
                .append(&sid, &m, 10, episode, false, None)
                .await
                .unwrap();
        }
        p.reply(&format!(
            r#"{{"candidats": [{{"type": "fait", "texte": "{candidate}", "importance": 6, "quand": ""}}]}}"#
        ));
        crate::review::review(&d, &sid, &format!("t{i}"), user, answer, None)
            .await
            .unwrap();
    }

    // Un PDF reçu : fiche, original embarqué, concepts, index, log.md.
    p.reply(
        r#"{"resume": "Contrat-cadre de facturation électronique.", "faits": [],
            "concepts": [{"nom": "Factur-X", "definition": "Norme franco-allemande de facture électronique.", "alias": ["ZUGFeRD"]}],
            "a_definir": ["PDP"]}"#,
    );
    let pdf = pdf_with_text(&["Contrat-cadre", "Les factures sont emises en Factur-X."]);
    crate::ingest::ingest(
        &d,
        "Contrat cadre.pdf",
        pdf,
        "telegram",
        Origin::Owner,
        Some(&sid),
    )
    .await
    .unwrap();

    // Clôture de l'épisode.
    p.reply(
        r#"{"resume": "Passage de la facturation en Factur-X et démo Durand.",
            "candidats": [{"type": "preference", "texte": "Les PR restent courtes", "importance": 7, "quand": ""}]}"#,
    );
    crate::episodes::ingest(&d, &sid, episode, crate::episodes::Boundary::Idle)
        .await
        .unwrap();

    // Un rêve qui promeut une entrée.
    let c = Candidate::new(
        CandidateType::Fait,
        "Le propriétaire facture ses clients en Factur-X",
        Origin::Owner,
        "interactive",
        &s.clock.now_rfc3339(),
    )
    .in_session(&sid)
    .with_importance(9);
    s.candidates.record(vec![c], 5).await.unwrap();
    // Verdict de la grille (issue #37) rattaché au numéro du candidat soumis.
    let n = crate::dream::submission_order(&s)
        .await
        .unwrap()
        .iter()
        .position(|t| t.contains("facture ses clients en Factur-X"))
        .expect("candidat soumis")
        + 1;
    p.reply(
        &serde_json::json!({
            "tri": [{"candidat": n, "durable": true, "utile": true, "precis": true,
                     "introuvable": true, "endosse": true, "justification": "mode de facturation"}],
            "operations": [{"op": "add_entry", "candidat": n, "file": "memoire.md",
                            "section": "Facturation",
                            "text": "Le propriétaire facture ses clients en Factur-X", "importance": 8}]
        })
        .to_string(),
    );
    let dream = crate::dream::run(&d, false).await.unwrap();
    assert!(dream.report.promoted >= 1, "{:?}", dream.report);

    // Validateur.
    let lint = wiki::lint(&vault);
    assert!(
        lint.bad_properties.is_empty(),
        "propriétés : {:?}",
        lint.bad_properties
    );
    assert!(
        lint.invalid_block_ids.is_empty(),
        "blocs : {:?}",
        lint.invalid_block_ids
    );
    assert!(
        lint.duplicate_block_ids.is_empty(),
        "blocs : {:?}",
        lint.duplicate_block_ids
    );
    assert!(lint.unresolved.is_empty(), "liens : {:?}", lint.unresolved);
    assert!(
        lint.broken_blocks.is_empty(),
        "renvois : {:?}",
        lint.broken_blocks
    );
    assert!(
        lint.name_collisions.is_empty(),
        "noms : {:?}",
        lint.name_collisions
    );
    assert!(
        lint.duplicate_aliases.is_empty(),
        "alias : {:?}",
        lint.duplicate_aliases
    );

    for rel in wiki::vault_files(&vault) {
        if let Some(kind) = wiki::note_type(&rel) {
            let raw = std::fs::read_to_string(vault.join(&rel)).unwrap();
            let fm = penelope_kernel::frontmatter::parse(&raw).unwrap();
            assert_eq!(fm.str("type"), Some(kind), "{rel}");
            assert!(fm.has("created") && fm.has("updated"), "{rel}");
        }
    }
    let concept = std::fs::read_to_string(vault.join("concepts/factur-x.md")).unwrap();
    assert!(concept.contains("aliases:\n  - ZUGFeRD\n"), "{concept}");
    let source = std::fs::read_to_string(vault.join("sources/contrat-cadre.md")).unwrap();
    assert!(source.contains("![[contrat-cadre.pdf]]"), "{source}");
    assert!(vault.join("attachments/contrat-cadre.pdf").is_file());
    let journal: String = std::fs::read_dir(vault.join("journal"))
        .unwrap()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    assert!(
        journal.contains("type: journal") && journal.contains("\ndate: "),
        "{journal}"
    );
    assert!(
        journal.contains("[[factur-x]]"),
        "concept cité relié : {journal}"
    );
    assert!(
        journal.contains("[[contrat-cadre]]"),
        "source de l'épisode reliée : {journal}"
    );
    assert!(
        vault
            .join("accueil")
            .read_dir()
            .unwrap()
            .flatten()
            .all(|e| { e.file_name().to_string_lossy().starts_with("accueil-") })
    );

    let log = std::fs::read_to_string(vault.join("log.md")).unwrap();
    let ops: Vec<(String, String)> = log
        .lines()
        .filter_map(|l| l.strip_prefix("## ["))
        .map(|l| {
            let (date, rest) = l.split_once("] ").unwrap();
            (
                date.to_string(),
                rest.split(" | ").next().unwrap().to_string(),
            )
        })
        .collect();
    let kinds: Vec<&str> = ops.iter().map(|(_, op)| op.as_str()).collect();
    for op in ["accueil", "ingest", "dream"] {
        assert!(kinds.contains(&op), "{op} absent de log.md : {log}");
    }
    assert!(
        ops.windows(2).all(|w| w[0].0 <= w[1].0),
        "log.md croissant : {log}"
    );

    assert_eq!(
        hidden_entries(&vault),
        hidden_before,
        "aucune écriture dans un dossier caché"
    );
    assert!(
        dream
            .report
            .promoted_refs
            .iter()
            .any(|r| r.starts_with("[[memoire#^"))
    );
}

/// La skill « wiki-markdown » est livrée avec le binaire, chargée avec les skills de
/// l'utilisateur, et une skill utilisateur du même nom l'emporte.
#[tokio::test]
async fn the_wiki_markdown_skill_is_bundled() {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    crate::runtime::reload_skills(&s).await.unwrap();
    let skill = s.skills.get("wiki-markdown").expect("skill livrée");
    assert_eq!(skill.scope, penelope_skills::Scope::Bundled);
    assert!(skill.body.contains("Propriétés YAML") && skill.body.contains("log.md"));
    assert!(s.skills.errors().is_empty(), "{:?}", s.skills.errors());

    let user = s.platform.dirs.skills().join("wiki-markdown");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(
        user.join("SKILL.md"),
        "---\nname: wiki-markdown\ndescription: version maison\n---\nRègles maison.\n",
    )
    .unwrap();
    crate::runtime::reload_skills(&s).await.unwrap();
    assert_eq!(
        s.skills.get("wiki-markdown").unwrap().description,
        "version maison"
    );
}
