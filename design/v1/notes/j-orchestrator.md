# Lot J : crate `penelope-orchestrator` (épopée #208, T27 et T11)

Agent `j-orchestrator`, branche `v1-j-orchestrator`, base 1.0.0-alpha.11 (`8f701ff`).
Spécification : `design/v1/decoupage-daemon.md` §3.2, §6 T27 et sa ligne de dépendances ;
`design/v1/boucle-et-outils.md` §5 T11. Préalables : T10 (`j-agent-crate.md`), T23
(`j-conversation.md`), T24 (`j-executor.md`), T26 (`j-dream-fin.md`).

## 0. Inventaire avant déplacement

### Ce qui part

| Module du daemon | Lignes | Ce qu'il lit du daemon |
|---|---|---|
| `workflow/` (10 fichiers hors tests) | 2 787 | `d.services` ; `d.workflows` (état, ports) ; `d.provider_for` ; `d.embedder()` ; `d.providers` (images, vision) ; `d.handle.is_shutting_down` ; `d.clone() as Arc<dyn Admin>` (exécuteur d'étape) ; `agent::services_of` (ports de la boucle sur les modules du daemon) |
| `scheduler.rs` + `scheduler/templating.rs` | 1 095 + 63 | `d.services` ; `d.handle` ; `d.bus.notify_enqueued` ; `d.dream()` ; `dream::DigestFeed(d)` |
| `dream/mod.rs` : `digest_inputs`, `DigestFeed` | 40 | `scheduler::label`, `scheduler::due_today`, `compaction::struggling_sessions` : que `Services` |

Chemins `crate::` à réécrire (seulement des `use`) : `agent` vers `penelope_agent`,
`executor`, `images`, `vision` vers `penelope_executor`, `conversation` vers
`penelope_conversation`, `embeddings` vers `penelope_vault`, `bus`, `ports`, `helpers`,
`codex_scope`, `runtime::Services`, `selfknow::Admin`, `testing` vers `penelope_app`.

### Coupure retenue (commit de signature, avant le déplacement)

- Un `Context` porte ce que workflow et ordonnanceur lisaient du daemon : `services`,
  `providers` (`Arc<dyn ProviderSource>`), `handle`, `bus`, `workflows`
  (`Arc<workflow::State>`), `embeddings`, `agent` (`Arc<AgentServices>`, que le daemon
  construit par `agent::services_of` : les étapes et les sous-agents appellent
  `penelope_agent::AgentLoop` directement, T11), `admin` (`Option<Arc<dyn Admin>>`).
- `Daemon.workflows` devient `Arc<workflow::State>`.
- La livraison vers le canal reste le `Slot<dyn ChannelDelivery>` de `scheduler::Ports`
  (port de `penelope-app`, lu au moment de s'en servir : la passerelle se branche après
  le daemon) ; `Slot::get` rend l'`Option<Arc<dyn ChannelDelivery>>` demandée.
- Ce qui ne lit que `Services` le prend directement : `origin_of`, `label`, `due_today`,
  `final_already_sent`, `digest_inputs`, `DigestFeed(Arc<Services>)`.

### Consommateurs à garder valides

Daemon : `supervisor.rs` (boucles, orchestrateur), `runner.rs` (`trigger_outcome_of`,
`final_already_sent`), `rpc/methods/workflows.rs`, `engine.rs` (`workflows.wake`),
`tool_jobs.rs` et deux tests (`WorkflowOrchestrator { daemon }`), `dream/mod.rs`.
Passerelle : `workflow::{answer, form_of, control, start_run, origin_of}`,
`scheduler::retarget`, et dans ses tests `drive`, `run_now`, `start_run_briefed`,
`brief_of`, `WorkflowOrchestrator`. Aucun n'est dans le périmètre : la façade du daemon
garde les anciennes signatures en `&Arc<Daemon>` jusqu'à T30.

### T11

- Étapes `agent` et sous-agents : `AgentLoop::new(ctx.agent.clone(), provider)` au lieu
  de `crate::agent::services_of(s)`.
- `skill_install` (`penelope-ops`) : plus aucun appel à la boucle depuis T28 (la ligne
  137 citée par la spécification rechargeait les skills, aujourd'hui
  `penelope_app::services::reload_skills`). Rien à faire.
- Le test « le sous-agent transforme `AwaitingApproval` en erreur » annoncé comme
  existant n'existe pas : il est écrit dans la crate.
