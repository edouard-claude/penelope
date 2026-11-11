//! Mission mise en attente par le run qui l'a lancée (#456).
//!
//! Un run en pause, ou bloqué par ses bornes, laissait tourner la mission de son étape
//! `mission` : elle vit dans une autre session, et rien ne lui disait d'attendre. Elle
//! codait, poussait et dépensait pendant que la carte du run disait « en pause ».
//!
//! ```text
//!  run en pause, run borné ──► hold ──► demande posée, relance en file annulée
//!                                         │ aucun tour en cours ...... rangée tout de suite
//!                                         │ tour ou vérification en cours : il finit, en
//!                                         │   autonomie, puis `after_turn` range la mission
//!                                         ▼   avec la consigne du tour suivant
//!                              `Waiting` + `hold` : ni tour, ni autonomie, ni dépense
//!  run repris ────────────► release ──► `Running`, échéance reculée de la durée de
//!                                         l'attente, la consigne gardée repart
//! ```
//!
//! Ce n'est pas un arrêt. Une mission qui finit pendant son dernier tour (terminée,
//! bloquée, budget) garde cet état ; et l'attente qu'elle rend d'elle-même au propriétaire
//! (`Waiting` sans `hold`) n'est jamais relancée par un run.

use super::{Mission, State, brief, check, event, key, with_relay};
use crate::services::Services;
use serde::{Deserialize, Serialize};

/// Motif d'annulation de la relance en file d'une mission mise en attente.
const HELD_TURN: &str = "mission en attente avec son run";

/// La demande d'attente d'un run, portée par la mission tant qu'elle n'est pas levée.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hold {
    /// Qui demande : le run, par son identifiant. La mission repart quand il avance.
    pub by: String,
    pub at_ms: i64,
    /// Pourquoi : « son run est en pause », « son run a atteint une borne ».
    pub why: String,
    /// La consigne du tour suivant, quand la mission s'est rangée à la fin d'un tour.
    #[serde(default)]
    pub next: Option<String>,
}

/// Ce que devient une mission à qui son run demande d'attendre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// Rangée : plus aucun tour ne part avant la reprise.
    Parked,
    /// Son tour ou sa vérification finit ; elle se rangera ensuite, ou aura fini.
    Turning,
    /// Pas de mission, ou déjà finie : rien à ranger.
    Settled,
}

/// Le tour à mettre en file pour relancer une mission rangée.
#[derive(Debug, Clone, PartialEq)]
pub struct Relaunch {
    pub text: String,
    pub origin: serde_json::Value,
    pub dedup: String,
}

/// Relit la mission et l'écrit dans la même transaction si `change` le demande. Rend la
/// mission d'avant et celle d'après ; rien s'il n'y en a pas.
async fn edit(
    s: &Services,
    session_id: &str,
    change: impl FnOnce(&mut Mission) -> bool + Send + 'static,
) -> anyhow::Result<Option<(Mission, Mission)>> {
    let k = key(session_id);
    Ok(s.store
        .write(move |tx| {
            let Some(before) = penelope_store::kv_get(tx, &k)?
                .and_then(|raw| serde_json::from_str::<Mission>(&raw).ok())
            else {
                return Ok(None);
            };
            let mut after = before.clone();
            if change(&mut after) {
                penelope_store::kv_set(tx, &k, &serde_json::to_string(&after)?)?;
            }
            Ok(Some((before, after)))
        })
        .await?)
}

/// Le run de la mission cesse d'avancer : elle attend avec lui. Sans effet sur une mission
/// finie, ni sur celle qui attend déjà.
pub async fn hold(s: &Services, session_id: &str, by: &str, why: &str) -> anyhow::Result<Held> {
    let asked = Hold {
        by: by.to_string(),
        at_ms: s.clock.now_ms(),
        why: why.to_string(),
        next: None,
    };
    let flagged = edit(s, session_id, move |m| {
        let ask = m.state == State::Running && m.hold.is_none();
        if ask {
            m.hold = Some(asked);
        }
        ask
    })
    .await?;
    match flagged {
        Some((_, m)) if m.parked() => return Ok(Held::Parked),
        Some((_, m)) if m.state == State::Running => {}
        _ => return Ok(Held::Settled),
    }
    // La relance déjà en file ne part pas. Un tour en cours finit, une vérification
    // aussi : `after_turn` rangera la mission, ou la clora si elle a fini.
    s.turns.cancel_pending(session_id, HELD_TURN).await?;
    if s.turns.queued_for(session_id).await? > 0 || check::running(s, session_id) {
        return Ok(Held::Turning);
    }
    Ok(match park(s, session_id, None).await? {
        Some(_) => Held::Parked,
        None => Held::Settled,
    })
}

/// Range la mission à qui son run a demandé d'attendre, avec la consigne du tour suivant
/// s'il y en a une. Rend la demande si la mission est rangée, à l'instant ou déjà ; rien
/// si personne ne lui a demandé d'attendre ou si elle a fini.
pub(super) async fn park(
    s: &Services,
    session_id: &str,
    next: Option<String>,
) -> anyhow::Result<Option<Hold>> {
    let edited = edit(s, session_id, move |m| {
        let Some(hold) = m.hold.as_mut() else {
            return false;
        };
        if m.state != State::Running {
            return false;
        }
        hold.next = next;
        m.note = format!("en attente : {}", hold.why);
        m.state = State::Waiting;
        true
    })
    .await?;
    let Some((before, after)) = edited else {
        return Ok(None);
    };
    if !after.parked() {
        return Ok(None);
    }
    if before.state == State::Running {
        event(s, &after, "mission.held").await;
        // Une relance mise en file entre la demande et le rangement ne part pas non plus.
        s.turns.cancel_pending(session_id, HELD_TURN).await?;
    }
    Ok(after.hold)
}

/// Le run repart : la mission rangée redevient en cours, son échéance reculée du temps
/// passé à attendre. Rend le tour à mettre en file ; rien si elle n'était pas rangée (son
/// tour n'avait pas fini, elle a fini, ou c'est le propriétaire qu'elle attend).
pub async fn release(s: &Services, session_id: &str) -> anyhow::Result<Option<Relaunch>> {
    let now = s.clock.now_ms();
    let edited = edit(s, session_id, move |m| {
        let Some(hold) = m.hold.take() else {
            return false;
        };
        if m.state == State::Waiting {
            m.state = State::Running;
            m.deadline_ms += (now - hold.at_ms).max(0);
            m.note = "reprise avec son run".into();
        }
        true
    })
    .await?;
    let Some((before, after)) = edited else {
        return Ok(None);
    };
    if !before.parked() {
        return Ok(None);
    }
    event(s, &after, "mission.resumed").await;
    let text = match before.hold.and_then(|h| h.next) {
        Some(next) => next,
        None => brief(&after, Some("reprise après l'attente de son run")),
    };
    Ok(Some(Relaunch {
        text: with_relay(s, &after, text),
        origin: after.origin.clone(),
        dedup: format!("mission:{session_id}:{}:reprise:{now}", after.turns),
    }))
}
