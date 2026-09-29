//! `env_explore` (issue #260) : chercher dans la carte par besoin, pas par nom seulement.
//!
//! « qu'y a-t-il pour naviguer ? » ne se tape pas `safari` : un besoin se traduit par un
//! vocabulaire ([`NEEDS`]), et chaque mot de la demande est aussi cherché tel quel dans
//! les noms, paquets et identifiants. Lecture seule, sur la dernière carte : aucune sonde
//! dans un tour.

use super::{Capability, Environment, Found};
use penelope_platform::discover::App;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Un besoin : les mots qui le disent, et les noms (outils, applications) qui y répondent.
/// `mcp` et `inference` désignent en plus les capacités de ce genre.
pub const NEEDS: &[(&[&str], &[&str])] = &[
    (
        &["navigateur", "navigation", "browser", "web"],
        &[
            "safari",
            "safaridriver",
            "firefox",
            "google chrome",
            "chromium",
            "chromedriver",
            "geckodriver",
            "brave browser",
            "microsoft edge",
            "arc",
            "orion",
            "playwright",
        ],
    ),
    (
        &["swift", "xcode", "ios"],
        &[
            "swift",
            "swiftc",
            "xcrun",
            "xcodebuild",
            "xcode",
            "swiftlint",
            "swiftformat",
            "sourcekit-lsp",
        ],
    ),
    (
        &["compilateur", "compiler", "compilation"],
        &[
            "swiftc",
            "rustc",
            "cargo",
            "clang",
            "gcc",
            "go",
            "javac",
            "tsc",
            "zig",
            "xcodebuild",
        ],
    ),
    (
        &["inference", "llm", "modele", "modeles", "ia"],
        &[
            "ollama",
            "lms",
            "lm studio",
            "mlx_lm.server",
            "mlx_audio.server",
            "llama-server",
            "vllm",
        ],
    ),
    (
        &["video", "audio", "media", "son"],
        &[
            "ffmpeg",
            "yt-dlp",
            "sox",
            "mpv",
            "vlc",
            "handbrakecli",
            "whisper-cli",
        ],
    ),
    (
        &["image", "images", "photo", "photos"],
        &[
            "magick", "exiftool", "sips", "pngquant", "vips", "aperçu", "preview",
        ],
    ),
    (
        &["conteneur", "conteneurs", "container", "docker"],
        &[
            "docker", "podman", "colima", "orbstack", "limactl", "kubectl",
        ],
    ),
    (
        &["python"],
        &["python3", "uv", "uvx", "pipx", "pip3", "poetry", "conda"],
    ),
    (
        &["javascript", "node", "js", "typescript"],
        &["node", "npm", "npx", "pnpm", "yarn", "bun", "deno", "tsc"],
    ),
    (
        &["forge", "git", "github", "gitlab"],
        &["git", "gh", "glab", "github desktop"],
    ),
    (
        &["mail", "courriel", "email", "e-mail"],
        &["mail", "microsoft outlook", "neomutt", "mutt"],
    ),
    (
        &["pdf", "document", "documents", "bureautique"],
        &[
            "pdftotext",
            "qpdf",
            "gs",
            "mutool",
            "pandoc",
            "soffice",
            "pages",
            "numbers",
            "keynote",
            "aperçu",
            "preview",
        ],
    ),
];

/// Mots d'une demande qui ne disent rien du besoin.
const STOP: &[&str] = &[
    "les", "des", "une", "pour", "avec", "local", "locale", "locaux", "quoi", "outil", "outils",
    "faire", "the", "and", "for",
];

/// Minuscules sans accents : « Inférence » et « inference » se valent.
pub fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

/// La demande comprise : les noms que ses besoins désignent, les genres de capacités, et
/// les mots à chercher tels quels.
struct Query {
    names: Vec<String>,
    kinds: Vec<&'static str>,
    words: Vec<String>,
}

fn understand(need: &str) -> Query {
    let folded = fold(need);
    let words: Vec<String> = folded
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.')
        .filter(|w| w.len() >= 2 && !STOP.contains(w))
        .map(str::to_string)
        .collect();
    let mut q = Query {
        names: Vec::new(),
        kinds: Vec::new(),
        words: words.clone(),
    };
    for w in &words {
        if w == "mcp" {
            q.kinds.push("mcp");
        }
        for (keys, names) in NEEDS {
            if keys.contains(&w.as_str()) {
                q.names.extend(names.iter().map(|n| fold(n)));
                if keys.contains(&"inference") {
                    q.kinds.push("inference");
                }
            }
        }
    }
    q
}

impl Query {
    fn matches(&self, fields: &[&str]) -> bool {
        fields.iter().any(|f| {
            let f = fold(f);
            // Un mot de deux lettres (`ia`, `go`) se trouverait partout : il ne vaut que par
            // son besoin, ou comme nom exact.
            self.names.contains(&f)
                || self
                    .words
                    .iter()
                    .any(|w| *w == f || (w.len() >= 3 && f.contains(w.as_str())))
        })
    }
}

fn tool_matches(q: &Query, t: &Found) -> bool {
    q.matches(&[&t.name, t.package.as_deref().unwrap_or_default()])
}

fn app_matches(q: &Query, a: &App) -> bool {
    q.matches(&[&a.name, a.bundle_id.as_deref().unwrap_or_default()])
}

fn capability_matches(q: &Query, c: &Capability) -> bool {
    q.kinds.contains(&c.kind.as_str()) || q.matches(&[&c.name, &c.via, &c.id])
}

/// Le résultat d'`env_explore`. `need` : un besoin ou un nom ; `kind` : `tools`, `apps`,
/// `capabilities` ou `hardware` ; `proposed` : les capacités déjà proposées au
/// propriétaire, et quand.
pub fn explore(
    env: Option<&Environment>,
    need: Option<&str>,
    kind: Option<&str>,
    limit: usize,
    proposed: &BTreeMap<String, String>,
) -> Value {
    let Some(env) = env else {
        return json!({
            "map": null,
            "note": "Carte de la machine pas encore dressée : elle l'est au démarrage du \
                     daemon, puis toutes les heures ; `penelope doctor` la dresse tout de suite.",
        });
    };
    let want = |k: &str| kind.is_none_or(|x| x == k);
    let q = need.map(understand);
    let keep_tool = |t: &&Found| q.as_ref().is_none_or(|q| tool_matches(q, t));
    let keep_app = |a: &&App| q.as_ref().is_none_or(|q| app_matches(q, a));
    let keep_cap = |c: &&Capability| q.as_ref().is_none_or(|q| capability_matches(q, c));

    let mut out = serde_json::Map::new();
    out.insert("checked_at".into(), json!(env.checked_at));
    if let Some(n) = need {
        out.insert("need".into(), json!(n));
    }
    // Sans besoin ni genre, le sommaire : ce que la carte contient, pas deux mille lignes.
    if need.is_none() && kind.is_none() {
        let mut by_source: BTreeMap<&str, usize> = BTreeMap::new();
        for t in &env.tools {
            *by_source.entry(t.source.as_str()).or_default() += 1;
        }
        out.insert("hardware".into(), json!(env.hardware));
        out.insert(
            "tools".into(),
            json!({"total": env.tools.len(), "by_source": by_source}),
        );
        out.insert("apps".into(), json!({"total": env.apps.len()}));
        out.insert(
            "capabilities".into(),
            capabilities(env.capabilities.iter().collect(), proposed),
        );
        out.insert("changes".into(), json!(env.changes));
        out.insert(
            "hint".into(),
            json!(
                "`need` cherche par besoin (« navigateur », « compilateur Swift », « MCP », \
                   « inférence locale ») ou par nom ; `kind` limite à un genre."
            ),
        );
        return Value::Object(out);
    }

    let mut truncated = false;
    let mut cut = |v: Vec<Value>| {
        truncated |= v.len() > limit;
        v.into_iter().take(limit).collect::<Vec<_>>()
    };
    // Le matériel répond aux questions de matériel, et à celles d'inférence : la mémoire
    // unifiée décide des modèles qui tiennent (#259).
    if want("hardware")
        && (kind == Some("hardware")
            || q.as_ref().is_some_and(|q| {
                q.kinds.contains(&"inference")
                    || q.words.iter().any(|w| {
                        ["materiel", "memoire", "gpu", "puce", "cpu", "ram"].contains(&w.as_str())
                    })
            }))
    {
        out.insert("hardware".into(), json!(env.hardware));
    }
    if want("capabilities") {
        let caps: Vec<&Capability> = env.capabilities.iter().filter(keep_cap).collect();
        out.insert("capabilities".into(), capabilities(caps, proposed));
    }
    if want("apps") {
        let apps: Vec<Value> = env.apps.iter().filter(keep_app).map(|a| json!(a)).collect();
        out.insert("apps".into(), json!(cut(apps)));
    }
    if want("tools") {
        let tools: Vec<Value> = env
            .tools
            .iter()
            .filter(keep_tool)
            .map(|t| json!(t))
            .collect();
        out.insert("tools".into(), json!(cut(tools)));
    }
    if truncated {
        out.insert(
            "truncated".into(),
            json!(format!(
                "plus de {limit} résultats : précise `need` ou monte `limit`"
            )),
        );
    }
    out.insert(
        "note".into(),
        json!(
            "Découvrir n'autorise rien : lancer un outil passe par `shell_exec` et ses \
               approbations ; un MCP ou un serveur non branché se propose au propriétaire."
        ),
    );
    Value::Object(out)
}

fn capabilities(caps: Vec<&Capability>, proposed: &BTreeMap<String, String>) -> Value {
    json!(
        caps.into_iter()
            .map(|c| {
                let mut v = json!(c);
                if let Some(at) = proposed.get(&c.id) {
                    v["proposed_at"] = json!(at);
                }
                v
            })
            .collect::<Vec<_>>()
    )
}
