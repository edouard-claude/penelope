use super::*;

// ------------------------------------------------------------------ pratiques

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PracticeStatus {
    Active,
    Contestee,
    Retiree,
}

impl PracticeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PracticeStatus::Active => "active",
            PracticeStatus::Contestee => "contestee",
            PracticeStatus::Retiree => "retiree",
        }
    }
    pub fn parse(s: &str) -> PracticeStatus {
        match s {
            "contestee" | "contestée" => PracticeStatus::Contestee,
            "retiree" | "retirée" => PracticeStatus::Retiree,
            _ => PracticeStatus::Active,
        }
    }
}

/// Règle défaisable (§6.4) : un défaut, des exceptions contextuelles, des écarts observés.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Practice {
    pub id: String,
    pub scope: String,
    pub confiance: f64,
    pub preuves: u32,
    pub statut: PracticeStatus,
    pub maj: String,
    pub declencheurs: Vec<String>,
    pub title: String,
    pub default_entry: Option<VaultEntry>,
    pub exceptions: Vec<VaultEntry>,
    pub ecarts: Vec<VaultEntry>,
    /// Autres propriétés (`created`, `updated`, `tags`…), conservées à la réécriture.
    #[serde(skip)]
    pub extra: BTreeMap<String, FmValue>,
}

/// Résultat du rappel d'une pratique dans un contexte donné (§6.7 point 3).
#[derive(Debug, Clone, PartialEq)]
pub struct PracticeRecall {
    pub id: String,
    pub confiance: f64,
    pub default_text: String,
    /// Exceptions dont le `quand` est **satisfait**.
    pub applicable: Vec<(String, When, f64)>,
    /// Exceptions dont une clé est inconnue mais dont la similarité est forte.
    pub to_verify: Vec<String>,
}

impl PracticeRecall {
    /// Rendu exact du PRD §6.7.
    pub fn render(&self) -> String {
        let mut s = format!(
            "[pratique: {} · confiance {:.1}]\nDéfaut : {}",
            self.id, self.confiance, self.default_text
        );
        for (text, when, conf) in &self.applicable {
            s.push_str(&format!(
                "\nS'applique ici : {text} ({}) · confiance {conf:.1}",
                when.render().replace("; ", ", ")
            ));
        }
        if !self.to_verify.is_empty() {
            s.push_str(&format!("\nÀ vérifier : {}", self.to_verify.join(" ; ")));
        }
        s
    }
}

impl Practice {
    pub fn parse(raw: &str, fallback_id: &str) -> Result<Practice, String> {
        let fm = frontmatter::parse(raw).map_err(|e| e.to_string())?;
        if fm.str("type") != Some("pratique") {
            return Err("`type: pratique` attendu dans le frontmatter".into());
        }
        let (entries, _) = parse_entries(raw);

        let title = fm
            .body
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .unwrap_or(fallback_id)
            .trim()
            .to_string();

        let by_section = |name: &str| -> Vec<VaultEntry> {
            entries
                .iter()
                .filter(|e| section_matches(&e.section, name))
                .cloned()
                .collect()
        };

        let defaults = by_section("Défaut");
        let exceptions = by_section("Exceptions");
        let ecarts = by_section("Écarts observés");

        Ok(Practice {
            id: {
                let id = fm.string("id");
                if id.is_empty() {
                    fallback_id.to_string()
                } else {
                    id
                }
            },
            scope: {
                let s = fm.string("scope");
                if s.is_empty() { "global".into() } else { s }
            },
            confiance: fm.f64("confiance").unwrap_or(0.5).clamp(0.0, 1.0),
            preuves: fm.f64("preuves").unwrap_or(0.0) as u32,
            statut: PracticeStatus::parse(&fm.string("statut")),
            maj: fm.string("maj"),
            declencheurs: fm.list("declencheurs"),
            title,
            default_entry: defaults.into_iter().next(),
            exceptions,
            ecarts,
            extra: fm
                .fields
                .iter()
                .filter(|(k, _)| {
                    !matches!(
                        k.as_str(),
                        "type"
                            | "id"
                            | "scope"
                            | "confiance"
                            | "preuves"
                            | "statut"
                            | "maj"
                            | "declencheurs"
                    )
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        })
    }

    /// Entrées invalides : une exception ou un écart **sans `quand`** est signalé et
    /// ignoré au rappel (§6.4).
    pub fn invalid_entries(&self) -> Vec<(&VaultEntry, &'static str)> {
        let mut out = Vec::new();
        for e in self.exceptions.iter().chain(self.ecarts.iter()) {
            if e.annotations.quand.is_none() {
                out.push((e, "exception ou écart sans prédicat `quand`"));
            }
        }
        out
    }

    /// Rappel contextuel : défaut **et uniquement** les exceptions satisfaites.
    ///
    /// Les écarts ne sont **jamais** injectés automatiquement : ce n'est pas encore du
    /// savoir (§6.7 point 4).
    pub fn recall(&self, ctx: &BTreeMap<String, String>, verify_threshold: f64) -> PracticeRecall {
        let mut applicable = Vec::new();
        let mut to_verify = Vec::new();

        for e in &self.exceptions {
            let Some(when) = &e.annotations.quand else {
                continue; // invalide : ignoré
            };
            match when.evaluate(ctx) {
                WhenMatch::Satisfied => applicable.push((
                    e.text.clone(),
                    when.clone(),
                    e.annotations.confiance.unwrap_or(self.confiance),
                )),
                WhenMatch::Unknown(missing) => {
                    // Prédicat inconnu ⇒ pas satisfait, mais listé si la similarité est
                    // forte : on approxime la similarité par la proportion de clés connues.
                    let known = when.clauses.len().saturating_sub(missing.len()) as f64;
                    let ratio = if when.clauses.is_empty() {
                        0.0
                    } else {
                        known / when.clauses.len() as f64
                    };
                    if ratio >= verify_threshold {
                        to_verify.push(format!("{} ({})", e.text, when.render()));
                    }
                }
                WhenMatch::Contradicted => {}
            }
        }

        PracticeRecall {
            id: self.id.clone(),
            confiance: self.confiance,
            default_text: self
                .default_entry
                .as_ref()
                .map(|e| e.text.clone())
                .unwrap_or_else(|| self.title.clone()),
            applicable,
            to_verify,
        }
    }

    /// Confiance déterministe (§6.8) : `(succès + 1) / (succès + contradictions + 2)`.
    pub fn confidence(successes: u32, contradictions: u32) -> f64 {
        (successes as f64 + 1.0) / (successes as f64 + contradictions as f64 + 2.0)
    }

    /// Statut dérivé de la confiance (§6.8).
    pub fn derived_status(
        confiance: f64,
        observations: u32,
        threshold: f64,
        min_obs: u32,
    ) -> PracticeStatus {
        if confiance < threshold && observations >= min_obs {
            PracticeStatus::Contestee
        } else {
            PracticeStatus::Active
        }
    }

    pub fn render(&self) -> String {
        let mut fields: BTreeMap<String, FmValue> = self.extra.clone();
        fields.insert("type".into(), FmValue::Str("pratique".into()));
        fields.insert("id".into(), FmValue::Str(self.id.clone()));
        fields.insert("scope".into(), FmValue::Str(self.scope.clone()));
        fields.insert("confiance".into(), FmValue::Num(self.confiance));
        fields.insert("preuves".into(), FmValue::Num(self.preuves as f64));
        fields.insert("statut".into(), FmValue::Str(self.statut.as_str().into()));
        fields.insert("maj".into(), FmValue::Str(self.maj.clone()));
        fields.insert(
            "declencheurs".into(),
            FmValue::List(self.declencheurs.clone()),
        );

        let mut body = format!("# {}\n\n## Défaut\n", self.title);
        // Le défaut est une entrée de liste comme les autres : sans puce, il disparaîtrait
        // à la relecture.
        if let Some(d) = &self.default_entry {
            body.push_str(&format!("- {} {}\n", d.text, d.annotations.render()));
        }
        body.push_str("\n## Exceptions\n");
        for e in &self.exceptions {
            body.push_str(&format!("- {} {}\n", e.text, e.annotations.render()));
        }
        body.push_str("\n## Écarts observés\n");
        for e in &self.ecarts {
            body.push_str(&format!("- {} {}\n", e.text, e.annotations.render()));
        }
        frontmatter::render(&fields, &body)
    }
}

fn section_matches(actual: &str, expected: &str) -> bool {
    let norm = |s: &str| {
        s.to_lowercase()
            .replace(['é', 'è', 'ê'], "e")
            .replace('à', "a")
            .replace('ç', "c")
    };
    norm(actual) == norm(expected)
}
