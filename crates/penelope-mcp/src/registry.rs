//! Nommage des outils (§8.8) et **registre paresseux** (§8.9).
//!
//! Au-delà de 20 outils MCP, les schémas ne sont jamais injectés dans le prompt : seuls
//! trois méta-outils sont exposés en permanence (`tool_search`, `tool_describe`,
//! `tool_call`). Un outil décrit ou appelé devient « collant » et rejoint la liste native
//! à la **frontière de compaction suivante**, pour ne pas casser le cache en cours.

use crate::protocol::ToolDescriptor;
use penelope_kernel::canonical::sha256_hex;
use penelope_kernel::risk::{RiskClass, classify_annotations};
use penelope_store::{Store, rusqlite::params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

/// Nom exposé au modèle : `mcp__<serveur>__<outil>`, normalisé `[a-z0-9_]`, tronqué à
/// 64 caractères avec suffixe de hash en cas de collision (§8.8).
pub fn qualified_name(server: &str, tool: &str) -> String {
    let raw = format!("mcp__{}__{}", normalise(server), normalise(tool));
    if raw.len() <= 64 {
        return raw;
    }
    let suffix = &sha256_hex(raw.as_bytes())[..8];
    let keep = 64 - 1 - suffix.len();
    let mut truncated: String = raw.chars().take(keep).collect();
    truncated.push('_');
    truncated.push_str(suffix);
    truncated
}

fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_us = false;
    for c in s.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_us = false;
        } else if !last_us {
            out.push('_');
            last_us = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// Outil indexé.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisteredTool {
    pub qualified: String,
    pub server: String,
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: Value,
    pub risk: RiskClass,
    pub schema_bytes: usize,
}

impl RegisteredTool {
    pub fn from_descriptor(server: &str, d: &ToolDescriptor) -> RegisteredTool {
        RegisteredTool {
            qualified: qualified_name(server, &d.name),
            server: server.to_string(),
            name: d.name.clone(),
            title: d.title.clone(),
            description: d.description.clone(),
            schema_bytes: penelope_kernel::schema::schema_bytes(&d.input_schema),
            input_schema: d.input_schema.clone(),
            output_schema: d.output_schema.clone(),
            risk: classify_annotations(&d.annotations),
            annotations: d.annotations.clone(),
        }
    }

    /// Résumé court renvoyé par `tool_search`.
    pub fn short(&self) -> Value {
        json!({
            "name": self.qualified,
            "server": self.server,
            "title": self.title.clone().unwrap_or_else(|| self.name.clone()),
            "description": truncate(&self.description, 200),
            "risk": self.risk.as_str(),
        })
    }

    /// Schéma complet, ou résumé si le plafond par schéma est dépassé (§8.9).
    pub fn describe(&self, max_bytes: usize) -> Value {
        let schema = if self.schema_bytes > max_bytes {
            summarise_schema(&self.input_schema)
        } else {
            self.input_schema.clone()
        };
        json!({
            "name": self.qualified,
            "server": self.server,
            "description": self.description,
            "inputSchema": schema,
            "outputSchema": self.output_schema,
            "annotations": self.annotations,
            "risk": self.risk.as_str(),
            "schemaTruncated": self.schema_bytes > max_bytes,
        })
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

/// Résumé d'un schéma trop volumineux : noms, types et champs requis seulement.
pub fn summarise_schema(schema: &Value) -> Value {
    let required: Vec<String> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let props = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|o| {
            o.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        json!({
                            "type": v.get("type").cloned().unwrap_or(Value::Null),
                            "description": v.get("description")
                                .and_then(|d| d.as_str())
                                .map(|d| truncate(d, 120)),
                        }),
                    )
                })
                .collect::<serde_json::Map<_, _>>()
        })
        .unwrap_or_default();
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "x-penelope-note": "schéma résumé (plafond dépassé) — schéma complet via tool_describe",
    })
}

/// Résultat de recherche hybride.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub tool: RegisteredTool,
    pub score: f32,
}

/// Registre paresseux.
#[derive(Clone)]
pub struct ToolRegistry {
    store: Store,
    /// Ensemble collant : outils promus en liste native (borné, §8.9).
    sticky: Arc<RwLock<BTreeSet<String>>>,
    /// Outils décrits ou appelés depuis la dernière frontière de compaction.
    pending_promotion: Arc<RwLock<BTreeSet<String>>>,
    sticky_max: usize,
    schema_max_bytes: usize,
    eager_total_max: usize,
    generation: Arc<std::sync::atomic::AtomicU64>,
}

impl ToolRegistry {
    pub fn new(
        store: Store,
        sticky_max: usize,
        schema_max_bytes: usize,
        eager_total_max: usize,
    ) -> Self {
        ToolRegistry {
            store,
            sticky: Arc::new(RwLock::new(BTreeSet::new())),
            pending_promotion: Arc::new(RwLock::new(BTreeSet::new())),
            sticky_max,
            schema_max_bytes,
            eager_total_max,
            generation: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Remplace les outils d'un serveur et publie une nouvelle génération du registre.
    pub async fn replace_server_tools(
        &self,
        server: &str,
        tools: Vec<RegisteredTool>,
        now: &str,
    ) -> penelope_store::Result<u64> {
        let srv = server.to_string();
        let ts = now.to_string();
        let generation = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;

        self.store
            .write(move |tx| {
                tx.execute("DELETE FROM mcp_tools WHERE server = ?1", [&srv])?;
                tx.execute("DELETE FROM mcp_tools_fts WHERE server = ?1", [&srv])?;
                for t in &tools {
                    tx.execute(
                        "INSERT OR REPLACE INTO mcp_tools(qualified, server, name, title,
                            description, input_schema, output_schema, annotations, risk,
                            schema_bytes, generation, updated_at)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                        params![
                            t.qualified,
                            t.server,
                            t.name,
                            t.title,
                            t.description,
                            t.input_schema.to_string(),
                            t.output_schema.as_ref().map(|s| s.to_string()),
                            t.annotations.to_string(),
                            t.risk.as_str(),
                            t.schema_bytes as i64,
                            generation as i64,
                            ts
                        ],
                    )?;
                    tx.execute(
                        "INSERT INTO mcp_tools_fts(name, title, description, server, qualified)
                         VALUES(?1,?2,?3,?4,?5)",
                        params![
                            t.name,
                            t.title.clone().unwrap_or_default(),
                            t.description,
                            t.server,
                            t.qualified
                        ],
                    )?;
                }
                tx.execute(
                    "UPDATE mcp_servers SET tool_count = ?2, updated_at = ?3 WHERE name = ?1",
                    params![srv, tools.len() as i64, ts],
                )?;
                Ok(())
            })
            .await?;
        Ok(generation)
    }

    /// `tool_search` : recherche hybride (FTS + similarité lexicale de repli).
    pub async fn search(
        &self,
        query: &str,
        server: Option<&str>,
        limit: usize,
    ) -> penelope_store::Result<Vec<SearchHit>> {
        let q = crate::registry::fts_query(query);
        let srv = server.map(String::from);
        let needle = query.to_lowercase();
        let lim = limit.max(1) as i64;

        self.store
            .read(move |c| {
                let mut hits: Vec<SearchHit> = Vec::new();
                if !q.is_empty() {
                    let mut st = c.prepare(
                        "SELECT t.qualified, t.server, t.name, t.title, t.description,
                                t.input_schema, t.output_schema, t.annotations, t.risk,
                                t.schema_bytes, rank
                         FROM mcp_tools_fts f
                         JOIN mcp_tools t ON t.qualified = f.qualified
                         WHERE mcp_tools_fts MATCH ?1 AND (?2 IS NULL OR t.server = ?2)
                         ORDER BY rank LIMIT ?3",
                    )?;
                    let rows = st.query_map(params![q, srv, lim], |r| {
                        let rank: f64 = r.get(10)?;
                        Ok((row_to_tool(r)?, (-rank) as f32))
                    })?;
                    for row in rows {
                        let (tool, score) = row?;
                        hits.push(SearchHit { tool, score });
                    }
                }

                // Repli lexical : garantit un résultat même si FTS ne matche pas
                // (requête d'un seul caractère, terme partiel).
                if hits.len() < limit {
                    let mut st = c.prepare(
                        "SELECT qualified, server, name, title, description, input_schema,
                                output_schema, annotations, risk, schema_bytes
                         FROM mcp_tools WHERE (?1 IS NULL OR server = ?1) LIMIT 2000",
                    )?;
                    let rows = st.query_map(params![srv], row_to_tool)?;
                    for r in rows {
                        let t = r?;
                        if hits.iter().any(|h| h.tool.qualified == t.qualified) {
                            continue;
                        }
                        let score = lexical_score(&needle, &t);
                        if score > 0.0 {
                            hits.push(SearchHit { tool: t, score });
                        }
                    }
                }

                hits.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.tool.qualified.cmp(&b.tool.qualified))
                });
                hits.truncate(limit);
                Ok(hits)
            })
            .await
    }

    /// `tool_describe` : schémas complets. Marque les outils pour promotion.
    pub async fn describe(&self, names: &[String]) -> penelope_store::Result<Vec<Value>> {
        let wanted: Vec<String> = names.to_vec();
        self.mark_for_promotion(names);
        let max = self.schema_max_bytes;
        self.store
            .read(move |c| {
                let mut out = Vec::new();
                for n in &wanted {
                    let mut st = c.prepare(
                        "SELECT qualified, server, name, title, description, input_schema,
                                output_schema, annotations, risk, schema_bytes
                         FROM mcp_tools WHERE qualified = ?1",
                    )?;
                    let mut rows = st.query([n])?;
                    if let Some(r) = rows.next()? {
                        out.push(row_to_tool(r)?.describe(max));
                    }
                }
                Ok(out)
            })
            .await
    }

    pub async fn get(&self, qualified: &str) -> penelope_store::Result<Option<RegisteredTool>> {
        let q = qualified.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT qualified, server, name, title, description, input_schema,
                            output_schema, annotations, risk, schema_bytes
                     FROM mcp_tools WHERE qualified = ?1",
                )?;
                let mut rows = st.query([&q])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_tool(r)?)),
                    None => Ok(None),
                }
            })
            .await
    }

    pub async fn count(&self) -> penelope_store::Result<i64> {
        self.store
            .read(|c| Ok(c.query_row("SELECT count(*) FROM mcp_tools", [], |r| r.get(0))?))
            .await
    }

    /// Enregistre un outil comme utilisé : il sera promu à la frontière suivante.
    pub fn mark_for_promotion(&self, names: &[String]) {
        if let Ok(mut g) = self.pending_promotion.write() {
            for n in names {
                g.insert(n.clone());
            }
        }
    }

    /// Applique les promotions en attente. À n'appeler **qu'à une frontière de
    /// compaction**, jamais en cours de session (§8.9).
    pub fn apply_promotions(&self) -> Vec<String> {
        let pending: Vec<String> = match self.pending_promotion.write() {
            Ok(mut g) => std::mem::take(&mut *g).into_iter().collect(),
            Err(_) => return Vec::new(),
        };
        let Ok(mut sticky) = self.sticky.write() else {
            return Vec::new();
        };
        for n in pending {
            sticky.insert(n);
        }
        // Borne : on garde les plus récents en ordre lexicographique stable.
        while sticky.len() > self.sticky_max {
            let first = sticky.iter().next().cloned();
            if let Some(f) = first {
                sticky.remove(&f);
            } else {
                break;
            }
        }
        sticky.iter().cloned().collect()
    }

    pub fn sticky_set(&self) -> Vec<String> {
        self.sticky
            .read()
            .map(|g| g.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn pending_promotions(&self) -> usize {
        self.pending_promotion.read().map(|g| g.len()).unwrap_or(0)
    }

    /// Schémas `eager` d'un serveur, sous plafond global (§8.9).
    pub async fn eager_schemas(&self, servers: &[String]) -> penelope_store::Result<Vec<Value>> {
        let wanted = servers.to_vec();
        let per_schema = self.schema_max_bytes;
        let total_max = self.eager_total_max;
        self.store
            .read(move |c| {
                let mut out = Vec::new();
                let mut total = 0usize;
                for s in &wanted {
                    let mut st = c.prepare(
                        "SELECT qualified, server, name, title, description, input_schema,
                                output_schema, annotations, risk, schema_bytes
                         FROM mcp_tools WHERE server = ?1 ORDER BY name",
                    )?;
                    let rows = st.query_map([s], row_to_tool)?;
                    for r in rows {
                        let t = r?;
                        let d = t.describe(per_schema);
                        let size = d.to_string().len();
                        if total + size > total_max {
                            return Ok(out);
                        }
                        total += size;
                        out.push(d);
                    }
                }
                Ok(out)
            })
            .await
    }

    /// Les trois méta-outils toujours exposés (§8.9).
    pub fn meta_tools() -> Vec<(&'static str, &'static str, Value)> {
        vec![
            (
                "tool_search",
                "Cherche un outil MCP par mots-clés. Renvoie noms, descriptions courtes \
                 et niveau de risque.",
                json!({
                    "type":"object",
                    "properties":{
                        "query":{"type":"string","description":"mots-clés"},
                        "server":{"type":"string","description":"limiter à un serveur"},
                        "limit":{"type":"integer","minimum":1,"maximum":50,"default":10}
                    },
                    "required":["query"]
                }),
            ),
            (
                "tool_describe",
                "Renvoie les schémas complets des outils nommés.",
                json!({
                    "type":"object",
                    "properties":{
                        "names":{"type":"array","items":{"type":"string"},"maxItems":20}
                    },
                    "required":["names"]
                }),
            ),
            (
                "tool_call",
                "Appelle un outil MCP par son nom qualifié, avec des arguments validés \
                 contre son schéma.",
                json!({
                    "type":"object",
                    "properties":{
                        "name":{"type":"string"},
                        "args":{"type":"object"}
                    },
                    "required":["name","args"]
                }),
            ),
        ]
    }

    /// Valide des arguments contre le schéma de l'outil avant tout appel.
    pub async fn validate_args(&self, qualified: &str, args: &Value) -> crate::error::Result<()> {
        let Some(t) = self.get(qualified).await? else {
            return Err(crate::error::McpError::UnknownTool(qualified.to_string()));
        };
        penelope_kernel::schema::validate_ok(&t.input_schema, args).map_err(|reason| {
            crate::error::McpError::InvalidArguments {
                tool: qualified.to_string(),
                reason,
            }
        })
    }
}

fn lexical_score(needle: &str, t: &RegisteredTool) -> f32 {
    if needle.is_empty() {
        return 0.0;
    }
    let mut score = 0.0f32;
    for term in needle.split_whitespace() {
        if t.name.to_lowercase().contains(term) {
            score += 3.0;
        }
        if t.qualified.to_lowercase().contains(term) {
            score += 1.5;
        }
        if t.description.to_lowercase().contains(term) {
            score += 1.0;
        }
        if t.server.to_lowercase().contains(term) {
            score += 0.5;
        }
    }
    score
}

/// Requête FTS5 assainie.
pub fn fts_query(q: &str) -> String {
    let cleaned: String = q
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect();
    cleaned
        .split_whitespace()
        .filter(|w| !matches!(*w, "AND" | "OR" | "NOT" | "NEAR"))
        .filter(|w| w.chars().count() > 1)
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn row_to_tool(
    r: &penelope_store::rusqlite::Row<'_>,
) -> penelope_store::rusqlite::Result<RegisteredTool> {
    let input_schema: String = r.get(5)?;
    let output_schema: Option<String> = r.get(6)?;
    let annotations: String = r.get(7)?;
    let risk: String = r.get(8)?;
    Ok(RegisteredTool {
        qualified: r.get(0)?,
        server: r.get(1)?,
        name: r.get(2)?,
        title: r.get(3)?,
        description: r.get(4)?,
        input_schema: serde_json::from_str(&input_schema).unwrap_or(json!({"type":"object"})),
        output_schema: output_schema.and_then(|s| serde_json::from_str(&s).ok()),
        annotations: serde_json::from_str(&annotations).unwrap_or(json!({})),
        risk: RiskClass::parse(&risk).unwrap_or(RiskClass::Unknown),
        schema_bytes: r.get::<_, i64>(9)? as usize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(name: &str, desc: &str, ann: Value) -> ToolDescriptor {
        ToolDescriptor {
            name: name.into(),
            title: None,
            description: desc.into(),
            input_schema: json!({
                "type":"object",
                "properties":{"path":{"type":"string"}},
                "required":["path"]
            }),
            output_schema: None,
            annotations: ann,
            icons: None,
        }
    }

    async fn registry() -> ToolRegistry {
        let store = Store::open_memory().unwrap();
        store
            .write(|tx| {
                for s in ["redmine", "forge", "fs"] {
                    tx.execute(
                        "INSERT INTO mcp_servers(name, transport, config, state, updated_at)
                         VALUES(?1,'stdio','{}','ready','t')",
                        [s],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
        ToolRegistry::new(store, 30, 8 * 1024, 64 * 1024)
    }

    #[test]
    fn qualified_names_are_normalised() {
        assert_eq!(
            qualified_name("Redmine", "get_issue"),
            "mcp__redmine__get_issue"
        );
        assert_eq!(
            qualified_name("my server", "Do Thing!"),
            "mcp__my_server__do_thing"
        );
        assert!(
            qualified_name("a", "b")
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        );
    }

    #[test]
    fn long_names_are_truncated_with_a_hash() {
        let long = "t".repeat(120);
        let q = qualified_name("serveur", &long);
        assert_eq!(q.len(), 64);
        // Deux noms longs différents ne collisionnent pas.
        let q2 = qualified_name("serveur", &format!("{long}x"));
        assert_ne!(q, q2);
    }

    #[tokio::test]
    async fn search_finds_tools_by_keyword() {
        let r = registry().await;
        r.replace_server_tools(
            "redmine",
            vec![
                RegisteredTool::from_descriptor(
                    "redmine",
                    &descriptor(
                        "get_issue",
                        "Lit un ticket Redmine",
                        json!({"readOnlyHint":true}),
                    ),
                ),
                RegisteredTool::from_descriptor(
                    "redmine",
                    &descriptor("update_issue", "Met à jour un ticket", json!({})),
                ),
            ],
            "t",
        )
        .await
        .unwrap();

        let hits = r.search("ticket", None, 10).await.unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        let hits = r.search("get_issue", None, 10).await.unwrap();
        assert_eq!(hits[0].tool.name, "get_issue");
        assert_eq!(hits[0].tool.risk, RiskClass::Read);
    }

    #[tokio::test]
    async fn search_can_be_scoped_to_a_server() {
        let r = registry().await;
        r.replace_server_tools(
            "redmine",
            vec![RegisteredTool::from_descriptor(
                "redmine",
                &descriptor("get_issue", "ticket", json!({})),
            )],
            "t",
        )
        .await
        .unwrap();
        r.replace_server_tools(
            "forge",
            vec![RegisteredTool::from_descriptor(
                "forge",
                &descriptor("get_issue", "issue github", json!({})),
            )],
            "t",
        )
        .await
        .unwrap();

        assert_eq!(r.search("issue", None, 10).await.unwrap().len(), 2);
        let scoped = r.search("issue", Some("forge"), 10).await.unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].tool.server, "forge");
    }

    #[tokio::test]
    async fn replacing_a_server_removes_its_old_tools() {
        let r = registry().await;
        r.replace_server_tools(
            "redmine",
            vec![RegisteredTool::from_descriptor(
                "redmine",
                &descriptor("ancien", "x", json!({})),
            )],
            "t",
        )
        .await
        .unwrap();
        let g1 = r.generation();
        r.replace_server_tools(
            "redmine",
            vec![RegisteredTool::from_descriptor(
                "redmine",
                &descriptor("nouveau", "y", json!({})),
            )],
            "t",
        )
        .await
        .unwrap();
        assert!(r.generation() > g1, "une nouvelle génération est publiée");
        assert_eq!(r.count().await.unwrap(), 1);
        assert!(r.get("mcp__redmine__ancien").await.unwrap().is_none());
        assert!(r.get("mcp__redmine__nouveau").await.unwrap().is_some());
    }

    /// §8.9 : promotion **seulement** à la frontière de compaction.
    #[tokio::test]
    async fn promotion_waits_for_the_compaction_boundary() {
        let r = registry().await;
        r.replace_server_tools(
            "redmine",
            vec![RegisteredTool::from_descriptor(
                "redmine",
                &descriptor("get_issue", "ticket", json!({})),
            )],
            "t",
        )
        .await
        .unwrap();

        r.describe(&["mcp__redmine__get_issue".to_string()])
            .await
            .unwrap();
        assert!(
            r.sticky_set().is_empty(),
            "rien n'est promu en cours de session : le cache reste intact"
        );
        assert_eq!(r.pending_promotions(), 1);

        let sticky = r.apply_promotions();
        assert_eq!(sticky, vec!["mcp__redmine__get_issue"]);
        assert_eq!(r.pending_promotions(), 0);
    }

    #[tokio::test]
    async fn sticky_set_is_bounded() {
        let store = Store::open_memory().unwrap();
        let r = ToolRegistry::new(store, 3, 8192, 65536);
        for i in 0..10 {
            r.mark_for_promotion(&[format!("mcp__s__t{i:02}")]);
        }
        let sticky = r.apply_promotions();
        assert_eq!(sticky.len(), 3);
    }

    #[tokio::test]
    async fn oversized_schema_is_summarised() {
        let r = registry().await;
        let mut d = descriptor("gros", "outil au gros schéma", json!({}));
        let props: serde_json::Map<String, Value> = (0..400)
            .map(|i| {
                (
                    format!("champ_{i}"),
                    json!({"type":"string","description":"x".repeat(200)}),
                )
            })
            .collect();
        d.input_schema = json!({"type":"object","properties":props,"required":["champ_0"]});
        let t = RegisteredTool::from_descriptor("fs", &d);
        assert!(t.schema_bytes > 8 * 1024);

        r.replace_server_tools("fs", vec![t], "t").await.unwrap();
        let described = r.describe(&["mcp__fs__gros".to_string()]).await.unwrap();
        assert_eq!(described[0]["schemaTruncated"], true);
        assert!(
            described[0]["inputSchema"]["x-penelope-note"].is_string(),
            "le schéma doit être résumé, pas tronqué au hasard"
        );
        assert_eq!(described[0]["inputSchema"]["required"][0], "champ_0");
    }

    #[tokio::test]
    async fn eager_schemas_respect_the_global_cap() {
        let store = Store::open_memory().unwrap();
        store
            .write(|tx| {
                tx.execute(
                    "INSERT INTO mcp_servers(name, transport, config, state, updated_at)
                     VALUES('fs','stdio','{}','ready','t')",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        // Plafond global volontairement bas : 2 Ko.
        let r = ToolRegistry::new(store, 30, 8192, 2048);
        let tools: Vec<RegisteredTool> = (0..50)
            .map(|i| {
                RegisteredTool::from_descriptor(
                    "fs",
                    &descriptor(&format!("outil_{i}"), &"description ".repeat(20), json!({})),
                )
            })
            .collect();
        r.replace_server_tools("fs", tools, "t").await.unwrap();
        let eager = r.eager_schemas(&["fs".to_string()]).await.unwrap();
        let total: usize = eager.iter().map(|v| v.to_string().len()).sum();
        assert!(total <= 2048, "total {total}");
        assert!(!eager.is_empty());
    }

    #[tokio::test]
    async fn arguments_are_validated_before_the_call() {
        let r = registry().await;
        r.replace_server_tools(
            "fs",
            vec![RegisteredTool::from_descriptor(
                "fs",
                &descriptor("read", "lire", json!({})),
            )],
            "t",
        )
        .await
        .unwrap();

        r.validate_args("mcp__fs__read", &json!({"path":"a.rs"}))
            .await
            .unwrap();
        let e = r
            .validate_args("mcp__fs__read", &json!({}))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("requise"), "{e}");
        assert!(
            r.validate_args("mcp__fs__inconnu", &json!({}))
                .await
                .is_err()
        );
    }

    #[test]
    fn meta_tools_are_the_three_of_the_prd() {
        let names: Vec<&str> = ToolRegistry::meta_tools()
            .iter()
            .map(|(n, _, _)| *n)
            .collect();
        assert_eq!(names, vec!["tool_search", "tool_describe", "tool_call"]);
    }

    #[test]
    fn fts_query_uses_prefix_or() {
        assert_eq!(fts_query("ticket redmine"), "\"ticket\"* OR \"redmine\"*");
        assert_eq!(fts_query("a"), "");
    }

    #[test]
    fn short_form_truncates_long_descriptions() {
        let t = RegisteredTool::from_descriptor(
            "s",
            &descriptor("t", &"x".repeat(500), json!({"destructiveHint":true})),
        );
        let s = t.short();
        assert!(s["description"].as_str().unwrap().chars().count() <= 201);
        assert_eq!(s["risk"], "destructive");
    }
}
