//! Un sujet Telegram = un projet (#301) : ce que la passerelle tranche de son côté de la
//! frontière (quel sujet vaut projet : ni « Général », ni le foyer ; quelles sessions sont
//! les siennes), puis déclare au cœur par le port `ChannelDelivery::subject_of` et par
//! `penelope_vault::session_project::subjects`, qui en fait la fiche et les rattachements.

use super::*;
use penelope_vault::session_project::subjects::{MigrationReport, Subject, adopt, migrate};

/// Le sujet « Général » d'un forum : la conversation sans sujet, jamais un projet.
pub(super) const GENERAL_TOPIC: i64 = 1;

/// Préfixe des clés `tg.topic_name.<chat>.<sujet>` (`penelope_app::helpers::topic_name_key`).
const TOPIC_NAME_PREFIX: &str = "tg.topic_name.";

impl TelegramGateway {
    /// Nom d'un sujet qui vaut projet : connu, ni « Général », ni le foyer
    /// (`telegram.home`), où arrivent les avis sans session.
    pub(super) async fn topic_subject(&self, chat_id: i64, topic_id: i64) -> Option<String> {
        if topic_id == GENERAL_TOPIC || self.home_chat() == (chat_id, Some(topic_id)) {
            return None;
        }
        self.daemon
            .services
            .kv_get(&topic_name_key(chat_id, topic_id))
            .await
            .ok()
            .flatten()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
    }

    /// Nom du sujet de la session, s'il vaut projet : ce que le port `subject_of` rend.
    pub(super) async fn session_subject(&self, session_id: &str) -> Option<String> {
        let sess = self
            .daemon
            .services
            .sessions
            .get(session_id)
            .await
            .ok()
            .flatten()?;
        let (chat, topic) = sess.tg_chat_id.zip(sess.tg_topic_id)?;
        self.topic_subject(chat, topic).await
    }

    /// Sessions d'un sujet, présentes et passées, les plus anciennes d'abord.
    async fn topic_sessions(&self, chat_id: i64, topic_id: i64) -> Vec<String> {
        self.daemon
            .services
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id FROM sessions WHERE tg_chat_id = ?1 AND tg_topic_id = ?2
                     ORDER BY created_at, id",
                )?;
                let rows = st.query_map(params![chat_id, topic_id], |r| r.get::<_, String>(0))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    /// Les sujets nommés qui valent projet, avec leurs sessions : tout ce que le canal a
    /// appris (`tg.topic_name.*`), hors « Général » et foyer.
    pub(super) async fn named_subjects(&self) -> Vec<Subject> {
        debug_assert!(topic_name_key(1, 2).starts_with(TOPIC_NAME_PREFIX));
        let named: Vec<(i64, i64, String)> = self
            .daemon
            .services
            .store
            .read(|c| {
                let mut st = c.prepare("SELECT k, v FROM kv WHERE k LIKE ?1 ORDER BY k")?;
                let rows = st.query_map([format!("{TOPIC_NAME_PREFIX}%")], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?;
                let mut out = Vec::new();
                for row in rows {
                    let (k, v) = row?;
                    let mut ids = k[TOPIC_NAME_PREFIX.len()..].split('.');
                    if let (Some(Ok(chat)), Some(Ok(topic))) =
                        (ids.next().map(str::parse), ids.next().map(str::parse))
                    {
                        out.push((chat, topic, v));
                    }
                }
                Ok(out)
            })
            .await
            .unwrap_or_default();
        let mut subjects = Vec::new();
        for (chat, topic, _) in named {
            let Some(name) = self.topic_subject(chat, topic).await else {
                continue;
            };
            subjects.push(Subject {
                name,
                sessions: self.topic_sessions(chat, topic).await,
            });
        }
        subjects
    }

    /// Au démarrage : chaque sujet nommé devient un projet, ses sessions y sont
    /// rattachées. Idempotente ; le compte rendu va au digest du lendemain.
    pub async fn adopt_subjects(&self) -> MigrationReport {
        let s = &self.daemon.services;
        let subjects = self.named_subjects().await;
        let vault = penelope_app::helpers::vault_dir(s);
        migrate(s, &vault, &subjects).await
    }

    /// Un sujet vient d'être nommé (`forum_topic_created`) ou renommé
    /// (`forum_topic_edited`, `previous` : l'ancien nom) : sa fiche et ses sessions suivent.
    pub(super) async fn subject_named(
        &self,
        chat_id: i64,
        topic_id: i64,
        name: &str,
        previous: Option<&str>,
    ) {
        let s = &self.daemon.services;
        if self.topic_subject(chat_id, topic_id).await.is_none() {
            return;
        }
        let subject = Subject {
            name: name.to_string(),
            sessions: self.topic_sessions(chat_id, topic_id).await,
        };
        let vault = penelope_app::helpers::vault_dir(s);
        let a = adopt(s, &vault, &subject, previous).await;
        tracing::info!(
            project = %a.project,
            created = a.created,
            moved = a.moved,
            attached = a.attached,
            corrected = a.corrected,
            "sujet nommé, projet suivi"
        );
    }
}
