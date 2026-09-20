//! Actions et jetons de rappel (§14.8).
//!
//! `callback_data` est limité à 64 octets : Pénélope y met un **jeton opaque**
//! (`a:<base62 12>`) lié en base à `{action, target, args, expires_at, single_use}`.
//! Un clic répété sur une action à usage unique répond « déjà traité » et met la carte à
//! jour, sans rejouer l'effet.

use penelope_kernel::clock::SharedClock;
use penelope_store::{Store, rusqlite::params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Limite stricte de l'API Bot.
pub const CALLBACK_DATA_MAX: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub token: String,
    pub action: String,
    pub target: String,
    pub args: Value,
    pub created_at: String,
    pub expires_at: String,
    pub single_use: bool,
    pub consumed_at: Option<String>,
    pub consumed_by: Option<i64>,
}

/// Résultat d'un clic.
#[derive(Debug, Clone, PartialEq)]
pub enum ClickOutcome {
    /// Première consommation : l'action doit être exécutée.
    Accepted(Action),
    /// Déjà traité : répondre « déjà traité », mettre la carte à jour, ne rien rejouer.
    AlreadyHandled(Action),
    Expired,
    Unknown,
    /// L'émetteur du clic n'est pas le propriétaire.
    NotOwner,
}

#[derive(Clone)]
pub struct ActionStore {
    store: Store,
    clock: SharedClock,
    owner_id: i64,
}

impl ActionStore {
    pub fn new(store: Store, clock: SharedClock, owner_id: i64) -> Self {
        ActionStore {
            store,
            clock,
            owner_id,
        }
    }

    /// Crée un jeton.
    pub async fn create(
        &self,
        action: &str,
        target: &str,
        args: Value,
        ttl_ms: i64,
        single_use: bool,
    ) -> penelope_store::Result<Action> {
        let token = format!("a:{}", penelope_kernel::ids::short_token(12));
        debug_assert!(token.len() <= CALLBACK_DATA_MAX);
        let now_ms = self.clock.now_ms();
        let a = Action {
            token: token.clone(),
            action: action.to_string(),
            target: target.to_string(),
            args,
            created_at: self.clock.now_rfc3339(),
            expires_at: ms_to_rfc3339(now_ms + ttl_ms),
            single_use,
            consumed_at: None,
            consumed_by: None,
        };
        let row = a.clone();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO tg_actions(token, action, target, args, created_at, expires_at,
                        single_use)
                     VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        row.token,
                        row.action,
                        row.target,
                        row.args.to_string(),
                        row.created_at,
                        row.expires_at,
                        row.single_use as i64
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(a)
    }

    /// Consomme un jeton. **Idempotent** : la seconde consommation renvoie
    /// `AlreadyHandled`.
    pub async fn click(&self, token: &str, from_id: i64) -> penelope_store::Result<ClickOutcome> {
        if from_id != self.owner_id {
            return Ok(ClickOutcome::NotOwner);
        }
        let (token, now) = (token.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                let existing: Option<Action> = {
                    let mut st = tx.prepare(
                        "SELECT token, action, target, args, created_at, expires_at, single_use,
                                consumed_at, consumed_by FROM tg_actions WHERE token = ?1",
                    )?;
                    let mut rows = st.query([&token])?;
                    match rows.next()? {
                        Some(r) => Some(row_to_action(r)?),
                        None => None,
                    }
                };
                let Some(a) = existing else {
                    return Ok(ClickOutcome::Unknown);
                };
                if a.expires_at <= now {
                    return Ok(ClickOutcome::Expired);
                }
                if a.single_use && a.consumed_at.is_some() {
                    return Ok(ClickOutcome::AlreadyHandled(a));
                }
                let n = tx.execute(
                    "UPDATE tg_actions SET consumed_at=?2, consumed_by=?3
                     WHERE token=?1 AND (single_use = 0 OR consumed_at IS NULL)",
                    params![token, now, from_id],
                )?;
                if n == 0 {
                    return Ok(ClickOutcome::AlreadyHandled(a));
                }
                Ok(ClickOutcome::Accepted(Action {
                    consumed_at: Some(now),
                    consumed_by: Some(from_id),
                    ..a
                }))
            })
            .await
    }

    pub async fn get(&self, token: &str) -> penelope_store::Result<Option<Action>> {
        let token = token.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT token, action, target, args, created_at, expires_at, single_use,
                            consumed_at, consumed_by FROM tg_actions WHERE token = ?1",
                )?;
                let mut rows = st.query([&token])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_action(r)?)),
                    None => Ok(None),
                }
            })
            .await
    }

    /// Supprime les jetons échus.
    pub async fn purge_expired(&self) -> penelope_store::Result<usize> {
        let now = self.clock.now_rfc3339();
        self.store
            .write(
                move |tx| Ok(tx.execute("DELETE FROM tg_actions WHERE expires_at <= ?1", [now])?),
            )
            .await
    }
}

fn row_to_action(
    r: &penelope_store::rusqlite::Row<'_>,
) -> penelope_store::rusqlite::Result<Action> {
    let args: String = r.get(3)?;
    Ok(Action {
        token: r.get(0)?,
        action: r.get(1)?,
        target: r.get(2)?,
        args: serde_json::from_str(&args).unwrap_or(Value::Null),
        created_at: r.get(4)?,
        expires_at: r.get(5)?,
        single_use: r.get::<_, i64>(6)? != 0,
        consumed_at: r.get(7)?,
        consumed_by: r.get(8)?,
    })
}

fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Mention ajoutée à la carte après décision (§14.8).
pub fn decided_footer(decision: &str, at_local: &str, via: &str) -> String {
    format!("✅ {decision} par toi à {at_local} via {via}")
}

/// Actions reconnues par le routeur de CTA.
pub mod kind {
    pub const APPROVE: &str = "approve";
    pub const APPROVE_RUN: &str = "approve_run";
    pub const APPROVE_ALWAYS: &str = "approve_always";
    pub const DENY: &str = "deny";
    pub const DENY_REASON: &str = "deny_reason";
    pub const CONFIRM_DESTRUCTIVE: &str = "confirm_destructive";
    pub const RUN_PAUSE: &str = "run_pause";
    pub const RUN_RESUME: &str = "run_resume";
    pub const RUN_CANCEL: &str = "run_cancel";
    pub const RUN_TRACE: &str = "run_trace";
    pub const RUN_RETRY_STEP: &str = "run_retry_step";
    pub const RUN_SKIP_STEP: &str = "run_skip_step";
    pub const REGENERATE: &str = "regenerate";
    pub const ESCALATE_MODEL: &str = "escalate_model";
    pub const MEMORISE: &str = "memorise";
    pub const CHOICE: &str = "choice";
    pub const FORM_NEXT: &str = "form_next";
    pub const FORM_PREV: &str = "form_prev";
    pub const FORM_SUBMIT: &str = "form_submit";
    pub const FORM_DECLINE: &str = "form_decline";
    pub const OAUTH_RETRY: &str = "oauth_retry";
    pub const OAUTH_PASTED: &str = "oauth_pasted";
    /// Élicitation MCP (`target` : identifiant de la demande) : accepter (confirmer, remplir
    /// le formulaire ou ouvrir le lien), refuser, annuler, lien terminé.
    pub const ELICIT_ACCEPT: &str = "elicit_accept";
    pub const ELICIT_DECLINE: &str = "elicit_decline";
    pub const ELICIT_CANCEL: &str = "elicit_cancel";
    pub const ELICIT_DONE: &str = "elicit_done";
    /// Relance une demande annulée par le délai : le tour reprend dans la session qui
    /// l'avait provoquée (issue #143).
    pub const ELICIT_RETRY: &str = "elicit_retry";
    pub const SCHEDULE_ENABLE: &str = "schedule_enable";
    pub const WORKFLOW_SAVE: &str = "workflow_save";
    pub const WORKFLOW_RUN_ONCE: &str = "workflow_run_once";
    pub const SKILL_ACTIVATE: &str = "skill_activate";
    pub const MEMORY_ACCEPT: &str = "memory_accept";
    pub const MEMORY_AS_EXCEPTION: &str = "memory_as_exception";
    pub const MEMORY_REJECT: &str = "memory_reject";
    /// Épingle un alias sur une session (`args.alias`), ou revient à l'automatique.
    pub const MODEL_PIN: &str = "model_pin";
    pub const BUDGET_RAISE: &str = "budget_raise";
    pub const BUDGET_STOP: &str = "budget_stop";
    pub const STOP_RESUME: &str = "stop_resume";
    pub const STOP_FORGET: &str = "stop_forget";
    pub const DOCTOR: &str = "doctor";
    pub const EFFECT_VERIFY: &str = "effect_verify";
    pub const EFFECT_RETRY: &str = "effect_retry";
    pub const EFFECT_IGNORE: &str = "effect_ignore";
    /// Menu `/sessions` : basculer, sous-menu, forker, renommer, fermer, page (`args.page`,
    /// `args.all`).
    pub const SESSION_SWITCH: &str = "session_switch";
    pub const SESSION_MENU: &str = "session_menu";
    pub const SESSION_FORK: &str = "session_fork";
    pub const SESSION_RENAME: &str = "session_rename";
    pub const SESSION_CLOSE: &str = "session_close";
    pub const SESSIONS_PAGE: &str = "sessions_page";
    /// Entretien d'accueil (`target` : fichier de la séance) : commencer, répondre
    /// (`args.n`, `args.answer`, `null` pour passer), pause, écrire, annuler.
    pub const ONBOARD_START: &str = "onboard_start";
    pub const ONBOARD_ANSWER: &str = "onboard_answer";
    pub const ONBOARD_PAUSE: &str = "onboard_pause";
    pub const ONBOARD_WRITE: &str = "onboard_write";
    pub const ONBOARD_CANCEL: &str = "onboard_cancel";
    /// Écrans de commandes (issue #30) : `target` nomme l'écran à (re)dessiner, `args` ses
    /// paramètres (page, filtre…).
    pub const SCREEN: &str = "screen";
    /// Opération d'un écran : `target` nomme l'opération, `args` = `{params, back}`.
    pub const SCREEN_DO: &str = "screen_do";
    /// Exécute une commande du catalogue sans argument (`target` : son nom).
    pub const RUN_COMMAND: &str = "run_command";
    /// Envoie `args.text` comme message du propriétaire dans la session `target` (suites
    /// proposées après une boucle arrêtée, issue #31).
    pub const SAY: &str = "say";

    pub const ALL: &[&str] = &[
        APPROVE,
        APPROVE_RUN,
        APPROVE_ALWAYS,
        DENY,
        DENY_REASON,
        CONFIRM_DESTRUCTIVE,
        RUN_PAUSE,
        RUN_RESUME,
        RUN_CANCEL,
        RUN_TRACE,
        RUN_RETRY_STEP,
        RUN_SKIP_STEP,
        REGENERATE,
        ESCALATE_MODEL,
        MEMORISE,
        CHOICE,
        FORM_NEXT,
        FORM_PREV,
        FORM_SUBMIT,
        FORM_DECLINE,
        OAUTH_RETRY,
        OAUTH_PASTED,
        ELICIT_ACCEPT,
        ELICIT_DECLINE,
        ELICIT_CANCEL,
        ELICIT_DONE,
        SCHEDULE_ENABLE,
        WORKFLOW_SAVE,
        WORKFLOW_RUN_ONCE,
        SKILL_ACTIVATE,
        MEMORY_ACCEPT,
        MEMORY_AS_EXCEPTION,
        MEMORY_REJECT,
        BUDGET_RAISE,
        BUDGET_STOP,
        STOP_RESUME,
        STOP_FORGET,
        DOCTOR,
        EFFECT_VERIFY,
        EFFECT_RETRY,
        EFFECT_IGNORE,
        SESSION_SWITCH,
        SESSION_MENU,
        SESSION_FORK,
        SESSION_RENAME,
        SESSION_CLOSE,
        SESSIONS_PAGE,
        ONBOARD_START,
        ONBOARD_ANSWER,
        ONBOARD_PAUSE,
        ONBOARD_WRITE,
        ONBOARD_CANCEL,
        SCREEN,
        SCREEN_DO,
        RUN_COMMAND,
        SAY,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use serde_json::json;
    use std::sync::Arc;

    fn actions(clock: TestClock) -> ActionStore {
        ActionStore::new(Store::open_memory().unwrap(), Arc::new(clock), 42)
    }

    #[tokio::test]
    async fn tokens_fit_in_callback_data() {
        let s = actions(TestClock::default());
        let a = s
            .create(
                kind::APPROVE,
                "a_01J",
                json!({"window":"once"}),
                86_400_000,
                true,
            )
            .await
            .unwrap();
        assert!(a.token.len() <= CALLBACK_DATA_MAX, "{}", a.token.len());
        assert!(a.token.starts_with("a:"));
        // Le jeton est opaque : la cible n'y apparaît pas.
        assert!(!a.token.contains("a_01J"));
    }

    /// CA 14 : un clic double est idempotent.
    #[tokio::test]
    async fn ca_14_4_double_click_is_idempotent() {
        let s = actions(TestClock::default());
        let a = s
            .create(kind::APPROVE, "a_1", json!({}), 86_400_000, true)
            .await
            .unwrap();

        match s.click(&a.token, 42).await.unwrap() {
            ClickOutcome::Accepted(x) => assert_eq!(x.action, kind::APPROVE),
            other => panic!("{other:?}"),
        }
        match s.click(&a.token, 42).await.unwrap() {
            ClickOutcome::AlreadyHandled(_) => {}
            other => panic!("attendu AlreadyHandled, obtenu {other:?}"),
        }
    }

    #[tokio::test]
    async fn multi_use_tokens_can_be_clicked_repeatedly() {
        let s = actions(TestClock::default());
        let a = s
            .create(kind::RUN_TRACE, "r_1", json!({}), 86_400_000, false)
            .await
            .unwrap();
        for _ in 0..3 {
            assert!(matches!(
                s.click(&a.token, 42).await.unwrap(),
                ClickOutcome::Accepted(_)
            ));
        }
    }

    /// CA 14 : un utilisateur non autorisé est ignoré.
    #[tokio::test]
    async fn ca_14_5_non_owner_clicks_are_refused() {
        let s = actions(TestClock::default());
        let a = s
            .create(kind::APPROVE, "a_1", json!({}), 86_400_000, true)
            .await
            .unwrap();
        assert_eq!(
            s.click(&a.token, 999).await.unwrap(),
            ClickOutcome::NotOwner
        );
        // L'action reste disponible pour le propriétaire.
        assert!(matches!(
            s.click(&a.token, 42).await.unwrap(),
            ClickOutcome::Accepted(_)
        ));
    }

    #[tokio::test]
    async fn expired_tokens_are_refused_and_purged() {
        let clock = TestClock::default();
        let s = actions(clock.clone());
        let a = s
            .create(kind::APPROVE, "a_1", json!({}), 3_600_000, true)
            .await
            .unwrap();
        clock.advance_hours(2);
        assert_eq!(s.click(&a.token, 42).await.unwrap(), ClickOutcome::Expired);
        assert_eq!(s.purge_expired().await.unwrap(), 1);
        assert_eq!(s.click(&a.token, 42).await.unwrap(), ClickOutcome::Unknown);
    }

    #[tokio::test]
    async fn unknown_token_is_reported() {
        let s = actions(TestClock::default());
        assert_eq!(
            s.click("a:inexistant", 42).await.unwrap(),
            ClickOutcome::Unknown
        );
    }

    #[tokio::test]
    async fn args_survive_the_roundtrip() {
        let s = actions(TestClock::default());
        let a = s
            .create(
                kind::CHOICE,
                "r_1:step_5",
                json!({"choice":"Déployer","index":0}),
                60_000,
                true,
            )
            .await
            .unwrap();
        let back = s.get(&a.token).await.unwrap().unwrap();
        assert_eq!(back.args["choice"], "Déployer");
        assert_eq!(back.target, "r_1:step_5");
    }

    #[test]
    fn decided_footer_names_the_channel() {
        let f = decided_footer("Autorisé", "14:32", "Telegram");
        assert!(f.contains("Autorisé par toi à 14:32 via Telegram"));
    }

    #[test]
    fn action_kinds_are_unique() {
        let mut v = kind::ALL.to_vec();
        let n = v.len();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), n);
    }
}
