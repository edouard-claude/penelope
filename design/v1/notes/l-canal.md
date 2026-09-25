# Lot L : le cœur ne nomme plus le canal (épopée #208, T36)

Agent `l-canal`, branche `v1-l-canal`, base 1.0.0-alpha.12 (`3359b0d`).
Spécification : `design/v1/decoupage-daemon.md` §1.3 et §6 T36 ; charte
`design/v1/README.md` §3.5 (règle R8 du découpage). Préalable : T29 (passerelle au-dessus
du daemon).

## 0. Inventaire avant changement

Mesure : motif `CHANNEL_PATTERN` d'archtest sur les lignes de code (hors commentaires,
hors module de tests), crates du cœur touchées par T36 : `penelope-app`,
`penelope-conversation`, `penelope-orchestrator`, `penelope-executor`, `penelope-agent`,
`penelope-daemon`. **Baseline `[channel.allowed]` de ces six crates : 166** (sur 341 pour
toutes les crates agnostiques ; le reste est `kernel`, `store`, `ops`, `observe`, `tools`,
`vault`, `mcp-host`, hors T36).

| Fichier | Mentions | Nature | Famille T36 |
|---|---|---|---|
| `orchestrator/scheduler/origin.rs` | 30 | `place_name` (noms de chats lus en kv), `retarget` (`allowed_chats`, `Origin::Telegram`), `destination`, `target_origin` | origines |
| `daemon/supervisor.rs` | 13 | origine d'une session liée (`tg_chat_id`, T37), journal « file Telegram », `actions.purge_expired` (sans mention) | cartes, T37 |
| `executor/selfknow.rs` | 12 | commandes du canal (`penelope_telegram::commands`) ×2, résumé `[telegram]` de la configuration ×5, chemins `owner.telegram_user_id` et `telegram.` | selfknow via `Admin` |
| `app/helpers.rs` | 12 | `owner_origin_of` ×4 (T37), `deep_link` ×1, clés kv `tg.topic_name`, `tg.chat_title`, `telegram.seen_chats` | cartes (lien) |
| `daemon/runner.rs` | 11 | fusion des rafales (`Origin::Telegram`, `cfg.telegram.burst_*`), « canal Telegram indisponible », filtre `Origin::Telegram` de `deliver` | rafales, chaînes |
| `daemon/engine.rs` | 10 | `chat_session_for` (`find_by_topic`, `bind_telegram`, T37), `hooks.telegram()` | T37 |
| `app/elicitation.rs` | 10 | `Destination { chat_id, topic_id }`, `fields_from_schema`, « sur Telegram » ×5, « Telegram non configuré » | élicitation, chaînes |
| `daemon/rpc/methods/workflows.rs` | 9 | `schedule move` (`chat_id`, `topic_id`) | origines |
| `daemon/rpc/methods/approvals.rs` | 9 | `telegram.quiet_hours`, origine de session (T37) | hors T36 |
| `app/bus.rs` | 9 | `Origin::Telegram`, `telegram_chat()` | permanent jusqu'à T37 |
| `executor/.../schedules_messaging.rs` | 7 | `schedule_move` : `private` / `here` en `(chat_id, topic_id)` | origines |
| `daemon/tool_jobs.rs` | 6 | origine de session (T37) ; fichier du lot `k-jobs` | hors périmètre |
| `conversation/lib.rs` | 6 | règle de rafale de `TurnInbox` | rafales, chaînes |
| `executor/executor/mod.rs` | 5 | `elicitation_destination` (`telegram_chat()`) | élicitation |
| `daemon/runtime.rs` | 5 | `Hooks::telegram()`, `StatusReport.telegram` (T37), `tg_outbox` en SQL | cartes (hooks) |
| `app/services.rs` | 5 | `TemplateRegistry`, `ActionStore`, `templates::CATALOG`, `sample.telegram`, sous-système `telegram` | cartes |
| `orchestrator/scheduler/fire.rs`, `outcome.rs` | 1 + 1 | « Telegram non configuré », `Origin::Telegram` | chaînes, origines |
| `executor/voice.rs`, `skills_workflows.rs` | 1 + 1 | « format Telegram » (doctor), `Origin::Telegram` | chaînes |
| `app/media.rs`, `codex_scope.rs` | 1 + 1 | dossier `telegram/` du workspace, `Origin::Telegram` | chaînes |
| `agent/pipeline.rs` | 1 | `EffectKind::Telegram` (T37 ; fichier du lot `k-attempts`) | hors périmètre |

Dépendance `penelope-app → penelope-telegram` : quatre sites (`services.rs:21, 308`,
`helpers.rs:137`, `elicitation.rs:488`).
