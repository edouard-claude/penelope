use super::*;
use penelope_kernel::clock::TestClock;
use penelope_kernel::session::SessionKind;
use std::sync::Arc;

async fn services() -> (tempfile::TempDir, Arc<Services>, String) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    let sid = s
        .sessions
        .create(SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string();
    (dir, Arc::new(s), sid)
}

async fn bill(s: &Services, sid: &str, model: &str, prompt: i64, cached: i64) {
    let (sid, model) = (sid.to_string(), model.to_string());
    s.store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO usage(ts, day, session_id, model, provider, role, prompt, completion,
                    cached, reasoning) VALUES('2026-10-06T08:00:00Z', '2026-10-06', ?1, ?2,
                    'openrouter', 'chat', ?3, 50, ?4, 7)",
                penelope_store::rusqlite::params![sid, model, prompt, cached],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// #322 : une session neuve n'a rien à mesurer et le dit, sans barre ni table.
#[tokio::test]
async fn a_fresh_session_says_there_is_nothing_to_measure() {
    let (_dir, s, sid) = services().await;
    let r = report(&s, &sid).await.unwrap();
    assert_eq!(r.last, None);
    assert!(r.tiles.is_empty());
    assert_eq!(r.totals, Totals::default());
    let text = render(&r, true);
    assert!(text.contains("Aucun appel au modèle"), "{text}");
    assert!(!text.contains('█') && !text.contains('░'), "{text}");
}

/// #322 : un modèle absent du catalogue prend la fenêtre de repli, signalée ; un appel
/// sans empreintes ne donne pas de découpe inventée, il le dit.
#[tokio::test]
async fn an_unknown_window_is_announced_as_a_fallback() {
    let (_dir, s, sid) = services().await;
    bill(&s, &sid, "openrouter:inconnu/modele-x", 32_000, 8_000).await;
    let r = report(&s, &sid).await.unwrap();
    assert!(!r.window_known);
    assert_eq!(r.window, 128_000);
    assert_eq!(
        r.window_source,
        penelope_llm::catalog::WindowSource::Fallback
    );
    assert!(r.tiles.is_empty(), "{:?}", r.tiles);
    assert_eq!(r.reserves.len(), 2, "{:?}", r.reserves);
    let text = render(&r, false);
    assert!(text.contains("repli prudent"), "{text}");
    assert!(text.contains("fenêtre 128\u{202f}000"), "{text}");
    assert!(text.contains("Utilisé : 32\u{202f}000 (25 %)"), "{text}");
    assert!(
        text.contains(&format!("{}{}", "█".repeat(5), "░".repeat(15))),
        "{text}"
    );
    assert!(text.contains("Cache : 25 % du dernier appel"), "{text}");
    assert!(text.contains("Session : 1 appel(s)"), "{text}");
}

/// #322 : la table aligne libellés, nombres à espaces fines et parts de la fenêtre ; le
/// bloc à chasse fixe n'est posé que pour le canal qui le rend.
#[test]
fn the_table_is_aligned_with_thin_spaces() {
    let r = ContextReport {
        session: "s1".into(),
        last: Some(LastCall {
            model: "codex:gpt-5.6-sol".into(),
            prompt: 191_312,
            cached: 162_615,
        }),
        window: 1_048_576,
        window_known: true,
        window_source: penelope_llm::catalog::WindowSource::Provider,
        background_compaction_at: 256_000,
        compaction_at: 300_000,
        compactions: 2,
        tiles: vec![
            TileShare {
                name: "T0".into(),
                label: "système".into(),
                tokens: 6_428,
            },
            TileShare {
                name: "T3".into(),
                label: "conversation".into(),
                tokens: 153_791,
            },
        ],
        reserves: vec![],
        totals: Totals {
            calls: 47,
            prompt: 920_360,
            completion: 70_691,
            reasoning: 53_941,
            cached: 736_288,
        },
    };
    let text = render(&r, true);
    // #324 : la source de la fenêtre suit le nombre.
    assert!(
        text.contains("fenêtre 1\u{202f}048\u{202f}576 (fournisseur)\n"),
        "{text}"
    );
    assert!(
        text.contains("Utilisé : 191\u{202f}312 (18 %)  ████░░░░"),
        "{text}"
    );
    assert!(text.contains("(dans 64\u{202f}688)"), "{text}");
    assert!(text.contains("compactions : 2"), "{text}");
    assert!(text.contains("Cache : 85 %"), "{text}");
    assert!(
        text.contains("T0 système        6\u{202f}428  0,6 %"),
        "{text}"
    );
    assert!(
        text.contains("T3 conversation 153\u{202f}791 14,7 %"),
        "{text}"
    );
    assert!(
        text.contains("Libre           857\u{202f}264 81,8 %"),
        "{text}"
    );
    assert!(text.contains("```\nT0"), "{text}");
    assert!(text.contains("cache moyen 80 %"), "{text}");
    assert!(!render(&r, false).contains("```"));
}

#[test]
fn numbers_and_bars() {
    assert_eq!(group(0), "0");
    assert_eq!(group(999), "999");
    assert_eq!(group(1_000), "1\u{202f}000");
    assert_eq!(group(1_048_576), "1\u{202f}048\u{202f}576");
    assert_eq!(bar(0, 100), "░".repeat(20));
    assert_eq!(
        bar(500, 100),
        "█".repeat(20),
        "un dépassement reste dans la barre"
    );
    assert_eq!(bar(10, 0), "░".repeat(20), "fenêtre nulle");
}

/// La méthode RPC refuse une session inconnue et, sans session, celle d'une CLI absente.
#[tokio::test]
async fn the_rpc_names_what_is_missing() {
    let (_dir, s, sid) = services().await;
    let e = rpc(&s, &json!({})).await.unwrap_err();
    assert!(e.to_string().contains("aucune session CLI"), "{e}");
    let e = rpc(&s, &json!({"session": "s_inconnue"}))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("session inconnue"), "{e}");
    let v = rpc(&s, &json!({"session": sid})).await.unwrap();
    assert!(v["text"].as_str().unwrap().contains("Aucun appel"), "{v}");
}
