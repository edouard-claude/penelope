//! Le routage d'une commande vers sa méthode RPC (CA 15 : parité Telegram et CLI), et
//! la lecture des valeurs de configuration qu'elle transporte.

use super::*;

/// Associe une commande à sa méthode RPC (CA 15 : parité Telegram ↔ CLI).
pub fn route(cmd: &Command) -> CliResult<(&'static str, Value)> {
    Ok(match cmd {
        Command::Session(c) => session_route(c),
        Command::Mcp(c) => mcp_route(c)?,
        Command::Schedule(c) => schedule_route(c)?,
        Command::Mem(c) => mem_route(c),
        Command::Status => (m::STATUS, json!({})),
        Command::Metrics => (m::METRICS, json!({})),
        Command::Doctor => (m::DOCTOR, json!({})),
        Command::Restart => (m::RESTART, json!({})),
        Command::Import(ImportCmd::Hermes {
            path,
            dry_run,
            no_test,
        }) => (
            m::IMPORT_HERMES,
            json!({
                "path": path.as_ref().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())),
                "apply": !dry_run,
                "test": !no_test,
            }),
        ),
        Command::Upgrade {
            check,
            rollback,
            tag,
            force,
            switch,
        } => (
            m::UPGRADE,
            json!({"check": check, "rollback": rollback, "tag": tag, "force": force, "switch": switch}),
        ),

        Command::Config(ConfigCmd::Get) => (m::CONFIG_GET, json!({})),
        Command::Config(ConfigCmd::Status) => (m::CONFIG_STATUS, json!({})),
        Command::Config(ConfigCmd::Reload) => (m::CONFIG_RELOAD, json!({})),
        Command::Config(ConfigCmd::Set { path, value }) => (
            m::CONFIG_SET,
            json!({"path": path, "value": parse_scalar(value)}),
        ),

        Command::Secret(SecretCmd::List) => (m::SECRET_LIST, json!({})),
        Command::Secret(SecretCmd::Backend) => (m::SECRET_BACKEND, json!({})),
        Command::Secret(SecretCmd::Rm { name }) => (m::SECRET_RM, json!({"name": name})),

        Command::Model(ModelCmd::List { filter }) => (m::MODEL_LIST, json!({"filter": filter})),
        Command::Model(ModelCmd::Set { alias, model }) => {
            (m::MODEL_SET, json!({"alias": alias, "model": model}))
        }
        // `model auth` sans option passe par `model_auth` (code affiché, puis attente) ;
        // cette route sert la parité CLI↔RPC et le mode `--json`.
        Command::Model(ModelCmd::Auth {
            provider,
            logout,
            status,
        }) => (
            m::MODEL_AUTH,
            json!({
                "provider": provider,
                "action": if *logout { "logout" } else if *status { "status" } else { "start" },
            }),
        ),

        Command::Wf(WfCmd::List) => (m::WF_LIST, json!({})),
        Command::Wf(WfCmd::Show { id }) => (m::WF_SHOW, json!({"id": id})),
        Command::Wf(WfCmd::Runs) => (m::WF_RUNS, json!({})),
        Command::Wf(WfCmd::Trace { run }) => (m::WF_TRACE, json!({"run": run})),
        Command::Wf(WfCmd::Run { id, params }) => (
            m::WF_RUN,
            json!({"id": id, "params": penelope_gateway_telegram::parse_params(&params.join(" "))}),
        ),
        Command::Wf(WfCmd::Control {
            run,
            op,
            choice,
            input,
            usd,
            tokens,
        }) => (
            m::WF_CONTROL,
            json!({"run": run, "op": op, "choice": choice, "input": input,
                   "usd": usd, "tokens": tokens}),
        ),

        Command::Vault(VaultCmd::Sync) => (m::VAULT_SYNC, json!({})),
        Command::Vault(VaultCmd::Check) => (m::VAULT_CHECK, json!({})),
        Command::Vault(VaultCmd::Lint) => (m::VAULT_LINT, json!({})),

        Command::Skill(SkillCmd::List) => (m::SKILL_LIST, json!({})),
        Command::Skill(SkillCmd::Show { name }) => (m::SKILL_SHOW, json!({"name": name})),
        Command::Skill(SkillCmd::Rollback { name }) => (m::SKILL_ROLLBACK, json!({"name": name})),
        Command::Skill(SkillCmd::Reload) => (m::SKILL_RELOAD, json!({})),
        Command::Skill(SkillCmd::Install { source, force }) => {
            (m::SKILL_INSTALL, json!({"source": source, "force": force}))
        }

        Command::Jobs { all } => (m::JOBS, json!({"all": all})),
        Command::Approvals { .. } => (m::APPROVALS, json!({})),
        Command::Approve { id, always, effect } => (
            m::APPROVE,
            json!({"id": id, "always": always, "effect": effect}),
        ),
        Command::Deny { id, reason } => (m::DENY, json!({"id": id, "reason": reason})),
        Command::Policies => (m::POLICIES, json!({})),

        Command::Usage {
            by,
            session,
            since,
            limit,
        } => (
            m::USAGE,
            json!({"by": by, "session": session, "since": since, "limit": limit}),
        ),
        Command::AuditVerify => (m::AUDIT_VERIFY, json!({})),
        Command::History(HistoryCmd::Verify { session }) => {
            (m::HISTORY_VERIFY, json!({"session": session}))
        }
        Command::History(HistoryCmd::Reindex { session }) => {
            (m::HISTORY_REINDEX, json!({"session": session}))
        }
        Command::Audit(AuditCmd::Show { turn, session }) => {
            if turn.is_none() && session.is_none() {
                return Err(CliError::Usage(
                    "préciser `--turn <id>` ou `--session <id>`".into(),
                ));
            }
            (m::AUDIT_SHOW, json!({"turn": turn, "session": session}))
        }
        Command::Backup { push, full, media } => (
            m::BACKUP,
            json!({"push": push, "full": *full || *push, "media": media}),
        ),
        Command::Export { what, id } => (m::EXPORT, json!({"what": what, "id": id})),
        Command::Store(StoreCmd::Rebuild) => (m::STORE_REBUILD, json!({})),
        other => {
            return Err(CliError::Usage(format!("commande non routée : {other:?}")));
        }
    })
}

/// Sessions : ouvrir, nommer, borner, forker, rembobiner.
fn session_route(cmd: &SessionCmd) -> (&'static str, Value) {
    match cmd {
        SessionCmd::List => (m::SESSION_LIST, json!({})),
        SessionCmd::New { title } => (m::SESSION_NEW, json!({"title": title})),
        SessionCmd::Close { session } => (m::SESSION_CLOSE, json!({"session": session})),
        SessionCmd::Purge {
            session, reason, ..
        } => (
            m::SESSION_PURGE,
            json!({"session": session, "reason": reason}),
        ),
        SessionCmd::Title { session, title } => (
            m::SESSION_TITLE,
            json!({"session": session, "title": title.join(" ")}),
        ),
        SessionCmd::Budget { session, usd } => (
            m::SESSION_BUDGET,
            json!({
                "session": session,
                "usd": match usd.as_deref() {
                    None => Value::Null,
                    Some("off") => json!(0),
                    Some(x) => json!(x),
                },
            }),
        ),
        SessionCmd::Model { alias, session } => (
            m::SESSION_MODEL,
            json!({"alias": alias, "session": session}),
        ),
        SessionCmd::Export { session } => (m::SESSION_EXPORT, json!({"session": session})),
        SessionCmd::Compact { session } => (m::SESSION_COMPACT, json!({"session": session})),
        SessionCmd::Mode { mode, session } => {
            (m::SESSION_MODE, json!({"mode": mode, "session": session}))
        }
        SessionCmd::Project { project, session } => (
            m::SESSION_PROJECT,
            json!({"project": project, "session": session}),
        ),
        SessionCmd::Fork { session, title } => {
            (m::SESSION_FORK, json!({"session": session, "title": title}))
        }
        SessionCmd::Rewind { turns, session } => (
            m::SESSION_REWIND,
            json!({"session": session, "turns": turns}),
        ),
    }
}

/// Serveurs MCP : administration et diagnostic.
fn mcp_route(cmd: &McpCmd) -> CliResult<(&'static str, Value)> {
    Ok(match cmd {
        McpCmd::List => (m::MCP_LIST, json!({})),
        McpCmd::Show { name } => (m::MCP_SHOW, json!({"name": name})),
        McpCmd::Auth { name, callback } => {
            (m::MCP_AUTH, json!({"name": name, "callback": callback}))
        }
        McpCmd::Add { file, name } => (m::MCP_ADD, json!({"toml": read_toml(file)?, "name": name})),
        McpCmd::Edit { name, field, value } => (
            m::MCP_EDIT,
            json!({"name": name, "patch": {field.clone(): parse_scalar(value)}}),
        ),
        McpCmd::Rm { name } => (m::MCP_RM, json!({"name": name})),
        McpCmd::Enable { name } => (m::MCP_ENABLE, json!({"name": name})),
        McpCmd::Disable { name } => (m::MCP_DISABLE, json!({"name": name})),
        McpCmd::Restart { name } => (m::MCP_RESTART, json!({"name": name})),
        McpCmd::Test { name, file } => match file {
            Some(f) => (m::MCP_TEST, json!({"toml": read_toml(f)?, "name": name})),
            None => (m::MCP_TEST, json!({"name": name})),
        },
        McpCmd::Logs { name, lines } => (m::MCP_LOGS, json!({"name": name, "lines": lines})),
    })
}

/// Déclencheurs planifiés ; `move` et `add` valident leurs arguments.
fn schedule_route(cmd: &ScheduleCmd) -> CliResult<(&'static str, Value)> {
    Ok(match cmd {
        ScheduleCmd::List => (m::SCHEDULE_LIST, json!({})),
        ScheduleCmd::Pause { id } => (m::SCHEDULE_PAUSE, json!({"id": id})),
        ScheduleCmd::Resume { id } => (m::SCHEDULE_RESUME, json!({"id": id})),
        ScheduleCmd::Rm { id } => (m::SCHEDULE_RM, json!({"id": id})),
        ScheduleCmd::Run { id } => (m::SCHEDULE_RUN_NOW, json!({"id": id})),
        ScheduleCmd::Move {
            id,
            chat,
            topic,
            private,
        } => {
            if !private && chat.is_none() {
                return Err(CliError::Usage(
                    "où l'envoyer : `--private`, ou `--chat <id>` (et `--topic <id>`)".into(),
                ));
            }
            (
                m::SCHEDULE_MOVE,
                json!({"id": id, "private": private, "chat_id": chat, "topic_id": topic}),
            )
        }
        ScheduleCmd::Add {
            kind,
            spec,
            target,
            dedup,
        } => {
            let json_arg = |name: &str, raw: &str| {
                serde_json::from_str::<Value>(raw)
                    .map_err(|e| CliError::Usage(format!("--{name} n'est pas du JSON : {e}")))
            };
            (
                m::SCHEDULE_ADD,
                json!({
                    "kind": kind,
                    "spec": json_arg("spec", spec)?,
                    "target": json_arg("target", target)?,
                    "dedup": match dedup {
                        Some(d) => json_arg("dedup", d)?,
                        None => json!({}),
                    },
                }),
            )
        }
    })
}

/// Mémoire durable.
fn mem_route(cmd: &MemCmd) -> (&'static str, Value) {
    match cmd {
        MemCmd::Search { query } => (m::MEM_SEARCH, json!({"query": query})),
        MemCmd::Show { uid } => (m::MEM_SHOW, json!({"uid": uid})),
        MemCmd::History { uid, file } => (m::MEM_HISTORY, json!({"uid": uid, "file": file})),
        MemCmd::Restore { id } => (m::MEM_RESTORE, json!({"id": id})),
        MemCmd::Reindex { embeddings } => (m::MEM_REINDEX, json!({"embeddings": embeddings})),
        MemCmd::Forget { uid } => (m::MEM_FORGET, json!({"uid": uid})),
        MemCmd::Candidates => (m::MEM_CANDIDATES, json!({})),
        MemCmd::Split { uid } => (m::MEM_SPLIT, json!({"uid": uid})),
        MemCmd::Audit => (m::MEM_AUDIT, json!({})),
        MemCmd::RetryRejected => (m::MEM_RETRY_REJECTED, json!({})),
        MemCmd::Diff { since } => (m::MEM_DIFF, json!({"since": since})),
        MemCmd::Dream { dry_run } => (m::MEM_DREAM, json!({"dry_run": dry_run})),
        MemCmd::Learned { days } => (m::MEM_LEARNED, json!({"days": days})),
        MemCmd::Signals { uid } => (m::MEM_SIGNALS, json!({"uid": uid})),
    }
}

/// Contenu d'un fichier de déclaration MCP.
fn read_toml(path: &std::path::Path) -> CliResult<String> {
    std::fs::read_to_string(path).map_err(|e| CliError::Io(format!("{} : {e}", path.display())))
}

/// `"12"` devient un nombre, `"true"` un booléen, le reste une chaîne.
pub(super) fn parse_scalar(raw: &str) -> Value {
    if let Ok(b) = raw.parse::<bool>() {
        return Value::Bool(b);
    }
    if let Ok(i) = raw.parse::<i64>() {
        return json!(i);
    }
    if let Ok(f) = raw.parse::<f64>() {
        return json!(f);
    }
    if ((raw.starts_with('{') && raw.ends_with('}'))
        || (raw.starts_with('[') && raw.ends_with(']')))
        && let Ok(v) = serde_json::from_str::<Value>(raw)
    {
        return v;
    }
    Value::String(raw.to_string())
}
