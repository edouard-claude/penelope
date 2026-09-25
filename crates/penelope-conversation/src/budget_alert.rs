//! Alerte de budget (§16, issue #20) : à la première traversée de `budget.alert_ratio`
//! (80 % par défaut), une seule notification par périmètre (jour, session, run), avec les
//! trois plus gros postes, leur coût et leur part de cache. Le blocage à 100 % reste celui
//! de la boucle d'agent.

use penelope_app::bus::Origin;
use penelope_app::ports::Messenger;
use penelope_app::ports::Slot;
use penelope_app::services::Services;
use penelope_kernel::budget::{BudgetScope, BudgetStatus, UsageRow, UsageWatcher};
use std::sync::{Arc, Weak};

/// Observe le ledger de coûts du daemon.
pub struct AlertWatcher {
    services: Weak<Services>,
    messenger: Slot<dyn Messenger>,
    /// Une vérification à la fois : deux consommations simultanées n'envoient pas deux
    /// alertes.
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl AlertWatcher {
    pub fn install(s: &Arc<Services>, messenger: Slot<dyn Messenger>) {
        s.budget.watch(Arc::new(AlertWatcher {
            services: Arc::downgrade(s),
            messenger,
            lock: Arc::default(),
        }));
    }
}

impl UsageWatcher for AlertWatcher {
    fn recorded(&self, session_id: Option<&str>, run_id: Option<&str>) {
        let (Some(s), Ok(rt)) = (
            self.services.upgrade(),
            tokio::runtime::Handle::try_current(),
        ) else {
            return;
        };
        let messenger = self.messenger.clone();
        let (session, run, lock) = (
            session_id.map(String::from),
            run_id.map(String::from),
            self.lock.clone(),
        );
        rt.spawn(async move {
            let _guard = lock.lock().await;
            if let Err(e) = check(&s, messenger.get(), session.as_deref(), run.as_deref()).await {
                tracing::warn!(error = %e, "alerte de budget non vérifiée");
            }
        });
    }
}

/// Envoie les alertes dues ; rend les textes envoyés.
pub async fn check(
    s: &Services,
    messenger: Option<Arc<dyn Messenger>>,
    session_id: Option<&str>,
    run_id: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let cfg = s.config.config();
    // Jour du propriétaire, comme le plafond qu'il surveille (#79).
    let today = s.budget.today();
    let mut sent = Vec::new();
    for status in s.budget.status(&cfg.budget, session_id, run_id).await? {
        if !status.alerting {
            continue;
        }
        let id = match status.scope {
            BudgetScope::Daily => today.clone(),
            BudgetScope::Session => session_id.unwrap_or_default().to_string(),
            BudgetScope::Run => run_id.unwrap_or_default().to_string(),
        };
        // Le plafond fait partie de la clé : relevé, il mérite une nouvelle alerte.
        let key = format!(
            "budget.alert.{}.{id}.{}",
            status.scope.as_str(),
            status.limit_usd
        );
        if s.kv_get(&key).await?.is_some() {
            continue;
        }
        s.kv_set(&key, &s.clock.now_rfc3339()).await?;
        let top = match status.scope {
            BudgetScope::Daily => s.budget.report("session", None, Some(&today), 3).await?,
            BudgetScope::Session => s.budget.report("turn", session_id, None, 3).await?,
            BudgetScope::Run => s.budget.report_run("turn", None, run_id, None, 3).await?,
        };
        let text = alert_text(&status, &top);
        match &messenger {
            Some(m) => {
                let origin = Origin::Internal {
                    source: "budget".into(),
                };
                if let Err(e) = m.send_text(&origin, &text).await {
                    tracing::warn!(error = %e, "alerte de budget non envoyée");
                }
            }
            None => tracing::warn!(alerte = %text, "alerte de budget sans canal"),
        }
        sent.push(text);
    }
    Ok(sent)
}

pub use penelope_kernel::budget::usd;

/// Texte de l'alerte.
pub fn alert_text(status: &BudgetStatus, top: &[UsageRow]) -> String {
    let (period, key, rows) = match status.scope {
        BudgetScope::Daily => ("aujourd'hui", "budget.daily_usd", "sessions du jour"),
        BudgetScope::Session => ("dans cette session", "budget.session_usd", "requêtes"),
        BudgetScope::Run => ("dans ce run", "budget.run_usd", "requêtes du run"),
    };
    let mut t = format!(
        "⚠️ {} dépensés {period} sur {} ({:.0} %). Au plafond, les tours sont suspendus \
         (`{key}`).",
        usd(status.spent_usd),
        usd(status.limit_usd),
        status.ratio * 100.0
    );
    if !top.is_empty() {
        t.push_str(&format!("\n\nPlus gros postes ({rows}) :"));
        for r in top {
            let label = r
                .label
                .as_deref()
                .map(|l| format!("« {l} »"))
                .unwrap_or_else(|| format!("`{}`", r.key));
            t.push_str(&format!(
                "\n- {} · {label} · {} appel(s) · cache {:.0} %",
                usd(r.cost_usd),
                r.calls,
                r.cache_ratio() * 100.0
            ));
        }
    }
    t.push_str("\n\n`/usage` pour le détail.");
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_app::testing::RecordingMessenger;
    use penelope_kernel::budget::UsageRecord;
    use penelope_kernel::clock::TestClock;
    use std::time::Duration;

    fn spend(session: &str, cost: f64, prompt: u64, cached: u64) -> UsageRecord {
        UsageRecord {
            session_id: Some(session.into()),
            model: "m".into(),
            provider: "p".into(),
            prompt,
            cached,
            completion: 100,
            cost_usd: cost,
            ..Default::default()
        }
    }

    /// Issue #20 : de 79 % à 81 %, une alerte et une seule, avec les plus gros postes.
    #[tokio::test]
    async fn crossing_the_alert_ratio_notifies_once() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            penelope_app::services::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        s.publish_config("test", |c| {
            c.budget.daily_usd = 10.0;
            c.budget.session_usd = 100.0;
            Ok(vec!["budget.daily_usd".into()])
        })
        .unwrap();
        let rec = RecordingMessenger::new();
        let messenger = Slot::default();
        messenger.set(Some(rec.clone() as Arc<dyn Messenger>));
        AlertWatcher::install(&s, messenger);
        let sent = || rec.texts();
        let settle = || tokio::time::sleep(Duration::from_millis(50));

        s.budget
            .record(spend("s1", 7.9, 100_000, 80_000))
            .await
            .unwrap();
        settle().await;
        assert!(sent().is_empty(), "{:?}", sent());

        s.budget.record(spend("s2", 0.2, 10_000, 0)).await.unwrap();
        settle().await;
        s.budget.record(spend("s2", 0.3, 10_000, 0)).await.unwrap();
        settle().await;
        let alerts = sent();
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        assert!(
            alerts[0].contains("8,10 $ dépensés aujourd'hui sur 10,00 $ (81 %)"),
            "{}",
            alerts[0]
        );
        assert!(
            alerts[0].contains("7,90 $ · `s1` · 1 appel(s) · cache 80 %"),
            "{}",
            alerts[0]
        );

        // Au-delà de 100 % : plus d'alerte, c'est le blocage du tour qui prend le relais.
        s.budget.record(spend("s2", 2.0, 10_000, 0)).await.unwrap();
        settle().await;
        assert_eq!(sent().len(), 1);
        let status = s
            .budget
            .status(&s.config.config().budget, Some("s2"), None)
            .await
            .unwrap();
        assert!(status[0].exceeded);
    }
}
