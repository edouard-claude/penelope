//! Deux processus sur la même base : pendant une mise à jour, l'ancien daemon finit
//! d'écrire pendant que le nouveau démarre et migre. Une transaction « différée » qui lit
//! puis écrit reçoit alors « database is locked » immédiatement, sans attendre le
//! `busy_timeout` (SQLITE_BUSY_SNAPSHOT) : c'est ce que le runner macOS de la CI a vu sur
//! `tool_jobs::tests::a_restart_during_a_job_fails_it_and_asks_the_owner_once` (0.17.61).
//! Une transaction « immédiate » prend le verrou d'écriture dès le début, donc attend son
//! tour et ne rate jamais.

use penelope_store::Store;
use penelope_store::rusqlite::params;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_stores_on_the_same_file_write_concurrently_without_busy_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("penelope.db");
    let a = Store::open(&path).unwrap();
    let b = Store::open(&path).unwrap();
    a.write(|tx| {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS compteur(n INTEGER NOT NULL)")?;
        Ok(())
    })
    .await
    .unwrap();

    // Chaque écriture lit d'abord (la transaction différée commence en lecture), puis
    // écrit : c'est la forme qui se fait refuser sans attente.
    let lire_puis_ecrire = |s: Store, n: usize| async move {
        for _ in 0..n {
            s.write(|tx| {
                let c: i64 = tx.query_row("SELECT count(*) FROM compteur", [], |r| r.get(0))?;
                tx.execute("INSERT INTO compteur(n) VALUES (?1)", params![c])?;
                Ok(())
            })
            .await?;
        }
        Ok::<_, penelope_store::StoreError>(())
    };
    let (ra, rb) = tokio::join!(
        tokio::spawn(lire_puis_ecrire(a.clone(), 200)),
        tokio::spawn(lire_puis_ecrire(b.clone(), 200)),
    );
    ra.unwrap().expect("premier processus : écriture refusée");
    rb.unwrap().expect("second processus : écriture refusée");

    let total: i64 = a
        .read(|c| Ok(c.query_row("SELECT count(*) FROM compteur", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(total, 400, "aucune écriture perdue");
}
