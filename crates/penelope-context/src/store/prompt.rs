//! Le préfixe système retenu et les instantanés envoyés (issue #17, #205 ; épopée #208,
//! T16 : la clé `kv prompt.prefix.*` est retirée, le journal la remplace).

use super::*;
use crate::journal::{KIND_ASSISTANT, KIND_ATTEMPT, KIND_SYSTEM, SystemPayload};
use crate::tiers::Tiers;
use dual::system_in;
use penelope_kernel::event::EventDraft;
use penelope_kernel::journal::TileMap;

/// Un projet fixé à la main pour la session (`penelope-vault`, `session_project::set`).
pub const KIND_SESSION_PROJECT: &str = "session.project";

/// Les événements qui libèrent le préfixe retenu : une compaction (le cache est cassé de
/// toute façon), un projet fixé à la main (l'instantané de l'épisode est refigé au tour
/// suivant).
pub const PREFIX_RELEASES: &[&str] = &["context.compacted", KIND_SESSION_PROJECT];

impl HistoryStore {
    /// Le préfixe (T0, T1, T2) du dernier `conv.system` de la session, relu dans le
    /// journal, tant qu'il tient : `None` sans préfixe journalisé par la session
    /// elle-même, pour un payload purgé, ou si un [`PREFIX_RELEASES`] est arrivé depuis le
    /// dernier appel au modèle (ou le dernier préfixe). Le volatil (T4) est vide.
    pub async fn retained_prefix(&self, session_id: &str) -> penelope_store::Result<Option<Tiers>> {
        let sid = session_id.to_string();
        self.store
            .read(move |c| {
                let Some(seq) = system_in(c, &sid)?.and_then(|at| at.own_seq) else {
                    return Ok(None);
                };
                let pinned: i64 = c.query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM events
                     WHERE session_id = ?1 AND kind IN (?2, ?3, ?4)",
                    params![sid, KIND_SYSTEM, KIND_ASSISTANT, KIND_ATTEMPT],
                    |r| r.get(0),
                )?;
                let released = c
                    .query_row(
                        "SELECT 1 FROM events WHERE session_id = ?1 AND seq > ?2
                           AND kind IN (?3, ?4) LIMIT 1",
                        params![sid, pinned, PREFIX_RELEASES[0], PREFIX_RELEASES[1]],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if released {
                    return Ok(None);
                }
                let payload: String = c.query_row(
                    "SELECT payload FROM events WHERE session_id = ?1 AND seq = ?2",
                    params![sid, seq],
                    |r| r.get(0),
                )?;
                let Ok(p) = serde_json::from_str::<SystemPayload>(&payload) else {
                    return Ok(None);
                };
                let tile = |name| p.tiles.slice(&p.rendered, name).map(String::from);
                Ok(match (tile("T0"), tile("T1"), tile("T2")) {
                    (Some(identity), Some(index), Some(context)) => Some(Tiers {
                        identity,
                        index,
                        context,
                        volatile: String::new(),
                    }),
                    _ => None,
                })
            })
            .await
    }

    /// Compte un envoi du prompt système `hash` dans son instantané (`uses`,
    /// `last_seen_at`) ; l'écrit s'il manque (préfixe hérité d'une mère, sans
    /// `conv.system` à la session).
    pub async fn prompt_sent(
        &self,
        hash: &str,
        rendered: &str,
        tiles: Option<&TileMap>,
    ) -> penelope_store::Result<()> {
        let (hash, rendered, ts) = (
            hash.to_string(),
            rendered.to_string(),
            self.clock.now_rfc3339(),
        );
        let tiles = tiles.and_then(|t| serde_json::to_string(t).ok());
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO prompt_snapshots(hash, rendered, tiers, first_seen_at,
                        last_seen_at, uses)
                     VALUES(?1,?2,?3,?4,?4,1)
                     ON CONFLICT(hash) DO UPDATE SET last_seen_at = ?4, uses = uses + 1",
                    params![hash, rendered, tiles, ts],
                )?;
                Ok(())
            })
            .await
    }
}

/// Une différence du préfixe envoyée en fin (issue #236, `tiers::update`).
pub const KIND_PROMPT_UPDATED: &str = "prompt.updated";
/// Une skill chargée dans la session (`skill_load`), avec l'empreinte de son corps.
pub const KIND_SKILL_LOADED: &str = "skill.loaded";

/// `prompt.updated` : ce que le modèle sait désormais, en plus du préfixe retenu.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PromptUpdated {
    /// Empreinte du préfixe retenu, auquel la différence s'ajoute (`None` : skills seules).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Le préfixe que la différence décrit, en entier, pour la différence suivante.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiles: Option<TileMap>,
    /// Tuiles changées, et celles trop changées pour être recopiées.
    #[serde(default)]
    pub changed: Vec<String>,
    #[serde(default)]
    pub rewritten: Vec<String>,
    /// Skills annoncées modifiées ou retirées : nom → empreinte du corps (vide : retirée).
    #[serde(default)]
    pub skills: BTreeMap<String, String>,
    /// Taille du bloc envoyé, en caractères.
    pub chars: usize,
}

/// Ce que le modèle d'une session sait au-delà de son préfixe retenu.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Announced {
    /// Le dernier préfixe annoncé depuis le dernier `conv.system`, et l'empreinte du
    /// préfixe retenu auquel il s'ajoute.
    pub prefix: Option<(String, Tiers)>,
    /// Skills déjà annoncées : nom → empreinte du corps annoncé.
    pub skills: BTreeMap<String, String>,
    /// Skills chargées : nom → empreinte du corps au dernier chargement.
    pub loaded: BTreeMap<String, String>,
}

impl HistoryStore {
    /// Relit dans le journal ce que la session a déjà reçu en fin de conversation.
    pub async fn announced(&self, session_id: &str) -> penelope_store::Result<Announced> {
        let sid = session_id.to_string();
        self.store
            .read(move |c| {
                let since: i64 = c.query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1 AND kind = ?2",
                    params![sid, KIND_SYSTEM],
                    |r| r.get(0),
                )?;
                let mut out = Announced::default();
                let mut st = c.prepare(
                    "SELECT seq, kind, payload FROM events
                     WHERE session_id = ?1 AND kind IN (?2, ?3) ORDER BY seq",
                )?;
                let rows =
                    st.query_map(params![sid, KIND_PROMPT_UPDATED, KIND_SKILL_LOADED], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    })?;
                for row in rows {
                    let (seq, kind, payload) = row?;
                    if kind == KIND_SKILL_LOADED {
                        let v: Value = serde_json::from_str(&payload).unwrap_or_default();
                        if let (Some(n), Some(h)) = (v["name"].as_str(), v["body_hash"].as_str()) {
                            out.loaded.insert(n.to_string(), h.to_string());
                        }
                        continue;
                    }
                    let Ok(p) = serde_json::from_str::<PromptUpdated>(&payload) else {
                        continue;
                    };
                    out.skills.extend(p.skills);
                    if seq <= since {
                        continue;
                    }
                    if let (Some(base), Some(rendered), Some(tiles)) = (p.base, p.rendered, p.tiles)
                    {
                        let tile = |name| tiles.slice(&rendered, name).map(String::from);
                        if let (Some(identity), Some(index), Some(context)) =
                            (tile("T0"), tile("T1"), tile("T2"))
                        {
                            let tiers = Tiers {
                                identity,
                                index,
                                context,
                                volatile: String::new(),
                            };
                            out.prefix = Some((base, tiers));
                        }
                    }
                }
                Ok(out)
            })
            .await
    }

    /// Journalise une différence envoyée (`prompt.updated`).
    pub async fn announce(
        &self,
        session_id: &str,
        update: &PromptUpdated,
    ) -> penelope_store::Result<()> {
        let payload = serde_json::to_value(update)
            .map_err(|e| penelope_store::StoreError::other(e.to_string()))?;
        self.events
            .append(EventDraft::new(KIND_PROMPT_UPDATED, payload).session(session_id))
            .await
            .map_err(|e| penelope_store::StoreError::other(e.to_string()))?;
        Ok(())
    }
}
