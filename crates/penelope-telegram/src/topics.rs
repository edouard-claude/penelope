//! Organisation des conversations en topics (§14.3).
//!
//! En conversation privée avec topics activés : un topic « Général », un pour les
//! approbations, un pour le système, un par run de workflow, un par session nommée.
//! Sans topics : tout dans le chat unique, avec en-têtes et `reply_parameters`.

use penelope_kernel::clock::SharedClock;
use penelope_store::{Store, rusqlite::params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    General,
    Approvals,
    System,
    Run,
    Session,
}

impl Purpose {
    pub fn as_str(&self) -> &'static str {
        match self {
            Purpose::General => "general",
            Purpose::Approvals => "approvals",
            Purpose::System => "system",
            Purpose::Run => "run",
            Purpose::Session => "session",
        }
    }
    pub fn parse(s: &str) -> Option<Purpose> {
        Some(match s {
            "general" => Purpose::General,
            "approvals" => Purpose::Approvals,
            "system" => Purpose::System,
            "run" => Purpose::Run,
            "session" => Purpose::Session,
            _ => return None,
        })
    }

    /// Nom affiché du topic (§14.3).
    pub fn default_name(&self) -> &'static str {
        match self {
            Purpose::General => "🏠 Général",
            Purpose::Approvals => "🔔 Approbations",
            Purpose::System => "⚙️ Système",
            Purpose::Run => "🔧 Run",
            Purpose::Session => "💬 Session",
        }
    }

    /// Les topics fixes, créés au premier démarrage.
    pub const FIXED: [Purpose; 3] = [Purpose::General, Purpose::Approvals, Purpose::System];
}

/// Nom d'un topic de run : `🔧 <workflow> #<id>`.
pub fn run_topic_name(workflow: &str, run_id: &str) -> String {
    let short = run_id.rsplit('_').next().unwrap_or(run_id);
    let short: String = short
        .chars()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mut name = format!("🔧 {workflow} #{short}");
    if name.chars().count() > 128 {
        name = name.chars().take(128).collect();
    }
    name
}

/// Nom d'un topic de session nommée.
pub fn session_topic_name(title: &str) -> String {
    let mut n = format!("💬 {title}");
    if n.chars().count() > 128 {
        n = n.chars().take(128).collect();
    }
    n
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Topic {
    pub chat_id: i64,
    pub topic_id: i64,
    pub purpose: Purpose,
    pub reference: Option<String>,
    pub name: String,
    pub closed: bool,
}

#[derive(Clone)]
pub struct TopicStore {
    store: Store,
    clock: SharedClock,
    enabled: bool,
}

impl TopicStore {
    pub fn new(store: Store, clock: SharedClock, enabled: bool) -> Self {
        TopicStore {
            store,
            clock,
            enabled,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub async fn register(
        &self,
        chat_id: i64,
        topic_id: i64,
        purpose: Purpose,
        reference: Option<&str>,
        name: &str,
    ) -> penelope_store::Result<Topic> {
        let t = Topic {
            chat_id,
            topic_id,
            purpose,
            reference: reference.map(String::from),
            name: name.to_string(),
            closed: false,
        };
        let row = t.clone();
        let ts = self.clock.now_rfc3339();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO tg_topics(chat_id, topic_id, purpose, ref, name,
                        created_at)
                     VALUES(?1,?2,?3,?4,?5,?6)",
                    params![
                        row.chat_id,
                        row.topic_id,
                        row.purpose.as_str(),
                        row.reference,
                        row.name,
                        ts
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(t)
    }

    /// Topic à utiliser pour une destination donnée.
    ///
    /// Sans topics, renvoie `None` : tout part dans le chat unique.
    pub async fn resolve(
        &self,
        chat_id: i64,
        purpose: Purpose,
        reference: Option<&str>,
    ) -> penelope_store::Result<Option<i64>> {
        if !self.enabled {
            return Ok(None);
        }
        let r = reference.map(String::from);
        let p = purpose.as_str().to_string();
        self.store
            .read(move |c| {
                let id: Option<i64> = c
                    .query_row(
                        "SELECT topic_id FROM tg_topics
                         WHERE chat_id = ?1 AND purpose = ?2
                           AND COALESCE(ref, '') = COALESCE(?3, '')
                           AND closed_at IS NULL
                         ORDER BY created_at DESC LIMIT 1",
                        params![chat_id, p, r],
                        |row| row.get(0),
                    )
                    .ok();
                Ok(id)
            })
            .await
    }

    pub async fn close(&self, chat_id: i64, topic_id: i64) -> penelope_store::Result<()> {
        let ts = self.clock.now_rfc3339();
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE tg_topics SET closed_at = ?3 WHERE chat_id = ?1 AND topic_id = ?2",
                    params![chat_id, topic_id, ts],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn list(&self, chat_id: i64) -> penelope_store::Result<Vec<Topic>> {
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT chat_id, topic_id, purpose, ref, name, closed_at
                     FROM tg_topics WHERE chat_id = ?1 ORDER BY created_at",
                )?;
                let rows = st.query_map([chat_id], |r| {
                    let p: String = r.get(2)?;
                    let closed: Option<String> = r.get(5)?;
                    Ok(Topic {
                        chat_id: r.get(0)?,
                        topic_id: r.get(1)?,
                        purpose: Purpose::parse(&p).unwrap_or(Purpose::General),
                        reference: r.get(3)?,
                        name: r.get(4)?,
                        closed: closed.is_some(),
                    })
                })?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }
}

/// En-tête ajouté en mode « sans topics », pour garder les fils lisibles (§14.3).
pub fn header_for(purpose: Purpose, reference: Option<&str>) -> String {
    match (purpose, reference) {
        (Purpose::Approvals, _) => "🔔 **Approbation**".into(),
        (Purpose::System, _) => "⚙️ **Système**".into(),
        (Purpose::Run, Some(r)) => format!("🔧 **Run {r}**"),
        (Purpose::Run, None) => "🔧 **Run**".into(),
        (Purpose::Session, Some(r)) => format!("💬 **{r}**"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    fn topics(enabled: bool) -> TopicStore {
        TopicStore::new(
            Store::open_memory().unwrap(),
            Arc::new(TestClock::default()),
            enabled,
        )
    }

    #[tokio::test]
    async fn fixed_topics_are_resolved() {
        let t = topics(true);
        for (i, p) in Purpose::FIXED.iter().enumerate() {
            t.register(42, 100 + i as i64, *p, None, p.default_name())
                .await
                .unwrap();
        }
        assert_eq!(
            t.resolve(42, Purpose::General, None).await.unwrap(),
            Some(100)
        );
        assert_eq!(
            t.resolve(42, Purpose::Approvals, None).await.unwrap(),
            Some(101)
        );
        assert_eq!(
            t.resolve(42, Purpose::System, None).await.unwrap(),
            Some(102)
        );
    }

    #[tokio::test]
    async fn run_topics_are_scoped_by_reference() {
        let t = topics(true);
        t.register(42, 200, Purpose::Run, Some("r_1"), "🔧 deploy #1")
            .await
            .unwrap();
        t.register(42, 201, Purpose::Run, Some("r_2"), "🔧 deploy #2")
            .await
            .unwrap();
        assert_eq!(
            t.resolve(42, Purpose::Run, Some("r_1")).await.unwrap(),
            Some(200)
        );
        assert_eq!(
            t.resolve(42, Purpose::Run, Some("r_2")).await.unwrap(),
            Some(201)
        );
        assert_eq!(
            t.resolve(42, Purpose::Run, Some("r_3")).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn closed_topics_are_not_reused() {
        let t = topics(true);
        t.register(42, 200, Purpose::Run, Some("r_1"), "x")
            .await
            .unwrap();
        t.close(42, 200).await.unwrap();
        assert_eq!(
            t.resolve(42, Purpose::Run, Some("r_1")).await.unwrap(),
            None
        );
        assert!(t.list(42).await.unwrap()[0].closed);
    }

    #[tokio::test]
    async fn without_topics_everything_goes_to_the_single_chat() {
        let t = topics(false);
        t.register(42, 200, Purpose::Run, Some("r_1"), "x")
            .await
            .unwrap();
        assert_eq!(
            t.resolve(42, Purpose::Run, Some("r_1")).await.unwrap(),
            None,
            "mode sans topics : aucun thread"
        );
    }

    #[test]
    fn topic_names_follow_the_prd() {
        assert_eq!(Purpose::General.default_name(), "🏠 Général");
        assert_eq!(Purpose::Approvals.default_name(), "🔔 Approbations");
        assert_eq!(Purpose::System.default_name(), "⚙️ Système");
        let n = run_topic_name("ticket-to-deploy", "r_01J8ABCDEF");
        assert!(n.starts_with("🔧 ticket-to-deploy #"));
        assert!(n.chars().count() <= 128);
        assert!(session_topic_name(&"x".repeat(500)).chars().count() <= 128);
    }

    #[test]
    fn headers_replace_topics_when_disabled() {
        assert!(header_for(Purpose::Approvals, None).contains("Approbation"));
        assert!(header_for(Purpose::Run, Some("r_1")).contains("r_1"));
        assert!(header_for(Purpose::General, None).is_empty());
    }
}
