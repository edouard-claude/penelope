use super::*;

#[tokio::test]
async fn open_migrate_read_write() {
    let s = Store::open_memory().unwrap();
    assert_eq!(s.integrity().unwrap(), "ok");

    s.write(|tx| {
        kv_set(tx, "hello", "world")?;
        Ok(())
    })
    .await
    .unwrap();

    let v = s.read(|c| kv_get(c, "hello")).await.unwrap();
    assert_eq!(v.as_deref(), Some("world"));
}

#[tokio::test]
async fn write_rolls_back_on_error() {
    let s = Store::open_memory().unwrap();
    let r: Result<()> = s
        .write(|tx| {
            kv_set(tx, "k", "v")?;
            Err(StoreError::other("boom"))
        })
        .await;
    assert!(r.is_err());
    let v = s.read(|c| kv_get(c, "k")).await.unwrap();
    assert!(v.is_none(), "le rollback doit annuler l'écriture");
}

#[tokio::test]
async fn concurrent_writes_are_serialised() {
    let s = Store::open_memory().unwrap();
    let mut handles = Vec::new();
    for i in 0..32 {
        let s2 = s.clone();
        handles.push(tokio::spawn(async move {
            s2.write(move |tx| {
                tx.execute(
                    "INSERT INTO kv(k,v) VALUES(?1,?2)",
                    rusqlite::params![format!("k{i}"), i.to_string()],
                )?;
                Ok(())
            })
            .await
        }));
    }
    for h in handles {
        h.await.unwrap().unwrap();
    }
    let n: i64 = s
        .read(|c| Ok(c.query_row("SELECT count(*) FROM kv", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(n, 32);
}

/// #44 : une panique dans une closure d'écriture ne tue plus l'écrivain. La
/// transaction est annulée, le demandeur reçoit la panique nommée, et l'écriture
/// suivante passe.
/// Le compteur de paniques de l'écrivain est global : les tests qui en provoquent
/// passent l'un après l'autre, sinon l'un voit la panique de l'autre.
static WRITER_PANIC_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn a_panicking_write_does_not_kill_the_writer() {
    let _one_at_a_time = WRITER_PANIC_TESTS.lock().await;
    let s = Store::open_memory().unwrap();
    s.write(|tx| kv_set(tx, "avant", "1")).await.unwrap();
    let avant = writer_panics();

    // La panique est attendue : pas de trace sur stderr pendant le test.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let e = s
        .write(|tx| {
            kv_set(tx, "jamais", "2")?;
            let v: Vec<u8> = Vec::new();
            let _ = v[3];
            Ok(())
        })
        .await
        .unwrap_err();
    std::panic::set_hook(hook);

    let msg = e.to_string();
    assert!(matches!(e, StoreError::WriterPanic(_)), "{msg}");
    assert!(msg.contains("index out of bounds"), "{msg}");
    assert_eq!(writer_panics(), avant + 1);

    // L'écrivain est vivant, et le travail qui a paniqué n'a rien commité.
    s.write(|tx| kv_set(tx, "apres", "3")).await.unwrap();
    let (avant_v, apres_v, jamais): (String, String, i64) = s
        .read(|c| {
            Ok((
                c.query_row("SELECT v FROM kv WHERE k='avant'", [], |r| r.get(0))?,
                c.query_row("SELECT v FROM kv WHERE k='apres'", [], |r| r.get(0))?,
                c.query_row("SELECT count(*) FROM kv WHERE k='jamais'", [], |r| r.get(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!((avant_v.as_str(), apres_v.as_str(), jamais), ("1", "3", 0));
}

fn synchronous(c: &Connection) -> i64 {
    c.query_row("PRAGMA synchronous;", [], |r| r.get(0))
        .unwrap()
}

/// #75 : une écriture durable s'exécute sous `synchronous=FULL` (2) et la connexion
/// revient à `NORMAL` (1) ensuite, après un succès, une erreur ou une panique.
#[tokio::test]
async fn a_durable_write_runs_under_full_sync_and_restores_normal() {
    let _one_at_a_time = WRITER_PANIC_TESTS.lock().await;
    let s = Store::open_memory().unwrap();
    let dedans = s
        .write_durable(|tx| {
            kv_set(tx, "effet", "dispatching")?;
            Ok(synchronous(tx))
        })
        .await
        .unwrap();
    assert_eq!(dedans, 2, "FULL pendant la transaction durable");
    assert_eq!(s.write(|tx| Ok(synchronous(tx))).await.unwrap(), 1);
    assert_eq!(s.durable_commits(), 1);

    // Erreur : rollback, pas de commit compté, NORMAL rétabli.
    let e = s
        .write_durable(|tx| -> Result<()> {
            kv_set(tx, "perdu", "x")?;
            Err(StoreError::other("refus"))
        })
        .await;
    assert!(e.is_err());
    assert_eq!(s.write(|tx| Ok(synchronous(tx))).await.unwrap(), 1);

    // Panique : rattrapée, NORMAL rétabli, l'écrivain continue.
    let e = s
        .write_durable(|_tx| -> Result<()> { panic!("panique voulue dans une écriture durable") })
        .await
        .unwrap_err();
    assert!(matches!(e, StoreError::WriterPanic(_)), "{e}");
    assert_eq!(s.write(|tx| Ok(synchronous(tx))).await.unwrap(), 1);
    assert_eq!(s.durable_commits(), 1, "seuls les commits aboutis comptent");

    let perdu: i64 = s
        .read(|c| Ok(c.query_row("SELECT count(*) FROM kv WHERE k='perdu'", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(perdu, 0);
}

/// #77 : l'instantané ne gèle plus l'écrivain. Pendant la copie d'une base de
/// quelques dizaines de Mo, des écritures partent et reviennent ; la copie passe
/// `integrity_check`. Gelé, l'écrivain n'aboutirait à rien avant la fin de la copie :
/// c'est `during > 0` qui le prouve. Le plafond de latence reste large, le disque
/// partagé d'un runner de CI peut suspendre une écriture 200 ms sous la pression de
/// la copie.
#[test]
fn writes_go_on_while_a_snapshot_is_taken() {
    let s = Store::open_memory().unwrap();
    s.write_blocking(|tx| {
        let blob = "x".repeat(4096);
        for i in 0..8000 {
            tx.execute(
                "INSERT INTO kv(k, v) VALUES(?1, ?2)",
                rusqlite::params![format!("gros{i}"), blob],
            )?;
        }
        Ok(())
    })
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("instantane.db");

    let done = Arc::new(AtomicBool::new(false));
    let snap = {
        let (s, dest, done) = (s.clone(), dest.clone(), done.clone());
        std::thread::spawn(move || {
            s.backup_to(&dest).unwrap();
            done.store(true, Ordering::SeqCst);
        })
    };
    let mut during = 0;
    let mut slowest = std::time::Duration::ZERO;
    let mut i = 0;
    while !done.load(Ordering::SeqCst) {
        let t = std::time::Instant::now();
        let k = format!("pendant{i}");
        s.write_blocking(move |tx| kv_set(tx, &k, "v")).unwrap();
        slowest = slowest.max(t.elapsed());
        if !done.load(Ordering::SeqCst) {
            during += 1;
        }
        i += 1;
    }
    snap.join().unwrap();
    assert!(
        during > 0,
        "aucune écriture n'a abouti pendant l'instantané"
    );
    assert!(
        slowest < std::time::Duration::from_secs(1),
        "écriture la plus lente : {slowest:?}"
    );

    let c = Connection::open(&dest).unwrap();
    let ok: String = c
        .query_row("PRAGMA integrity_check;", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    let n: i64 = c
        .query_row("SELECT count(*) FROM kv WHERE k LIKE 'gros%'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 8000);
}

#[test]
fn backup_produces_readable_copy() {
    let s = Store::open_memory().unwrap();
    s.write_blocking(|tx| kv_set(tx, "a", "b")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("backup.db");
    s.backup_to(&dest).unwrap();
    let c = Connection::open(&dest).unwrap();
    let v: String = c
        .query_row("SELECT v FROM kv WHERE k='a'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, "b");
}
