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

mod search;
pub use search::fts_query;

/// Similarité cosinus minimale d'un outil trouvé par le sens.
const VECTOR_MIN_SIMILARITY: f32 = 0.3;

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

    /// Empreinte de ce que le modèle lit et de ce qui décide du risque : description,
    /// schéma d'entrée, annotations. Un « Toujours » vaut pour cette empreinte (#92).
    pub fn fingerprint(&self) -> String {
        penelope_kernel::canonical::sha256_hex(
            penelope_kernel::canonical::canonical_json(&json!({
                "description": self.description,
                "input_schema": self.input_schema,
                "annotations": self.annotations,
            }))
            .as_bytes(),
        )
    }

    /// Motifs d'injection trouvés par le détecteur local dans la description de l'outil
    /// ou celles de ses paramètres : ce texte est lu par le modèle comme une consigne
    /// (« tool poisoning », #92).
    pub fn flags(&self) -> Vec<String> {
        let mut texts = vec![self.description.clone()];
        collect_descriptions(&self.input_schema, &mut texts);
        let mut out: Vec<String> = texts
            .iter()
            .flat_map(|t| penelope_observe::injection::scan(t))
            .map(|f| format!("{} « {} »", f.rule, f.excerpt.trim()))
            .collect();
        out.dedup();
        out
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

/// Descriptions des propriétés d'un schéma, à toute profondeur.
fn collect_descriptions(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                if k == "description"
                    && let Some(d) = x.as_str()
                {
                    out.push(d.to_string());
                } else {
                    collect_descriptions(x, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_descriptions(x, out)),
        _ => {}
    }
}

/// Ce qu'a changé l'inscription des outils d'un serveur (#92).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReplaceReport {
    pub generation: u64,
    /// Outils déjà connus dont la description, le schéma ou les annotations ont changé.
    pub changed: Vec<String>,
    /// Outils nouveaux ou changés que le détecteur local signale, avec ses motifs.
    pub flagged: Vec<(String, Vec<String>)>,
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
    ///
    /// Chaque outil garde son empreinte et la date où elle est apparue ; le rapport dit
    /// lesquels ont changé depuis la dernière inscription (rug pull, #92) et lesquels le
    /// détecteur local signale.
    pub async fn replace_server_tools(
        &self,
        server: &str,
        tools: Vec<RegisteredTool>,
        now: &str,
    ) -> penelope_store::Result<ReplaceReport> {
        let srv = server.to_string();
        let ts = now.to_string();
        let generation = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;

        self.store
            .write(move |tx| {
                let mut known: std::collections::HashMap<String, (String, Option<String>)> =
                    std::collections::HashMap::new();
                {
                    let mut st = tx.prepare(
                        "SELECT qualified, fingerprint, first_seen FROM mcp_tools
                         WHERE server = ?1",
                    )?;
                    let rows = st.query_map([&srv], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get(2)?))
                    })?;
                    for r in rows {
                        let (q, fp, first) = r?;
                        known.insert(q, (fp, first));
                    }
                }
                let mut report = ReplaceReport {
                    generation,
                    ..Default::default()
                };
                tx.execute("DELETE FROM mcp_tools WHERE server = ?1", [&srv])?;
                tx.execute("DELETE FROM mcp_tools_fts WHERE server = ?1", [&srv])?;
                for t in &tools {
                    let fingerprint = t.fingerprint();
                    let (first_seen, fresh) = match known.get(&t.qualified) {
                        Some((fp, first)) if fp == &fingerprint => (first.clone(), false),
                        Some((fp, _)) => {
                            // Empreinte vide : inscrit avant la migration, rien à comparer.
                            if !fp.is_empty() {
                                report.changed.push(t.qualified.clone());
                            }
                            (Some(ts.clone()), !fp.is_empty())
                        }
                        None => (Some(ts.clone()), true),
                    };
                    if fresh {
                        let flags = t.flags();
                        if !flags.is_empty() {
                            report.flagged.push((t.qualified.clone(), flags));
                        }
                    }
                    tx.execute(
                        "INSERT OR REPLACE INTO mcp_tools(qualified, server, name, title,
                            description, input_schema, output_schema, annotations, risk,
                            schema_bytes, generation, updated_at, fingerprint, first_seen)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
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
                            ts,
                            fingerprint,
                            first_seen
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
                Ok(report)
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

    /// Outils d'un serveur, triés par nom.
    pub async fn list_server(&self, server: &str) -> penelope_store::Result<Vec<RegisteredTool>> {
        let srv = server.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT qualified, server, name, title, description, input_schema,
                            output_schema, annotations, risk, schema_bytes
                     FROM mcp_tools WHERE server = ?1 ORDER BY name",
                )?;
                let rows = st.query_map([&srv], row_to_tool)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })
            .await
    }

    pub async fn count(&self) -> penelope_store::Result<i64> {
        self.store
            .read(|c| Ok(c.query_row("SELECT count(*) FROM mcp_tools", [], |r| r.get(0))?))
            .await
    }

    /// Noms qualifiés de tous les outils connus, pour proposer un nom proche (issue #110).
    pub async fn names(&self) -> penelope_store::Result<Vec<String>> {
        self.store
            .read(|c| {
                let mut st = c.prepare("SELECT qualified FROM mcp_tools ORDER BY qualified")?;
                let rows = st.query_map([], |r| r.get::<_, String>(0))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
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
                        let mut d = t.describe(per_schema);
                        // Exposée d'office au modèle : la description dit d'où elle vient, et
                        // disparaît si le détecteur y voit une consigne (#92).
                        let flags = t.flags();
                        d["description"] = json!(if flags.is_empty() {
                            format!(
                                "[outil du serveur MCP `{}`, description non vérifiée] {}",
                                t.server, t.description
                            )
                        } else {
                            // Les motifs seulement : l'extrait redirait la consigne.
                            let rules: Vec<&str> = flags
                                .iter()
                                .map(|f| f.split(" «").next().unwrap_or(f))
                                .collect();
                            format!(
                                "[description de `{}` retirée : le détecteur local de \
                                 Pénélope y a vu une consigne ({})]",
                                t.server,
                                rules.join(", ")
                            )
                        });
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
                "Cherche un outil par mots-clés : outils natifs à la demande (planification, \
                 git, configuration, intentions, skills…) et outils MCP. Renvoie noms, \
                 descriptions courtes et niveau de risque.",
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
                "Appelle un outil par son nom : natif à la demande (`schedule_create`…) ou \
                 MCP qualifié (`mcp__serveur__outil`), avec des arguments validés contre son \
                 schéma.",
                json!({
                    "type":"object",
                    "properties":{
                        "name":{"type":"string","description":"nom de l'outil visé"},
                        "args":{
                            "type":"object",
                            "additionalProperties":true,
                            "description":"arguments de l'outil visé, selon son schéma \
                                           (`tool_describe`) : par exemple {\"issue_id\": 7653}"
                        },
                        "args_json":{
                            "type":"string",
                            "description":"les mêmes arguments en chaîne JSON, si l'objet \
                                           `args` arrive vide"
                        },
                        "pourquoi":{
                            "type":"string",
                            "maxLength":200,
                            "description":"une phrase simple, pour le propriétaire : ce que \
                                           tu cherches à faire (carte d'approbation)"
                        }
                    },
                    "required":["name"]
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
mod tests;
