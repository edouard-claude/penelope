use super::super::{reindex, reindex_report, remember};
use super::*;
use penelope_kernel::clock::TestClock;
use penelope_memory::Level;
use penelope_memory::index::SearchFilter;
use std::path::PathBuf;
use std::sync::Arc;

async fn services() -> (tempfile::TempDir, Arc<Services>, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    let vault = penelope_app::helpers::vault_dir(&s);
    (dir, Arc::new(s), vault)
}

/// Réécrit une note à la main, comme dans un éditeur.
fn edit(vault: &Path, rel: &str, f: impl Fn(&str) -> bool) -> Vec<String> {
    let raw = std::fs::read_to_string(vault.join(rel)).unwrap();
    let (kept, cut): (Vec<&str>, Vec<&str>) = raw.lines().partition(|l| f(l));
    std::fs::write(vault.join(rel), kept.join("\n") + "\n").unwrap();
    cut.into_iter().map(String::from).collect()
}

async fn recalled(s: &Services, query: &str) -> Vec<String> {
    s.memory
        .search(query, None, &SearchFilter::explicit(), &[])
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.entry.uid)
        .collect()
}

/// #449 : le propriétaire efface une ligne dans son éditeur. Après réindexation,
/// l'entrée n'est plus rappelée ni servie d'office ; remise à la main, elle revient avec
/// sa provenance et ses signaux.
#[tokio::test]
async fn a_line_deleted_by_hand_leaves_the_index() {
    let (_d, s, vault) = services().await;
    let keep = remember(
        &s,
        &vault,
        Level::Coeur,
        "Le serveur de prod est à Paris",
        "s1",
    )
    .await
    .unwrap();
    let gone = remember(
        &s,
        &vault,
        Level::Coeur,
        "Le vieux serveur est à Lyon",
        "s1",
    )
    .await
    .unwrap();
    s.memory
        .record_recall(&gone, "serveur", true)
        .await
        .unwrap();
    assert!(recalled(&s, "serveur Lyon").await.contains(&gone));

    let cut = edit(&vault, "memoire.md", |l| !l.contains(&gone));
    assert_eq!(cut.len(), 1);
    let report = reindex_report(&s, &vault, false).await.unwrap();
    assert_eq!(
        report.retired,
        vec![Orphan {
            uid: gone.clone(),
            file: "memoire.md".into()
        }]
    );
    assert!(report.held.is_empty());
    assert!(!recalled(&s, "serveur Lyon").await.contains(&gone));
    let core = s.memory.by_level(Level::Coeur).await.unwrap();
    assert_eq!(core.iter().map(|e| &e.uid).collect::<Vec<_>>(), [&keep]);
    assert_eq!(
        s.memory.get(&gone).await.unwrap().unwrap().statut,
        "retiree"
    );
    assert_eq!(report.to_json()["retired_files"]["memoire.md"], 1);

    // Une seconde passe ne retire plus rien.
    assert!(
        reindex_report(&s, &vault, false)
            .await
            .unwrap()
            .retired
            .is_empty()
    );

    // La ligne remise à la main : même entrée, mêmes signaux, même auteur.
    let raw = std::fs::read_to_string(vault.join("memoire.md")).unwrap();
    std::fs::write(vault.join("memoire.md"), format!("{raw}{}\n", cut[0])).unwrap();
    reindex(&s, &vault).await.unwrap();
    assert!(recalled(&s, "serveur Lyon").await.contains(&gone));
    assert_eq!(s.memory.signals_of(&gone).await.unwrap().recalls, 1);
    assert_eq!(
        s.memory.origin_of(&gone).await.unwrap(),
        Some(penelope_memory::Origin::Owner)
    );
}

/// Une ligne déplacée d'une note à l'autre garde son identifiant : l'entrée reste, avec
/// ses signaux, sous son nouveau fichier. La comparaison porte sur toute la passe.
#[tokio::test]
async fn a_line_moved_to_another_note_keeps_its_entry() {
    let (_d, s, vault) = services().await;
    let uid = remember(&s, &vault, Level::Coeur, "ACME paie à 30 jours", "s1")
        .await
        .unwrap();
    remember(
        &s,
        &vault,
        Level::Coeur,
        "Le serveur de prod est à Paris",
        "s1",
    )
    .await
    .unwrap();
    s.memory.record_recall(&uid, "acme", true).await.unwrap();

    let cut = edit(&vault, "memoire.md", |l| !l.contains(&uid));
    std::fs::create_dir_all(vault.join("clients")).unwrap();
    std::fs::write(
        vault.join("clients/acme.md"),
        format!("# ACME\n\n{}\n", cut[0]),
    )
    .unwrap();
    let report = reindex_report(&s, &vault, false).await.unwrap();
    assert!(report.retired.is_empty(), "{report:?}");
    let e = s.memory.get(&uid).await.unwrap().unwrap();
    assert_eq!(
        (e.file.as_str(), e.statut.as_str()),
        ("clients/acme.md", "active")
    );
    assert_eq!(s.memory.signals_of(&uid).await.unwrap().recalls, 1);
    assert!(recalled(&s, "ACME 30 jours").await.contains(&uid));
}

/// Une note supprimée emporte ses entrées ; une note illisible, ou que la
/// synchronisation n'a pas encore descendue, n'en retire aucune.
#[tokio::test]
async fn only_a_note_that_is_gone_takes_its_entries_with_it() {
    let (_d, s, vault) = services().await;
    std::fs::create_dir_all(vault.join("clients")).unwrap();
    for name in ["acme", "globex", "initech"] {
        std::fs::write(
            vault.join(format!("clients/{name}.md")),
            format!("# {name}\n\n- {name} paie à 30 jours\n"),
        )
        .unwrap();
    }
    for i in 0..12 {
        remember(
            &s,
            &vault,
            Level::Coeur,
            &format!("Fait de fond numéro {i}"),
            "s1",
        )
        .await
        .unwrap();
    }
    assert_eq!(reindex(&s, &vault).await.unwrap(), 15);

    // Supprimée, illisible, restée dans le nuage.
    std::fs::remove_file(vault.join("clients/acme.md")).unwrap();
    std::fs::write(vault.join("clients/globex.md"), b"- globex \xff\n").unwrap();
    std::fs::remove_file(vault.join("clients/initech.md")).unwrap();
    std::fs::write(vault.join("clients/.initech.md.icloud"), b"").unwrap();

    let stale = crate::vault_inventory::inventory(&s).await.unwrap().stale;
    assert_eq!(stale.len(), 1, "{stale:?}");
    assert_eq!(stale[0].path, "clients/acme.md");
    assert!(
        stale[0]
            .reason
            .starts_with("1 entrée(s) indexée(s) que le vault ne porte plus")
    );

    let report = reindex_report(&s, &vault, false).await.unwrap();
    let files: Vec<&str> = report.retired.iter().map(|o| o.file.as_str()).collect();
    assert_eq!(files, ["clients/acme.md"], "{report:?}");
    assert_eq!(s.memory.live().await.unwrap().len(), 14);
    assert!(
        crate::vault_inventory::inventory(&s)
            .await
            .unwrap()
            .stale
            .is_empty()
    );
}

/// La garde : une passe qui retirerait plus du quart de l'index, ou qui n'a rien pu
/// relire, ne retire rien et le dit. `--prune` confirme.
#[tokio::test]
async fn a_pass_that_would_empty_the_index_retires_nothing_unless_confirmed() {
    let (_d, s, vault) = services().await;
    for i in 0..8 {
        remember(
            &s,
            &vault,
            Level::Coeur,
            &format!("Fait de fond numéro {i}"),
            "s1",
        )
        .await
        .unwrap();
    }
    remember(
        &s,
        &vault,
        Level::Profil,
        "Toujours répondre en français",
        "s1",
    )
    .await
    .unwrap();

    // Le dossier du vault vidé, comme un volume démonté : rien n'a été relu.
    let saved = std::fs::read(vault.join("memoire.md")).unwrap();
    let profil = std::fs::read(vault.join("profil.md")).unwrap();
    std::fs::remove_file(vault.join("memoire.md")).unwrap();
    std::fs::remove_file(vault.join("profil.md")).unwrap();
    let report = reindex_report(&s, &vault, false).await.unwrap();
    assert!(report.retired.is_empty());
    assert_eq!(report.held.len(), 9);
    assert_eq!(s.memory.live().await.unwrap().len(), 9);

    // Une note sur deux revenue : huit entrées sur neuf manquent encore, c'est trop.
    std::fs::write(vault.join("profil.md"), profil).unwrap();
    let report = reindex_report(&s, &vault, false).await.unwrap();
    assert!(report.retired.is_empty());
    let json = report.to_json();
    assert_eq!(json["held"]["entries"], 8);
    assert_eq!(json["held"]["files"]["memoire.md"], 8);
    assert!(
        json["held"]["message"]
            .as_str()
            .unwrap()
            .contains("penelope mem reindex --prune"),
        "{json}"
    );
    assert_eq!(s.memory.live().await.unwrap().len(), 9);

    // Le vault revenu, tout est là : rien n'a été perdu entre-temps.
    std::fs::write(vault.join("memoire.md"), &saved).unwrap();
    let report = reindex_report(&s, &vault, false).await.unwrap();
    assert!(report.held.is_empty() && report.retired.is_empty());

    // La note vraiment supprimée, et la suppression confirmée.
    std::fs::remove_file(vault.join("memoire.md")).unwrap();
    let report = reindex_report(&s, &vault, true).await.unwrap();
    assert_eq!(report.retired.len(), 8);
    assert_eq!(s.memory.live().await.unwrap().len(), 1);
}

/// #451 : la réindexation ne rajeunit pas la mémoire. 200 jours plus tard, une entrée
/// intacte a gardé sa date, donc son âge dans le classement ; celle que le propriétaire
/// a réécrite dans son éditeur, ou validée, prend le jour de sa dernière vérité.
#[tokio::test]
async fn a_reindex_two_hundred_days_later_only_dates_what_changed() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let s = Services::for_tests(dir.path().to_path_buf(), clock.clone())
        .await
        .unwrap();
    let vault = penelope_app::helpers::vault_dir(&s);
    let mut uids = Vec::new();
    for text in [
        "Le serveur de prod est à Paris",
        "Le serveur de recette est à Lyon",
        "Le serveur de secours est à Lille",
    ] {
        uids.push(remember(&s, &vault, Level::Cure, text, "s1").await.unwrap());
    }
    let maj = |uid: String| {
        let s = &s;
        async move { s.memory.get(&uid).await.unwrap().unwrap().maj }
    };
    let (born, file) = (maj(uids[0].clone()).await, {
        s.memory.get(&uids[0]).await.unwrap().unwrap().file
    });
    reindex(&s, &vault).await.unwrap();

    clock.advance_days(200);
    let later = super::super::day(&s);
    assert_ne!(later, born);
    let raw = std::fs::read_to_string(vault.join(&file)).unwrap();
    let edited: Vec<String> = raw
        .lines()
        .map(|l| match l {
            l if l.contains(&uids[1]) => l.replace("Lyon", "Nantes"),
            l if l.contains(&uids[2]) => format!("{l} <!-- revue: {born} -->"),
            l => l.to_string(),
        })
        .collect();
    std::fs::write(vault.join(&file), edited.join("\n") + "\n").unwrap();
    reindex(&s, &vault).await.unwrap();

    assert_eq!(maj(uids[0].clone()).await, born, "intacte");
    assert_eq!(maj(uids[1].clone()).await, later, "réécrite à la main");
    assert_eq!(maj(uids[2].clone()).await, born, "validée le jour dit");
    assert!(recalled(&s, "serveur Nantes").await.contains(&uids[1]));

    // Validée aujourd'hui : elle redevient fraîche sans que son texte ait bougé.
    let raw = std::fs::read_to_string(vault.join(&file)).unwrap();
    let today = format!("revue: {later}");
    std::fs::write(
        vault.join(&file),
        raw.replace(&format!("revue: {born}"), &today),
    )
    .unwrap();
    reindex(&s, &vault).await.unwrap();
    assert_eq!(maj(uids[2].clone()).await, later, "validée aujourd'hui");
    assert_eq!(maj(uids[0].clone()).await, born);
}
