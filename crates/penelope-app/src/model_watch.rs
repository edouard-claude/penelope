//! Écarts de modèle, annoncés une fois par changement d'état (#333).
//!
//! Un écart, c'est un appel servi par un autre modèle que celui que le profil choisit :
//! repli après panne, garde Codex appliquée à un travail de fond, principal injoignable.
//! Chacun est dit dans la conversation concernée au moment où il se produit, puis tu
//! tant qu'il dure ; le retour à la normale est dit à son tour.
//!
//! ```text
//!  appel servi par X ──► écart sous la clé K ? ──non──► rien (ou « ✅ retour » si K
//!                              │                         était en écart sur un autre)
//!                              oui
//!                              ▼
//!                  K déjà en écart sur X ? ──oui──► rien (date du dernier vu)
//!                              │ non
//!                              ▼
//!          kv `model.deviation.K` ◄── écrit ── événement `model.notice` ──► canal
//! ```
//!
//! Le cœur ne nomme pas le canal : l'événement porte le texte, l'origine si elle est
//! connue et la session ; le canal branché l'écrit là où il faut (#214). Le module ne
//! touche que la base et le journal : la boucle d'agent s'en sert sans `Services`.

use crate::bus::Origin;
use penelope_kernel::event::{EventDraft, EventLog};
use penelope_store::Store;
use penelope_store::rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Événement d'une annonce : le canal l'écrit dans la conversation.
pub const NOTICE_EVENT: &str = "model.notice";
/// Préfixe `kv` des écarts en cours.
const PREFIX: &str = "model.deviation.";
/// Un écart vu dans ce délai est « en cours » pour `/model`.
pub const RECENT_MS: i64 = 24 * 3_600_000;

/// Un écart en cours.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deviation {
    pub key: String,
    /// Le modèle qui sert pendant l'écart.
    pub served: String,
    pub text: String,
    /// Début de l'écart et dernier appel vu, en ms.
    pub since_ms: i64,
    pub last_ms: i64,
}

/// Où dire un écart.
#[derive(Debug, Clone, Default)]
pub struct Place<'a> {
    pub session: Option<&'a str>,
    pub origin: Option<&'a Origin>,
}

/// Le journal et la base, ce qu'une annonce touche : la boucle d'agent les a sans
/// `Services`.
pub struct Watch<'a> {
    pub store: &'a Store,
    pub events: &'a EventLog,
    pub now_ms: i64,
}

impl Watch<'_> {
    async fn read(&self, key: &str) -> Option<Deviation> {
        let k = format!("{PREFIX}{key}");
        let raw: Option<String> = self
            .store
            .read(move |c| {
                Ok(
                    c.query_row("SELECT v FROM kv WHERE k = ?1", [k], |r| r.get(0))
                        .optional()?,
                )
            })
            .await
            .ok()
            .flatten();
        raw.and_then(|r| serde_json::from_str(&r).ok())
    }

    async fn write(&self, d: Option<&Deviation>, key: &str) {
        let k = format!("{PREFIX}{key}");
        let v = d.and_then(|d| serde_json::to_string(d).ok());
        let r = self
            .store
            .write(move |tx| {
                match v {
                    Some(v) => tx.execute(
                        "INSERT INTO kv(k, v, ts) VALUES(?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                         ON CONFLICT(k) DO UPDATE SET v = excluded.v, ts = excluded.ts",
                        params![k, v],
                    )?,
                    None => tx.execute("DELETE FROM kv WHERE k = ?1", [k])?,
                };
                Ok(())
            })
            .await;
        if let Err(e) = r {
            tracing::warn!(error = %e, "état d'écart de modèle non écrit");
        }
    }

    async fn announce(&self, place: &Place<'_>, key: &str, text: &str) {
        let mut draft = EventDraft::new(
            NOTICE_EVENT,
            json!({
                "key": key,
                "text": text,
                "origin": place.origin.map(Origin::to_value),
            }),
        );
        if let Some(session) = place.session {
            draft = draft.session(session);
        }
        if let Err(e) = self.events.append(draft).await {
            tracing::warn!(error = %e, "annonce d'écart de modèle non écrite");
        }
    }

    /// `served` sert à la place du modèle choisi : annoncé si l'état change.
    pub async fn deviate(&self, place: &Place<'_>, key: &str, served: &str, text: &str) {
        match self.read(key).await {
            Some(mut d) if d.served == served => {
                d.last_ms = self.now_ms;
                self.write(Some(&d), key).await;
            }
            _ => {
                let d = Deviation {
                    key: key.to_string(),
                    served: served.to_string(),
                    text: text.to_string(),
                    since_ms: self.now_ms,
                    last_ms: self.now_ms,
                };
                self.write(Some(&d), key).await;
                self.announce(place, key, text).await;
            }
        }
    }

    /// `served` sert, comme le profil le veut : un écart en cours sur un autre modèle
    /// prend fin, et `text` le dit.
    pub async fn settle(&self, place: &Place<'_>, key: &str, served: &str, text: &str) {
        if let Some(d) = self.read(key).await
            && d.served != served
        {
            self.write(None, key).await;
            self.announce(place, key, text).await;
        }
    }

    /// Écarts vus dans les dernières 24 h, les plus récents d'abord.
    pub async fn recent(&self) -> Vec<Deviation> {
        let like = format!("{PREFIX}%");
        let rows: Vec<String> = self
            .store
            .read(move |c| {
                let mut st = c.prepare("SELECT v FROM kv WHERE k LIKE ?1")?;
                let rows = st.query_map([like], |r| r.get::<_, String>(0))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut out: Vec<Deviation> = rows
            .iter()
            .filter_map(|r| serde_json::from_str::<Deviation>(r).ok())
            .filter(|d| self.now_ms - d.last_ms <= RECENT_MS)
            .collect();
        out.sort_by_key(|d| std::cmp::Reverse(d.last_ms));
        out
    }
}

/// Nom court d'un modèle, pour une annonce.
pub fn short(model_id: &str) -> &str {
    let bare = penelope_llm::catalog::strip_provider(model_id);
    bare.rsplit('/').next().unwrap_or(bare)
}

/// Clé de l'écart d'une conversation (repli après panne).
pub fn fallback_key(session_id: &str) -> String {
    format!("fallback.{session_id}")
}
