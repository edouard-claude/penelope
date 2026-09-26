//! Contrôles des secrets et des traces laissées : lignes collées, formulaires oubliés,
//! secrets stockés en clair, magasin qui refuse.

use super::*;
use penelope_kernel::event::EventDraft;
use serde_json::json;

#[tokio::test]
async fn glued_shell_lines_fail_beyond_one_in_five() {
    let (_d, s) = services().await;
    let c = glued_lines_check(&s).await;
    assert!(c.ok && c.detail.contains("aucun appel"), "{c:?}");
    for shape in ["simple", "liste", "composee", "composee"] {
        s.events
            .append(EventDraft::new(
                "tool.result",
                json!({"tool": "shell_exec", "shape": shape}),
            ))
            .await
            .unwrap();
    }
    s.events
        .append(EventDraft::new("tool.result", json!({"tool": "fs_read"})))
        .await
        .unwrap();
    let c = glued_lines_check(&s).await;
    assert!(!c.ok, "{c:?}");
    assert!(
        c.detail
            .contains("4 appels en 7 jours : 25% une commande, 25% listes `&&`, 50% composées"),
        "{}",
        c.detail
    );
    assert!(c.fix.unwrap().contains("une commande par appel"));
}

/// #149 : un formulaire ouvert depuis plus d'une heure est nommé avec son sujet ; un
/// formulaire sans date ou récent ne l'est pas.
#[tokio::test]
async fn a_form_left_open_is_named_with_its_topic() {
    let (_d, s) = services().await;
    let old = (s.clock.now_utc() - chrono::Duration::hours(2)).to_rfc3339();
    let recent = s.clock.now_utc().to_rfc3339();
    s.kv_set(
        "tg.form.42.7",
        &json!({"since": old, "choice": "Déployer"}).to_string(),
    )
    .await
    .unwrap();
    s.kv_set("tg.form.42.8", &json!({"since": recent}).to_string())
        .await
        .unwrap();
    s.kv_set("tg.form.42.9", &json!({"choice": "Ancien"}).to_string())
        .await
        .unwrap();
    s.kv_set("tg.form.42.10", "").await.unwrap();
    let c = open_forms_check(&s).await;
    assert!(!c.ok, "{c:?}");
    assert!(
        c.detail
            .starts_with("1 ouvert(s) depuis plus d'une heure : Déployer (sujet 7, depuis "),
        "{}",
        c.detail
    );
    s.kv_set("tg.form.42.7", "").await.unwrap();
    let c = open_forms_check(&s).await;
    assert!(c.ok && c.detail.starts_with("2 en cours"), "{c:?}");
}

/// #134 : un secret en clair dans la file Telegram est signalé ligne par ligne, au plus
/// cinq nommées.
#[tokio::test]
async fn a_secret_stored_in_the_telegram_queue_is_reported() {
    let (_d, s) = services().await;
    assert!(stored_secret_check(&s).await.ok);
    let now = s.clock.now_rfc3339();
    s.store
        .write(move |tx| {
            for i in 0..6 {
                tx.execute(
                    "INSERT INTO tg_outbox(id, chat_id, method, payload, created_at)
                     VALUES (?1, 42, 'sendMessage', ?2, ?3)",
                    penelope_store::rusqlite::params![
                        format!("o{i}"),
                        json!({"text": "clé ghp_0123456789abcdef0123456789abcdef0123"}).to_string(),
                        now
                    ],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let c = stored_secret_check(&s).await;
    assert!(!c.ok);
    assert!(
        c.detail.starts_with("6 ligne(s) en clair : tg_outbox o"),
        "{}",
        c.detail
    );
    assert!(c.detail.contains(" ; …"), "{}", c.detail);
    assert!(
        !c.detail.contains("ghp_"),
        "le contrôle ne recopie pas le secret"
    );
}

mod refusing_stores {
    use penelope_platform::Result;
    use penelope_platform::secrets::SecretStore;
    use std::sync::Mutex;

    /// Magasin qui refuse une étape de l'aller-retour.
    #[derive(Default)]
    struct Refusing {
        set: bool,
        lose: bool,
        delete: bool,
        value: Mutex<Option<String>>,
    }
    fn no(what: &str) -> penelope_platform::PlatformError {
        penelope_platform::PlatformError::Secret(format!("{what} : refusé"))
    }
    impl SecretStore for Refusing {
        fn backend(&self) -> String {
            "essai".into()
        }
        fn get(&self, _: &str) -> Result<Option<String>> {
            if self.lose {
                return Ok(None);
            }
            Ok(self.value.lock().unwrap().clone())
        }
        fn set(&self, _: &str, v: &str) -> Result<()> {
            if self.set {
                return Err(no("écriture"));
            }
            *self.value.lock().unwrap() = Some(v.to_string());
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<()> {
            if self.delete {
                return Err(no("suppression"));
            }
            Ok(())
        }
        fn list(&self) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    #[test]
    fn each_refused_step_of_the_roundtrip_is_named() {
        let check = |store: Refusing| crate::doctor::secret_roundtrip_of(&store);
        let c = check(Refusing {
            set: true,
            ..Default::default()
        });
        assert!(!c.ok && c.detail.starts_with("écriture refusée"), "{c:?}");
        let c = check(Refusing {
            lose: true,
            ..Default::default()
        });
        assert_eq!(c.detail, "écrit puis introuvable");
        let c = check(Refusing {
            delete: true,
            ..Default::default()
        });
        assert!(c.detail.starts_with("suppression refusée"), "{c:?}");
        let c = check(Refusing::default());
        assert!(c.ok && c.detail.starts_with("essai : 8 Ko"), "{c:?}");
    }
}
