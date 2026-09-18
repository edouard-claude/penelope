//! Sessions et métadonnées structurées (§5.1).

use crate::clock::SharedClock;
use crate::error::{KernelError, Result};
use crate::ids::SessionId;
use penelope_store::Store;
use penelope_store::rusqlite::{self, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Chat,
    WorkflowRun,
    SubAgent,
    Scheduled,
    Heartbeat,
}

impl SessionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionKind::Chat => "chat",
            SessionKind::WorkflowRun => "workflow_run",
            SessionKind::SubAgent => "sub_agent",
            SessionKind::Scheduled => "scheduled",
            SessionKind::Heartbeat => "heartbeat",
        }
    }
    pub fn parse(s: &str) -> Option<SessionKind> {
        Some(match s {
            "chat" => SessionKind::Chat,
            "workflow_run" => SessionKind::WorkflowRun,
            "sub_agent" => SessionKind::SubAgent,
            "scheduled" => SessionKind::Scheduled,
            "heartbeat" => SessionKind::Heartbeat,
            _ => return None,
        })
    }

    /// Provenance associée (§6.5) : seules les sessions interactives produisent des
    /// candidats promouvables.
    pub fn provenance_kind(&self) -> &'static str {
        match self {
            SessionKind::Chat => "interactive",
            SessionKind::WorkflowRun => "workflow",
            SessionKind::Scheduled => "scheduled",
            SessionKind::SubAgent => "sub_agent",
            SessionKind::Heartbeat => "heartbeat",
        }
    }

    /// §6.5 « filtre de session » : les sessions de fond ne promeuvent rien.
    pub fn produces_promotable_candidates(&self) -> bool {
        matches!(self, SessionKind::Chat)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub kind: SessionKind,
    pub title: Option<String>,
    pub model_alias: Option<String>,
    pub model_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub parent_id: Option<String>,
    pub tg_chat_id: Option<i64>,
    pub tg_topic_id: Option<i64>,
    pub workspace: Option<String>,
    pub metadata: Value,
    pub usage_anchor: Option<Value>,
    pub spent_usd: f64,
    pub state: String,
    pub episode_seq: i64,
    pub last_activity: Option<String>,
    /// Plafond propre à la session (issue #32) ; `None` : `budget.session_usd`.
    #[serde(default)]
    pub budget_usd: Option<f64>,
}

#[derive(Clone)]
pub struct SessionStore {
    store: Store,
    clock: SharedClock,
}

/// Opération sur `session_metadata` (§11, outil `session_metadata`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataOp {
    Set,
    Append,
    Update,
    Remove,
}

impl MetadataOp {
    pub fn parse(s: &str) -> Option<MetadataOp> {
        Some(match s {
            "set" => MetadataOp::Set,
            "append" | "add" => MetadataOp::Append,
            "update" => MetadataOp::Update,
            "remove" | "delete" => MetadataOp::Remove,
            _ => return None,
        })
    }
}

impl SessionStore {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        SessionStore { store, clock }
    }

    pub async fn create(&self, kind: SessionKind, title: Option<String>) -> Result<Session> {
        self.create_with(kind, title, None, None).await
    }

    pub async fn create_with(
        &self,
        kind: SessionKind,
        title: Option<String>,
        parent: Option<String>,
        workspace: Option<String>,
    ) -> Result<Session> {
        let now = self.clock.now_rfc3339();
        let s = Session {
            id: SessionId::new(),
            kind,
            title,
            model_alias: None,
            model_id: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            closed_at: None,
            parent_id: parent,
            tg_chat_id: None,
            tg_topic_id: None,
            workspace,
            metadata: json!({}),
            usage_anchor: None,
            spent_usd: 0.0,
            state: "active".into(),
            episode_seq: 0,
            last_activity: Some(now),
            budget_usd: None,
        };
        let row = s.clone();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO sessions(id, kind, title, created_at, updated_at, parent_id,
                        workspace, metadata, state, last_activity)
                     VALUES(?1,?2,?3,?4,?4,?5,?6,'{}','active',?4)",
                    params![
                        row.id.as_str(),
                        row.kind.as_str(),
                        row.title,
                        row.created_at,
                        row.parent_id,
                        row.workspace
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(s)
    }

    pub async fn get(&self, id: &str) -> Result<Option<Session>> {
        let id = id.to_string();
        Ok(self
            .store
            .read(move |c| {
                let mut st = c.prepare(SELECT_SESSION)?;
                let mut rows = st.query([&id])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_session(r)?)),
                    None => Ok(None),
                }
            })
            .await?)
    }

    pub async fn require(&self, id: &str) -> Result<Session> {
        self.get(id)
            .await?
            .ok_or_else(|| KernelError::not_found(format!("session {id}")))
    }

    pub async fn list(&self, kind: Option<SessionKind>, limit: i64) -> Result<Vec<Session>> {
        let k = kind.map(|k| k.as_str().to_string());
        Ok(self
            .store
            .read(move |c| {
                let mut out = Vec::new();
                match k {
                    Some(k) => {
                        let sql = format!("{SELECT_SESSION_BASE} WHERE kind = ?1 AND state != 'deleted' ORDER BY updated_at DESC LIMIT ?2");
                        let mut st = c.prepare(&sql)?;
                        let rows = st.query_map(params![k, limit], row_to_session)?;
                        for r in rows {
                            out.push(r?);
                        }
                    }
                    None => {
                        let sql = format!("{SELECT_SESSION_BASE} WHERE state != 'deleted' ORDER BY updated_at DESC LIMIT ?1");
                        let mut st = c.prepare(&sql)?;
                        let rows = st.query_map(params![limit], row_to_session)?;
                        for r in rows {
                            out.push(r?);
                        }
                    }
                }
                Ok(out)
            })
            .await?)
    }

    /// Modèle collant (§10.3) : figé pour la session.
    pub async fn set_model(&self, id: &str, alias: &str, model_id: &str) -> Result<()> {
        let (id, alias, model_id, now) = (
            id.to_string(),
            alias.to_string(),
            model_id.to_string(),
            self.clock.now_rfc3339(),
        );
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE sessions SET model_alias=?2, model_id=?3, updated_at=?4 WHERE id=?1",
                    params![id, alias, model_id, now],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Retire l'alias collant : le message suivant repasse par le routage (#82).
    pub async fn clear_model(&self, id: &str) -> Result<()> {
        let (id, now) = (id.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE sessions SET model_alias=NULL, model_id=NULL, updated_at=?2 WHERE id=?1",
                    params![id, now],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn touch(&self, id: &str) -> Result<()> {
        let (id, now) = (id.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE sessions SET updated_at=?2, last_activity=?2 WHERE id=?1",
                    params![id, now],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Donne un titre lisible. Avec `only_if_untitled`, un titre déjà posé (à la main ou
    /// par `/new`) est gardé ; `CLI` compte comme absence de titre. Renvoie vrai si le
    /// titre a changé.
    /// Plafond propre à la session ; `None` ou 0 revient au plafond de la configuration.
    pub async fn set_budget(&self, id: &str, usd: Option<f64>) -> Result<bool> {
        let id = id.to_string();
        let usd = usd.filter(|u| *u > 0.0);
        let changed = self
            .store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE sessions SET budget_usd=?2 WHERE id=?1",
                    params![id, usd],
                )?)
            })
            .await?;
        Ok(changed > 0)
    }

    pub async fn set_title(&self, id: &str, title: &str, only_if_untitled: bool) -> Result<bool> {
        let (id, title) = (id.to_string(), title.trim().to_string());
        let changed = self
            .store
            .write(move |tx| {
                let sql = if only_if_untitled {
                    "UPDATE sessions SET title=?2 WHERE id=?1
                     AND (title IS NULL OR trim(title) = '' OR title = 'CLI')"
                } else {
                    "UPDATE sessions SET title=?2 WHERE id=?1"
                };
                Ok(tx.execute(sql, params![id, title])?)
            })
            .await?;
        Ok(changed > 0)
    }

    /// Lie une session à un chat (et sujet) Telegram. Une seule session est liée par chat :
    /// les autres sont détachées, et leurs identifiants renvoyés pour que l'appelant arrête
    /// leurs tours (issue #10).
    pub async fn bind_telegram(
        &self,
        id: &str,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> Result<Vec<String>> {
        let id = id.to_string();
        Ok(self
            .store
            .write(move |tx| {
                let detached: Vec<String> = {
                    let mut st = tx.prepare(
                        "SELECT id FROM sessions WHERE tg_chat_id = ?2
                         AND COALESCE(tg_topic_id, -1) = COALESCE(?3, -1) AND id != ?1",
                    )?;
                    let rows = st.query_map(params![id, chat_id, topic_id], |r| r.get(0))?;
                    rows.collect::<std::result::Result<_, _>>()?
                };
                tx.execute(
                    "UPDATE sessions SET tg_chat_id = NULL, tg_topic_id = NULL
                     WHERE tg_chat_id = ?2 AND COALESCE(tg_topic_id, -1) = COALESCE(?3, -1)
                       AND id != ?1",
                    params![id, chat_id, topic_id],
                )?;
                tx.execute(
                    "UPDATE sessions SET tg_chat_id=?2, tg_topic_id=?3 WHERE id=?1",
                    params![id, chat_id, topic_id],
                )?;
                Ok(detached)
            })
            .await?)
    }

    /// Détache une session de son chat Telegram.
    pub async fn unbind_telegram(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE sessions SET tg_chat_id = NULL, tg_topic_id = NULL WHERE id = ?1",
                    [id],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Retrouve la session de chat liée à un couple (chat, topic) Telegram.
    pub async fn find_by_topic(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> Result<Option<Session>> {
        Ok(self
            .store
            .read(move |c| {
                let sql = format!(
                    "{SELECT_SESSION_BASE} WHERE tg_chat_id = ?1 AND
                     COALESCE(tg_topic_id, -1) = COALESCE(?2, -1) AND state = 'active'
                     ORDER BY updated_at DESC LIMIT 1"
                );
                let mut st = c.prepare(&sql)?;
                let mut rows = st.query(params![chat_id, topic_id])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_session(r)?)),
                    None => Ok(None),
                }
            })
            .await?)
    }

    pub async fn set_state(&self, id: &str, state: &str) -> Result<()> {
        let (id, state, now) = (id.to_string(), state.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                let closed = if state == "closed" {
                    Some(now.clone())
                } else {
                    None
                };
                tx.execute(
                    "UPDATE sessions SET state=?2, updated_at=?3, closed_at=COALESCE(?4, closed_at)
                     WHERE id=?1",
                    params![id, state, now, closed],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn set_usage_anchor(&self, id: &str, anchor: &Value) -> Result<()> {
        let (id, a) = (id.to_string(), serde_json::to_string(anchor)?);
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE sessions SET usage_anchor=?2 WHERE id=?1",
                    params![id, a],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn next_episode(&self, id: &str) -> Result<i64> {
        let id = id.to_string();
        Ok(self
            .store
            .write(move |tx| {
                tx.execute(
                    "UPDATE sessions SET episode_seq = episode_seq + 1 WHERE id=?1",
                    params![id],
                )?;
                let n: i64 =
                    tx.query_row("SELECT episode_seq FROM sessions WHERE id=?1", [&id], |r| {
                        r.get(0)
                    })?;
                Ok(n)
            })
            .await?)
    }

    /// Applique une opération sur `session_metadata` (critères, findings, todos).
    pub async fn metadata(
        &self,
        id: &str,
        op: MetadataOp,
        key: &str,
        entry: Value,
    ) -> Result<Value> {
        let (id, key) = (id.to_string(), key.to_string());
        let now = self.clock.now_rfc3339();
        Ok(self
            .store
            .write(move |tx| {
                // Session inconnue : on ne fait pas semblant d'avoir écrit (issue #47).
                let raw: String = tx
                    .query_row("SELECT metadata FROM sessions WHERE id=?1", [&id], |r| {
                        r.get(0)
                    })
                    .optional()?
                    .ok_or_else(|| {
                        penelope_store::StoreError::other(format!("session introuvable : {id}"))
                    })?;
                let mut meta: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
                let obj = meta.as_object_mut().ok_or_else(|| {
                    penelope_store::StoreError::other("metadata de session corrompue")
                })?;

                match op {
                    MetadataOp::Set => {
                        obj.insert(key.clone(), entry);
                    }
                    MetadataOp::Append => {
                        let arr = obj
                            .entry(key.clone())
                            .or_insert_with(|| Value::Array(Vec::new()));
                        if !arr.is_array() {
                            *arr = Value::Array(vec![arr.clone()]);
                        }
                        if let Some(a) = arr.as_array_mut() {
                            a.push(entry);
                        }
                    }
                    MetadataOp::Update => {
                        // Met à jour l'élément dont le champ `id` correspond.
                        let target_id = entry.get("id").cloned();
                        if let Some(Value::Array(a)) = obj.get_mut(&key)
                            && let Some(tid) = target_id
                        {
                            for item in a.iter_mut() {
                                if item.get("id") == Some(&tid)
                                    && let (Some(dst), Some(src)) =
                                        (item.as_object_mut(), entry.as_object())
                                {
                                    for (k, v) in src {
                                        dst.insert(k.clone(), v.clone());
                                    }
                                }
                            }
                        }
                    }
                    MetadataOp::Remove => {
                        if entry.is_null() {
                            obj.remove(&key);
                        } else if let Some(Value::Array(a)) = obj.get_mut(&key) {
                            a.retain(|x| x.get("id") != entry.get("id"));
                        }
                    }
                }

                let s = serde_json::to_string(&meta).map_err(penelope_store::StoreError::Json)?;
                tx.execute(
                    "UPDATE sessions SET metadata=?2, updated_at=?3 WHERE id=?1",
                    params![id, s, now],
                )?;
                Ok(meta)
            })
            .await?)
    }

    pub async fn get_metadata(&self, id: &str) -> Result<Value> {
        let id = id.to_string();
        Ok(self
            .store
            .read(move |c| {
                let raw: String = c
                    .query_row("SELECT metadata FROM sessions WHERE id=?1", [&id], |r| {
                        r.get(0)
                    })
                    .unwrap_or_else(|_| "{}".into());
                Ok(serde_json::from_str(&raw).unwrap_or_else(|_| json!({})))
            })
            .await?)
    }
}

const SELECT_SESSION_BASE: &str = "SELECT id, kind, title, model_alias, model_id, created_at,
     updated_at, closed_at, parent_id, tg_chat_id, tg_topic_id, workspace, metadata,
     usage_anchor, spent_usd, state, episode_seq, last_activity, budget_usd FROM sessions";

const SELECT_SESSION: &str = "SELECT id, kind, title, model_alias, model_id, created_at,
     updated_at, closed_at, parent_id, tg_chat_id, tg_topic_id, workspace, metadata,
     usage_anchor, spent_usd, state, episode_seq, last_activity, budget_usd FROM sessions WHERE id = ?1";

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let kind: String = r.get(1)?;
    let meta: String = r.get(12)?;
    let anchor: Option<String> = r.get(13)?;
    Ok(Session {
        id: SessionId(r.get(0)?),
        kind: SessionKind::parse(&kind).unwrap_or(SessionKind::Chat),
        title: r.get(2)?,
        model_alias: r.get(3)?,
        model_id: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        closed_at: r.get(7)?,
        parent_id: r.get(8)?,
        tg_chat_id: r.get(9)?,
        tg_topic_id: r.get(10)?,
        workspace: r.get(11)?,
        metadata: serde_json::from_str(&meta).unwrap_or_else(|_| json!({})),
        usage_anchor: anchor.and_then(|s| serde_json::from_str(&s).ok()),
        spent_usd: r.get(14)?,
        state: r.get(15)?,
        episode_seq: r.get(16)?,
        last_activity: r.get(17)?,
        budget_usd: r.get(18)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use std::sync::Arc;

    async fn ss() -> SessionStore {
        SessionStore::new(
            Store::open_memory().unwrap(),
            Arc::new(TestClock::default()),
        )
    }

    #[tokio::test]
    async fn create_and_get() {
        let s = ss().await;
        let sess = s
            .create(SessionKind::Chat, Some("test".into()))
            .await
            .unwrap();
        let back = s.require(sess.id.as_str()).await.unwrap();
        assert_eq!(back.title.as_deref(), Some("test"));
        assert_eq!(back.kind, SessionKind::Chat);
    }

    /// #47 : écrire les métadonnées d'une session inconnue ne réussit pas en silence.
    #[tokio::test]
    async fn metadata_on_an_unknown_session_fails() {
        let s = ss().await;
        let e = s
            .metadata("s_inconnue", MetadataOp::Set, "k", json!("v"))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("introuvable"), "{e}");
    }

    #[tokio::test]
    async fn metadata_criteria_flow() {
        let s = ss().await;
        let sess = s.create(SessionKind::WorkflowRun, None).await.unwrap();
        let id = sess.id.as_str();

        s.metadata(
            id,
            MetadataOp::Append,
            "criteria",
            json!({"id":"c1","text":"tests verts","status":"pending"}),
        )
        .await
        .unwrap();
        s.metadata(
            id,
            MetadataOp::Append,
            "criteria",
            json!({"id":"c2","text":"lint propre","status":"pending"}),
        )
        .await
        .unwrap();
        let m = s
            .metadata(
                id,
                MetadataOp::Update,
                "criteria",
                json!({"id":"c1","status":"completed"}),
            )
            .await
            .unwrap();

        let arr = m["criteria"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["status"], "completed");
        assert_eq!(arr[1]["status"], "pending");

        let m = s
            .metadata(id, MetadataOp::Remove, "criteria", json!({"id":"c2"}))
            .await
            .unwrap();
        assert_eq!(m["criteria"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn background_sessions_do_not_promote() {
        assert!(SessionKind::Chat.produces_promotable_candidates());
        for k in [
            SessionKind::Scheduled,
            SessionKind::SubAgent,
            SessionKind::Heartbeat,
            SessionKind::WorkflowRun,
        ] {
            assert!(!k.produces_promotable_candidates(), "{k:?}");
        }
    }

    #[tokio::test]
    async fn telegram_topic_lookup() {
        let s = ss().await;
        let a = s.create(SessionKind::Chat, None).await.unwrap();
        s.bind_telegram(a.id.as_str(), 42, Some(7)).await.unwrap();
        let found = s.find_by_topic(42, Some(7)).await.unwrap().unwrap();
        assert_eq!(found.id, a.id);
        assert!(s.find_by_topic(42, Some(8)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn episodes_increment() {
        let s = ss().await;
        let a = s.create(SessionKind::Chat, None).await.unwrap();
        assert_eq!(s.next_episode(a.id.as_str()).await.unwrap(), 1);
        assert_eq!(s.next_episode(a.id.as_str()).await.unwrap(), 2);
    }
}
