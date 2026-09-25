//! Consolidation nocturne (§6.8) : Light → REM → Deep.
//!
//! **Portes déterministes, jugement du modèle à l'intérieur.** L'éligibilité, la
//! provenance et le cycle de vie sont du code. Faits, préférences, décisions et corrections
//! passent par la grille de tri ([`crate::grid`], issue #37) : le modèle juge chaque
//! critère, le code en déduit la place. Sa sortie est validée structurellement avant
//! écriture.

use crate::candidates::{Candidate, CandidateGroup, CandidateType};
use crate::provenance::Origin;
use crate::vault::When;
use serde::{Deserialize, Serialize};

mod contradiction;
pub use contradiction::{
    CONTRADICTION_SIMILARITY, Contradiction, contradicts, detect_contradiction,
};

/// Seuils de promotion (§6.8), issus de la configuration. Les écarts gardent leurs
/// seuils de répétition ; les autres types passent par la grille (issue #37).
#[derive(Debug, Clone, Copy)]
pub struct PromotionGates {
    pub ecart_min_occurrences: u32,
    pub ecart_min_sessions: u32,
    pub ecart_min_days: u32,
    pub max_retire_ratio: f64,
}

impl Default for PromotionGates {
    fn default() -> Self {
        PromotionGates {
            ecart_min_occurrences: 3,
            ecart_min_sessions: 3,
            ecart_min_days: 2,
            max_retire_ratio: 0.20,
        }
    }
}

impl PromotionGates {
    pub fn from_config(p: &penelope_kernel::config::Promotion) -> Self {
        PromotionGates {
            ecart_min_occurrences: p.ecart_min_occurrences,
            ecart_min_sessions: p.ecart_min_sessions,
            ecart_min_days: p.ecart_min_days,
            max_retire_ratio: p.max_retire_ratio,
        }
    }
}

/// Verdict de la porte déterministe.
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    /// Promotion directe autorisée.
    Promote,
    /// À trier par la grille (issue #37) : fait, préférence, décision, correction.
    Sort,
    /// À transformer en **proposition** HITL, jamais appliqué seul.
    Propose(String),
    /// Refusé, avec la raison (affichée dans le digest).
    Reject(String),
}

impl Gate {
    pub fn is_promote(&self) -> bool {
        matches!(self, Gate::Promote)
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Gate::Propose(r) | Gate::Reject(r) => Some(r),
            Gate::Promote | Gate::Sort => None,
        }
    }
}

/// Motif d'une règle notée par l'agent sans citation du propriétaire, demandée avant la
/// grille de tri (issue #24) ; gardé pour relire les demandes encore ouvertes.
pub const CONFIRM_REASON: &str = "notée par l'agent sans citation du propriétaire : à confirmer";

/// Anciens motifs de rejet d'une règle d'origine `agent`, rejouables.
pub const LEGACY_ORIGIN_REJECTIONS: &[&str] = &[
    "une préférence doit venir du propriétaire",
    "une correction doit venir du propriétaire",
    "une décision doit être confirmée par le propriétaire",
];

/// Applique la porte déterministe à un groupe de candidats (§6.8, tableau ; issue #37).
pub fn gate(g: &CandidateGroup, gates: &PromotionGates) -> Gate {
    // Les origines `untrusted` et `system` sont exclues **avant** toute construction de
    // prompt : elles ne doivent même pas atteindre le modèle de consolidation.
    if !g.origins.iter().any(|o| o.can_be_promoted()) {
        return Gate::Reject("origine non promouvable (contenu non fiable ou système)".into());
    }

    match g.ctype {
        // Plus de comptage ni d'importance : une règle dite une fois, explicitement, passe
        // la grille ; un état passager part au journal ; le propriétaire ne valide rien à
        // la main.
        CandidateType::Preference
        | CandidateType::Fait
        | CandidateType::Correction
        | CandidateType::Decision => Gate::Sort,
        CandidateType::Ecart => {
            if g.occurrences < gates.ecart_min_occurrences
                || g.distinct_sessions < gates.ecart_min_sessions
                || g.distinct_days < gates.ecart_min_days
            {
                return Gate::Reject(format!(
                    "écart vu {} fois dans {} sessions sur {} jours (minimum {}/{}/{})",
                    g.occurrences,
                    g.distinct_sessions,
                    g.distinct_days,
                    gates.ecart_min_occurrences,
                    gates.ecart_min_sessions,
                    gates.ecart_min_days
                ));
            }
            match g.common_when() {
                Some(_) => Gate::Promote,
                None => {
                    Gate::Reject("signatures de contexte incompatibles : intersection vide".into())
                }
            }
        }
        CandidateType::ProcedureCandidate => {
            if g.occurrences < 2 {
                Gate::Reject("une seule exécution réussie".into())
            } else if g.distinct_sessions < 2 {
                Gate::Reject("séquence non vérifiée dans deux sessions distinctes".into())
            } else {
                Gate::Propose("séquence réussie dans deux sessions : skill proposée".into())
            }
        }
    }
}

// ------------------------------------------------------------------ opérations

/// Opérations que le modèle de consolidation peut renvoyer (§6.8 point 2).
///
/// Il ne réécrit **jamais** un fichier : il propose des opérations, validées ensuite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    AddEntry {
        file: String,
        section: Option<String>,
        text: String,
        importance: Option<u8>,
        declencheurs: Option<Vec<String>>,
        /// État passager : date après laquelle l'entrée n'est plus injectée d'office.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expire: Option<String>,
        /// Donnée client, financière ou de sécurité : jamais injectée d'office.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensible: Option<bool>,
    },
    ReplaceEntry {
        uid: String,
        text: String,
    },
    /// L'ancienne entrée est retirée, la nouvelle la remplace avec un lien (`remplace`) et
    /// une date (`depuis`) : un fait corrigé ne coexiste plus avec l'ancien (issue #37).
    SupersedeEntry {
        uid: String,
        text: String,
        #[serde(default)]
        reason: Option<String>,
    },
    RetireEntry {
        uid: String,
        reason: String,
    },
    AddException {
        practice: String,
        text: String,
        quand: String,
        confiance: Option<f64>,
    },
    UpdateException {
        uid: String,
        text: Option<String>,
        quand: Option<String>,
        confiance: Option<f64>,
    },
    RecordEcart {
        practice: String,
        text: String,
        quand: String,
    },
    /// Toujours transformée en proposition HITL (§6.8).
    UpdateDefault {
        practice: String,
        text: String,
    },
    Link {
        from_uid: String,
        to_slug: String,
    },
    CreateEntity {
        kind: String,
        slug: String,
        title: String,
        body: String,
    },
}

impl Operation {
    pub fn kind(&self) -> &'static str {
        match self {
            Operation::AddEntry { .. } => "add_entry",
            Operation::ReplaceEntry { .. } => "replace_entry",
            Operation::SupersedeEntry { .. } => "supersede_entry",
            Operation::RetireEntry { .. } => "retire_entry",
            Operation::AddException { .. } => "add_exception",
            Operation::UpdateException { .. } => "update_exception",
            Operation::RecordEcart { .. } => "record_ecart",
            Operation::UpdateDefault { .. } => "update_default",
            Operation::Link { .. } => "link",
            Operation::CreateEntity { .. } => "create_entity",
        }
    }

    /// Une opération qui ne peut **jamais** être appliquée automatiquement.
    pub fn requires_hitl(&self) -> bool {
        matches!(self, Operation::UpdateDefault { .. })
    }

    /// Texte porté par l'opération, pour le filtre de contenu interdit.
    pub fn text(&self) -> Option<&str> {
        match self {
            Operation::AddEntry { text, .. }
            | Operation::ReplaceEntry { text, .. }
            | Operation::SupersedeEntry { text, .. }
            | Operation::AddException { text, .. }
            | Operation::RecordEcart { text, .. }
            | Operation::UpdateDefault { text, .. } => Some(text),
            Operation::UpdateException { text, .. } => text.as_deref(),
            Operation::CreateEntity { body, .. } => Some(body),
            _ => None,
        }
    }

    /// Entrée visée, pour la détection de conflit d'édition manuelle.
    pub fn target_uid(&self) -> Option<&str> {
        match self {
            Operation::ReplaceEntry { uid, .. }
            | Operation::SupersedeEntry { uid, .. }
            | Operation::RetireEntry { uid, .. }
            | Operation::UpdateException { uid, .. } => Some(uid),
            Operation::Link { from_uid, .. } => Some(from_uid),
            _ => None,
        }
    }
}

/// Schéma JSON de la sortie du modèle de consolidation.
pub fn operations_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "operations": {
                "type": "array",
                "maxItems": 50,
                "items": {
                    "type": "object",
                    "properties": {
                        "op": {"type": "string", "enum": [
                            "add_entry","replace_entry","supersede_entry","retire_entry",
                            "add_exception",
                            "update_exception","record_ecart","update_default","link",
                            "create_entity"
                        ]}
                    },
                    "required": ["op"]
                }
            }
        },
        "required": ["operations"],
        "additionalProperties": false
    })
}

/// Résultat de la validation structurelle (§6.8 point 3).
#[derive(Debug, Clone, PartialEq)]
pub struct Validation {
    pub applied: Vec<Operation>,
    pub proposals: Vec<(Operation, String)>,
    pub rejected: Vec<(Operation, String)>,
    pub deferred: Vec<(Operation, String)>,
}

/// Contexte de validation : ce que le harnais sait, que le modèle ne contrôle pas.
pub struct ValidationContext<'a> {
    /// uid existants, par fichier.
    pub known_uids: &'a std::collections::BTreeSet<String>,
    /// Nombre d'entrées par fichier, pour le plafond de retrait.
    pub entries_per_file: &'a std::collections::BTreeMap<String, usize>,
    /// Fichier de chaque uid, pour rattacher un retrait au bon fichier.
    pub uid_files: &'a std::collections::BTreeMap<String, String>,
    /// uid modifiés à la main depuis le début de la passe : leur opération est **reportée**.
    pub manually_modified: &'a std::collections::BTreeSet<String>,
    /// Pratiques connues.
    pub known_practices: &'a std::collections::BTreeSet<String>,
    /// Date du jour (AAAA-MM-JJ), pour l'expiration des états passagers.
    pub today: &'a str,
}

/// Valide un lot d'opérations.
pub fn validate(
    ops: Vec<Operation>,
    ctx: &ValidationContext<'_>,
    gates: &PromotionGates,
) -> Validation {
    let mut v = Validation {
        applied: Vec::new(),
        proposals: Vec::new(),
        rejected: Vec::new(),
        deferred: Vec::new(),
    };
    let mut retire_count: std::collections::BTreeMap<String, usize> = Default::default();
    let mut queue: std::collections::VecDeque<Operation> = ops.into();

    while let Some(mut op) = queue.pop_front() {
        // Les emprunts sont matérialisés en valeurs possédées : l'opération est ensuite
        // déplacée dans l'un des quatre seaux du rapport.
        let text = op.text().map(String::from);
        let target = op.target_uid().map(String::from);

        // 1. Filtre de contenu interdit (§6.10), y compris pour la consolidation.
        if let Some(t) = &text {
            if penelope_observe::contains_secret(t) {
                let kind = penelope_observe::redact::secret_kind(t).unwrap_or("secret");
                v.rejected.push((op, format!("contenu interdit : {kind}")));
                continue;
            }
            if penelope_observe::is_suspicious(t) {
                v.rejected.push((op, "motif d'injection détecté".into()));
                continue;
            }
        }

        // 1 bis. Porte de qualité (issue #25) : un fait complet, court, au sujet
        //        identifiable ; un état passager part en projet avec une expiration ; une
        //        donnée sensible est marquée ; un fait sur Pénélope n'est pas retenu.
        if let Some(t) = &text {
            use crate::quality::{self, Verdict};
            match &mut op {
                Operation::AddEntry {
                    file,
                    text: body,
                    expire,
                    sensible,
                    ..
                } => {
                    if quality::is_about_penelope(t) {
                        v.rejected.push((
                            op,
                            "fait sur Pénélope : la configuration effective fait foi (self_status)"
                                .into(),
                        ));
                        continue;
                    }
                    match quality::check_text(t) {
                        Verdict::Reject(why) => {
                            v.rejected.push((op, why));
                            continue;
                        }
                        Verdict::Split { parts, dropped } => {
                            for (piece, why) in dropped {
                                let mut rejected = op.clone();
                                if let Operation::AddEntry { text, .. } = &mut rejected {
                                    *text = piece;
                                }
                                v.rejected.push((rejected, why));
                            }
                            for piece in parts.into_iter().rev() {
                                let mut part = op.clone();
                                if let Operation::AddEntry { text, .. } = &mut part {
                                    *text = piece;
                                }
                                queue.push_front(part);
                            }
                            continue;
                        }
                        Verdict::Keep => {}
                    }
                    *body = t.trim().to_string();
                    if quality::is_temporal(t) {
                        *file = "projets.md".into();
                        if expire.is_none() {
                            *expire = Some(quality::expiry_from(ctx.today));
                        }
                    }
                    if quality::is_sensitive(t) {
                        *sensible = Some(true);
                    }
                }
                Operation::ReplaceEntry { .. } | Operation::SupersedeEntry { .. } => {
                    if quality::is_about_penelope(t) {
                        v.rejected.push((
                            op,
                            "fait sur Pénélope : la configuration effective fait foi (self_status)"
                                .into(),
                        ));
                        continue;
                    }
                    let why = match quality::check_text(t) {
                        Verdict::Keep => None,
                        Verdict::Reject(why) => Some(why),
                        Verdict::Split { .. } => Some(format!(
                            "remplacement trop long (maximum {} caractères) : une entrée par fait",
                            quality::MAX_ENTRY_CHARS
                        )),
                    };
                    if let Some(why) = why {
                        v.rejected.push((op, why));
                        continue;
                    }
                }
                _ => {}
            }
        }

        // 2. Conflit avec une édition manuelle : l'opération est **reportée**, jamais
        //    appliquée par-dessus (§6.8).
        if let Some(uid) = &target {
            if ctx.manually_modified.contains(uid) {
                v.deferred.push((
                    op,
                    "entrée modifiée à la main pendant la passe : reportée à la nuit suivante"
                        .into(),
                ));
                continue;
            }
            if !ctx.known_uids.contains(uid) {
                let msg = format!("uid inconnu : {uid}");
                v.rejected.push((op, msg));
                continue;
            }
        }

        // 3. Pratique cible connue.
        let practice = match &op {
            Operation::AddException { practice, .. }
            | Operation::RecordEcart { practice, .. }
            | Operation::UpdateDefault { practice, .. } => Some(practice.clone()),
            _ => None,
        };
        if let Some(p) = practice
            && !ctx.known_practices.contains(&p)
        {
            v.rejected.push((op, format!("pratique inconnue : {p}")));
            continue;
        }

        // 4. Prédicat `quand` valide : une exception sans `quand` valide est invalide.
        let when_ok = match &op {
            Operation::AddException { quand, .. } | Operation::RecordEcart { quand, .. } => {
                When::parse(quand).map_err(|e| e.to_string())
            }
            Operation::UpdateException { quand: Some(q), .. } => {
                When::parse(q).map_err(|e| e.to_string())
            }
            _ => Ok(When::default()),
        };
        if let Err(e) = when_ok {
            v.rejected
                .push((op, format!("prédicat `quand` invalide : {e}")));
            continue;
        }

        // 5. Plafond de retrait : au plus 20 % des entrées d'un fichier par nuit.
        if let Operation::RetireEntry { uid, .. } = &op {
            let file = uid_file(uid, ctx);
            let total = ctx.entries_per_file.get(&file).copied().unwrap_or(0);
            let already = retire_count.entry(file.clone()).or_insert(0);
            let max = ((total as f64) * gates.max_retire_ratio).floor() as usize;
            if total > 0 && *already >= max.max(1) {
                v.rejected.push((
                    op,
                    format!("plafond de retrait atteint pour {file} ({max} sur {total} entrées)"),
                ));
                continue;
            }
            *already += 1;
        }

        // 6. `update_default` devient toujours une proposition.
        if op.requires_hitl() {
            v.proposals.push((
                op,
                "modification d'un défaut : proposée, jamais appliquée seule".into(),
            ));
            continue;
        }

        v.applied.push(op);
    }
    v
}

fn uid_file(uid: &str, ctx: &ValidationContext<'_>) -> String {
    // Le fichier est déterminé par le harnais, pas par le modèle.
    ctx.uid_files
        .get(uid)
        .cloned()
        .unwrap_or_else(|| "inconnu".into())
}

/// Phase d'une passe de consolidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Light,
    Rem,
    Deep,
    Done,
    Failed,
}

impl Phase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Phase::Light => "light",
            Phase::Rem => "rem",
            Phase::Deep => "deep",
            Phase::Done => "done",
            Phase::Failed => "failed",
        }
    }
    pub fn next(&self) -> Phase {
        match self {
            Phase::Light => Phase::Rem,
            Phase::Rem => Phase::Deep,
            Phase::Deep => Phase::Done,
            other => *other,
        }
    }
}

/// Bilan d'une passe, rendu dans le digest du matin (§6.8, sortie).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DreamReport {
    pub candidates_seen: u32,
    pub groups: u32,
    pub promoted: u32,
    pub proposals: u32,
    pub rejected: Vec<String>,
    pub deferred: u32,
    pub reflections: Vec<String>,
    pub questions: Vec<String>,
    pub files_touched: Vec<String>,
    /// Signalements (budget du niveau Cœur dépassé, issue #25).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Entrées ajoutées ou réécrites, en wikilinks `[[note#^uid]]` (issue #29).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promoted_refs: Vec<String>,
    /// Lint du wiki : une ligne par catégorie de problème.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lint: Vec<String>,
    #[serde(default)]
    pub lint_problems: u32,
    /// Tri de la grille : une ligne par candidat, décision, critères et justification
    /// (issue #37).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sorted: Vec<String>,
    /// États passagers mis au journal cette nuit.
    #[serde(default)]
    pub journal: u32,
    /// États passagers expirés, retirés du journal.
    #[serde(default)]
    pub journal_expired: u32,
    /// Secrets rangés dans le magasin, par nom.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    /// Entrées durables jamais rappelées depuis 60 jours, proposées au retrait.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unused: Vec<String>,
    /// Ce que la nuit a appris, en clair : cinq lignes au plus, tronquées, avec leur
    /// fichier. C'est ce que le digest montre à la place des wikilinks (issue #145).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promoted_examples: Vec<String>,
    /// Nettoyage **proposé**, jamais exécuté : entrées au-delà de la borne, avec la
    /// commande qui en propose le découpage (issue #145).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup: Vec<String>,
    /// Appels au modèle de consolidation, dont ceux jetés (sortie coupée) : le coût
    /// d'une passe se voit (issue #135).
    #[serde(default)]
    pub calls: u32,
    #[serde(default)]
    pub wasted_calls: u32,
    /// Lots écrits : opérations appliquées et candidats marqués. Une passe interrompue
    /// garde ce compte, et la suivante reprend sur le reste (issue #152).
    #[serde(default)]
    pub lots: u32,
    /// Durée de la passe, en millisecondes.
    #[serde(default)]
    pub duration_ms: u64,
}

impl DreamReport {
    /// Vrai si la passe n'a rien changé (deux exécutions consécutives sans nouvelle
    /// donnée ne produisent aucun changement, CA 6).
    pub fn is_noop(&self) -> bool {
        self.promoted == 0 && self.proposals == 0 && self.files_touched.is_empty()
    }

    /// Rapport complet, pour `DREAMS.md` : chaque rejet avec son motif.
    pub fn render(&self) -> String {
        let mut s = self.render_brief();
        if !self.rejected.is_empty() {
            s.push_str(&format!(
                "\n{} candidats rejetés : {}",
                self.rejected.len(),
                self.rejected.join(" ; ")
            ));
        }
        s
    }

    /// Ce que le digest du matin en dit : court, lisible sur un téléphone, sans un seul
    /// wikilink brut (issue #145).
    ///
    /// Le digest du 20/09 faisait 11 300 caractères en six messages, dont la moitié en
    /// `[[memoire#^01M2…]]` : les références promues, les avertissements internes de la
    /// passe, le journal, les secrets rangés et le lint ont leur place dans `DREAMS.md`
    /// et le journal du vault, pas sur Telegram. Ce qui reste : ce qui a été appris,
    /// combien de questions attendent, et où lire le détail.
    pub fn render_digest(&self) -> String {
        let mut s = format!(
            "Appris cette nuit : {} entrées promues, {} propositions en attente.",
            self.promoted, self.proposals
        );
        for e in self.promoted_examples.iter().take(5) {
            s.push_str(&format!("\n- {e}"));
        }
        if !self.files_touched.is_empty() {
            s.push_str(&format!("\nFichiers : {}", self.files_touched.join(", ")));
        }
        if !self.questions.is_empty() {
            s.push_str(&format!(
                "\n{} question(s) en attente de ta réponse.",
                self.questions.len()
            ));
        }
        // Un seul avertissement peut demander une action du propriétaire : le Cœur au-delà
        // de son budget, et il porte alors la commande. Les autres (relances, lots coupés)
        // vont au journal du vault et à `penelope doctor`.
        for w in self.warnings.iter().filter(|w| w.contains("Cœur")) {
            s.push_str(&format!(
                "\n⚠️ {w} — `penelope config set memory.core_budget_tokens <n>`"
            ));
        }
        // Nettoyage proposé, jamais lancé tout seul : la commande est dans la ligne.
        if !self.cleanup.is_empty() {
            s.push_str(&format!(
                "\n{} entrée(s) trop longues faussent la consolidation :",
                self.cleanup.len()
            ));
            for c in self.cleanup.iter().take(3) {
                s.push_str(&format!("\n- {c}"));
            }
            if self.cleanup.len() > 3 {
                s.push_str(&format!("\n- … et {} autres", self.cleanup.len() - 3));
            }
        }
        // Ce qui attend une décision : trois exemples, jamais la liste entière.
        if !self.unused.is_empty() {
            s.push_str("\nJamais rappelées depuis 60 jours, à retirer ?");
            for u in self.unused.iter().take(3) {
                s.push_str(&format!("\n- {u}"));
            }
            if self.unused.len() > 3 {
                s.push_str(&format!("\n- … et {} autres", self.unused.len() - 3));
            }
        }
        s
    }

    /// Rapport sans la liste des rejets, pour le digest qui les regroupe par motif
    /// (issue #109) : cinquante rejets ne font pas déborder un message Telegram.
    pub fn render_brief(&self) -> String {
        let mut s = format!(
            "Appris cette nuit : {} entrées promues, {} propositions en attente.",
            self.promoted, self.proposals
        );
        if !self.files_touched.is_empty() {
            s.push_str(&format!("\nFichiers : {}", self.files_touched.join(", ")));
        }
        if !self.questions.is_empty() {
            s.push_str("\nQuestions :");
            for q in &self.questions {
                s.push_str(&format!("\n- {q}"));
            }
        }
        for w in &self.warnings {
            s.push_str(&format!("\nAttention : {w}"));
        }
        if self.calls > 0 {
            s.push_str(&format!(
                "\nPasse : {} appel(s) au modèle{}, {} s.",
                self.calls,
                if self.wasted_calls > 0 {
                    format!(" dont {} jeté(s) (sortie coupée)", self.wasted_calls)
                } else {
                    String::new()
                },
                self.duration_ms / 1000
            ));
        }
        if !self.promoted_refs.is_empty() {
            s.push_str(&format!("\nEntrées : {}", self.promoted_refs.join(", ")));
        }
        if self.journal > 0 || self.journal_expired > 0 {
            s.push_str(&format!(
                "\nJournal : {} état(s) en cours ajouté(s), {} expiré(s) retiré(s).",
                self.journal, self.journal_expired
            ));
        }
        if !self.secrets.is_empty() {
            s.push_str(&format!(
                "\nSecrets rangés dans le magasin : {}",
                self.secrets.join(", ")
            ));
        }
        if !self.unused.is_empty() {
            s.push_str("\nJamais rappelées depuis 60 jours, à retirer ?");
            for u in &self.unused {
                s.push_str(&format!("\n- {u}"));
            }
        }
        if !self.lint.is_empty() {
            s.push_str("\nLint du wiki :");
            for l in &self.lint {
                s.push_str(&format!("\n- {l}"));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests;
