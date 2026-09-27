# Matrice des critères d'acceptation

Index **généré** à partir des sources : chaque test nommé `ca_<section>_<n>_<nom>`
couvre le critère d'acceptation correspondant du PRD (§20.2, point 2).

Pour le régénérer après avoir ajouté un test :

```bash
UPDATE_CA_MATRIX=1 cargo test -p penelope-evals --test ca_matrix
```

78 tests d'acceptation, 14 sections couvertes.

## §2. Plateformes, portabilité et exploitation headless

| CA | Test | Fichier |
|---|---|---|
| CA 2.2 | Penelope home reroots everything | `crates/penelope-platform/src/lib.rs` |
| CA 2.3 | No os specific code outside the platform crate | `crates/penelope-archtest/src/tests.rs` |
| CA 2.5 | Workspace write blocks outside writes | `crates/penelope-evals/tests/security.rs` |
| CA 2.8 | A broken upgrade is rolled back automatically | `crates/penelope-ops/src/upgrade/tests.rs` |

## §3. Architecture

| CA | Test | Fichier |
|---|---|---|
| CA 3.1 | Dependency rules hold | `crates/penelope-archtest/src/tests.rs` |
| CA 3.2 | Four sessions run concurrently | `crates/penelope-kernel/src/turn/tests.rs` |
| CA 3.3 | Expired lease is reclaimed | `crates/penelope-kernel/src/turn/tests.rs` |

## §4. Noyau : event log, ledger d'effets, générations

| CA | Test | Fichier |
|---|---|---|
| CA 4.1 | Detects tampering | `crates/penelope-kernel/src/event.rs` |
| CA 4.2 | Dispatching becomes unknown without retry | `crates/penelope-kernel/src/effects.rs` |
| CA 4.3 | Generations are monotonic | `crates/penelope-kernel/src/config/tests.rs` |
| CA 4.4 | Config changes are published live | `crates/penelope-evals/tests/hot_reload.rs` |
| CA 4.5 | Model visible is logged | `crates/penelope-evals/tests/scenarios.rs` |
| CA 4.6 | Reindex is lossless | `crates/penelope-evals/tests/scenarios.rs` |

## §5. Sessions et moteur de contexte

| CA | Test | Fichier |
|---|---|---|
| CA 5.1 | Level4 proves it fits | `crates/penelope-context/src/compaction/tests.rs` |
| CA 5.3 | Prefix is byte identical across turns | `crates/penelope-context/src/tiers.rs` |
| CA 5.4 | Each request extends the previous one | `crates/penelope-daemon/src/cache_audit.rs` |
| CA 5.5 | Replace only at a turn boundary | `crates/penelope-evals/tests/scenarios.rs` |

## §6. Mémoire, apprentissage continu et second brain

| CA | Test | Fichier |
|---|---|---|
| CA 6.1 | Defeasible rule recall | `crates/penelope-memory/src/vault/tests.rs` |
| CA 6.2 | Ecart promotion thresholds | `crates/penelope-memory/src/consolidation/tests.rs` |
| CA 6.3 | Correction scoping | `crates/penelope-memory/src/consolidation/tests.rs` |
| CA 6.4 | Contradiction without distinct context asks | `crates/penelope-memory/src/consolidation/tests.rs` |
| CA 6.6 | Recalled memory is never re extracted | `crates/penelope-memory/src/provenance.rs` |
| CA 6.7 | Background sessions produce nothing | `crates/penelope-memory/src/provenance.rs` |
| CA 6.8 | Manual edit defers the operation | `crates/penelope-memory/src/consolidation/tests.rs` |
| CA 6.9 | Reindex keeps provenance and signals | `crates/penelope-memory/src/index/tests.rs` |
| CA 6.10 | Intent fires respects cooldown and expires | `crates/penelope-memory/src/intents.rs` |
| CA 6.11 | Empty pass is a noop | `crates/penelope-memory/src/consolidation/tests.rs` |
| CA 6.12 | Recall never blocks | `crates/penelope-memory/src/recall/tests.rs` |
| CA 6.13 | Forbidden content is blocked in consolidation | `crates/penelope-memory/src/consolidation/tests.rs` |
| CA 6.14 | A profile write waits for the next episode | `crates/penelope-vault/src/episodes.rs` |
| CA 6.15 | Two idle hours close the episode and ingest it | `crates/penelope-vault/src/episodes.rs` |

## §7. Skills

| CA | Test | Fichier |
|---|---|---|
| CA 7.1 | New skill is available after reload | `crates/penelope-skills/src/lib.rs` |
| CA 7.2 | Invalid skills are rejected explicitly | `crates/penelope-skills/src/lib.rs` |
| CA 7.3 | Rejected proposal is never written | `crates/penelope-skills/src/lib.rs` |
| CA 7.4 | A dropped skill is available without restart | `crates/penelope-evals/tests/hot_reload.rs` |

## §8. MCP : client complet

| CA | Test | Fichier |
|---|---|---|
| CA 8.1 | Every version and transport negotiates | `crates/penelope-evals/tests/mcp_conformance.rs` |
| CA 8.2 | Mixed versions behind the same url | `crates/penelope-evals/tests/mcp_conformance.rs` |
| CA 8.3 | Incremental consent and registration | `crates/penelope-evals/tests/mcp_conformance.rs` |
| CA 8.3 | Invalid issuer is rejected | `crates/penelope-evals/tests/mcp_conformance.rs` |
| CA 8.3 | Oauth discovery and pkce | `crates/penelope-evals/src/ca_matrix.rs` |
| CA 8.3 | Oauth discovery and pkce | `crates/penelope-evals/tests/mcp_conformance.rs` |
| CA 8.3 | Paste back flow over telegram | `crates/penelope-evals/tests/mcp_conformance.rs` |
| CA 8.6 | Tasks survive a restart | `crates/penelope-mcp/src/tasks.rs` |
| CA 8.7 | Adding an mcp server is hot | `crates/penelope-evals/tests/hot_reload.rs` |

## §9. HITL (Human in the Loop)

| CA | Test | Fichier |
|---|---|---|
| CA 9.1 | First decision wins | `crates/penelope-hitl/src/tests.rs` |
| CA 9.2 | Expiry blocks then can resume | `crates/penelope-hitl/src/tests.rs` |
| CA 9.3 | Always rule applies then is revocable | `crates/penelope-hitl/src/policy/tests.rs` |
| CA 9.4 | An approved tool runs once after resume and the ledger shows it | `crates/penelope-daemon/tests/approval_e2e.rs` |
| CA 9.5 | A denied tool is reported to the model and never runs | `crates/penelope-daemon/tests/approval_e2e.rs` |
| CA 9.6 | A session window lets the same call pass without a new request | `crates/penelope-daemon/tests/approval_e2e.rs` |

## §10. LLM : providers et routage

| CA | Test | Fichier |
|---|---|---|
| CA 10.1 | Sticky model survives until a boundary | `crates/penelope-llm/src/router/tests.rs` |
| CA 10.2 | High complexity routes to reasoning | `crates/penelope-llm/src/router/tests.rs` |
| CA 10.3 | Fallback chain is used on transient failure | `crates/penelope-llm/src/router/tests.rs` |
| CA 10.4 | Daily budget exceeded is detected | `crates/penelope-kernel/src/budget/tests.rs` |

## §12. Workflows, triggers et jobs

| CA | Test | Fichier |
|---|---|---|
| CA 12.1 | Ticket to deploy runs end to end and survives restarts | `crates/penelope-gateway-telegram/src/ticket_to_deploy_e2e.rs` |
| CA 12.2 | Runs are recovered at their current step | `crates/penelope-workflow/src/runs/tests.rs` |
| CA 12.3 | Invalid file is rejected and previous stays | `crates/penelope-workflow/src/registry.rs` |
| CA 12.4 | Poll fires once per item | `crates/penelope-workflow/src/schedules/tests.rs` |
| CA 12.5 | Workflows reload and reject without losing the previous version | `crates/penelope-evals/tests/hot_reload.rs` |

## §13. Sécurité

| CA | Test | Fichier |
|---|---|---|
| CA 13.1 | Injected instructions never act on their own | `crates/penelope-evals/tests/security.rs` |
| CA 13.2 | Planted secret never leaks | `crates/penelope-observe/src/redact.rs` |
| CA 13.2 | Secrets never leak | `crates/penelope-evals/tests/security.rs` |

## §14. Telegram : implémentation complète

| CA | Test | Fichier |
|---|---|---|
| CA 14.1 | Every template renders in both forms | `crates/penelope-telegram/src/templates/tests.rs` |
| CA 14.2 | A telegram message gets an answer and a ledger entry | `crates/penelope-gateway-telegram/tests/telegram_e2e.rs` |
| CA 14.3 | Rate limit is respected without loss | `crates/penelope-telegram/src/api/tests.rs` |
| CA 14.4 | Double click is idempotent | `crates/penelope-telegram/src/actions.rs` |
| CA 14.5 | Non owner clicks are refused | `crates/penelope-telegram/src/actions.rs` |
| CA 14.5 | Unauthorized users are rejected | `crates/penelope-telegram/src/lib.rs` |
| CA 14.6 | Splitting never breaks a code block | `crates/penelope-telegram/src/render.rs` |

## §15. CLI et SSH

| CA | Test | Fichier |
|---|---|---|
| CA 15.1 | Every command maps to an rpc method | `crates/penelope-telegram/src/commands.rs` |

## §17. Résilience

| CA | Test | Fichier |
|---|---|---|
| CA 17.1 | A turn interrupted mid flight is requeued once | `crates/penelope-evals/tests/resilience.rs` |
| CA 17.1 | A turn is requeued | `crates/penelope-evals/src/ca_matrix.rs` |
| CA 17.2 | A completed effect is replayed not reexecuted | `crates/penelope-evals/tests/resilience.rs` |
| CA 17.3 | An uncertain effect asks instead of retrying | `crates/penelope-evals/tests/resilience.rs` |
| CA 17.4 | Llm calls are classified by where the crash happened | `crates/penelope-evals/tests/resilience.rs` |
| CA 17.5 | A run resumes at its current step | `crates/penelope-evals/tests/resilience.rs` |
| CA 17.6 | The event chain survives and detects tampering | `crates/penelope-evals/tests/resilience.rs` |
| CA 17.7 | A frozen writer does not duplicate a turn in flight | `crates/penelope-evals/tests/resilience.rs` |

