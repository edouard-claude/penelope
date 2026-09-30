//! Sections `[owner]` et `[telegram]` : le propriétaire et son canal.

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Owner {
    /// Identifiant Telegram du propriétaire : le seul compte auquel le bot répond (0 :
    /// canal fermé).
    pub telegram_user_id: i64,
    /// Fuseau horaire du propriétaire : planifications, digest, date du jour.
    pub timezone: String,
    /// Langue des réponses.
    pub language: String,
}

impl Default for Owner {
    fn default() -> Self {
        Owner {
            telegram_user_id: 0,
            timezone: "Indian/Reunion".into(),
            language: "fr".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Telegram {
    /// Jeton du bot, par référence au magasin de secrets.
    pub token: String,
    /// Réception des messages : `polling` (long polling) ou `webhook` (pas encore servi).
    pub mode: String,
    /// Sujets de forum Telegram. Sans effet dans cette version.
    pub topics: bool,
    /// Rendu riche natif de la Bot API plutôt que HTML.
    pub rich_messages: bool,
    /// Heures calmes `HH:MM-HH:MM` : les notifications non urgentes attendent la fin de la
    /// plage.
    pub quiet_hours: String,
    /// Adresse de la Bot API.
    pub api_base: String,
    /// Attente d'un appel `getUpdates` en long polling, en secondes.
    pub poll_timeout_s: u64,
    /// Messages envoyés au plus par seconde, par chat.
    pub rate_per_chat_per_s: f64,
    /// Taille maximale d'un message. Sans effet dans cette version.
    pub text_limit: usize,
    /// Taille maximale d'une légende. Sans effet dans cette version.
    pub caption_limit: usize,
    /// Fragments au-delà desquels un avis interne (digest du matin, rapport de veille)
    /// part en document plutôt qu'en chapelet de messages (issue #145). 0 : jamais.
    pub max_fragments: usize,
    /// Intervalle entre deux mises à jour du brouillon de réponse, en millisecondes (300 au
    /// moins).
    pub draft_interval_ms: u64,
    /// Adresse du webhook. Sans effet dans cette version.
    pub webhook_url: String,
    /// Ancien interrupteur des groupes, sans effet depuis 0.17.4 : un groupe s'ouvre en
    /// ajoutant son identifiant à `telegram.allowed_chats`.
    pub allow_groups: bool,
    /// Conversations de groupe autorisées, par identifiant (`-100…` pour un supergroupe) :
    /// le propriétaire y parle, y compris en administrateur anonyme ; un sujet donne une
    /// session. `penelope doctor` liste les conversations refusées récemment avec leur
    /// identifiant.
    pub allowed_chats: Vec<i64>,
    /// Attente après un morceau qui ressemble à une coupure de Telegram (4 000 caractères
    /// ou plus) ou un message transféré, en millisecondes : les morceaux d'un même envoi
    /// forment un seul tour. Un message court tapé part tout de suite. 0 : un message, un
    /// tour.
    pub text_group_window_ms: u64,
    /// Messages regroupés à partir desquels Pénélope demande quoi en faire au lieu de
    /// répondre à chacun. 0 : jamais.
    pub burst_messages: usize,
    /// Caractères cumulés à partir desquels elle demande de même. 0 : jamais.
    pub burst_chars: usize,
    /// Foyer du propriétaire : le chat (et le sujet) où arrivent les notifications qui
    /// n'appartiennent à aucune session — alertes de budget, rappels, digest du rêve,
    /// veille, cartes OAuth, élicitations sans conversation. Vide : le chat privé.
    pub home: TelegramHome,
    /// Trace des outils d'un tour (issue #222) : une bulle éditée en place qui groupe les
    /// appels consécutifs (« 💻 shell_exec · echo test (×4) ✅ »). `off` (aucune bulle),
    /// `compact` (l'outil et son argument principal, sans argument dans un groupe),
    /// `full` (plus un extrait du résultat, en privé seulement), `resume` (une ligne par
    /// familles et verbes, « 📄 6 lectures, 7 recherches · 💻 `cargo test` en cours »,
    /// sans modèle), `narre` (une phrase de 4 à 8 mots et un emoji, écrite par le modèle
    /// du rôle `trace`, par défaut un alias `local:` ; sans modèle joignable, `resume`)
    /// (#273). Arguments et extraits sont caviardés.
    pub tool_trace: ToolTrace,
}

/// Trace des outils d'un tour sur Telegram (issue #222 ; `resume` et `narre` : #273).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolTrace {
    Off,
    /// Par défaut : le propriétaire voit ce qui tourne sans ouvrir `penelope logs`, et
    /// l'extrait du résultat, plus bavard, reste à demander.
    #[default]
    Compact,
    Full,
    /// Une ligne déterministe, par famille et par verbe, sans appel de modèle. C'est
    /// aussi le repli de `narre`.
    Resume,
    /// Une phrase courte avec un emoji, écrite par le modèle du rôle `trace` à chaque
    /// modification de la bulle ; `resume` quand le modèle manque, tarde ou déraille.
    Narre,
}

impl ToolTrace {
    /// Les valeurs admises, dans l'ordre du moins au plus bavard puis les deux modes
    /// résumés : pour les messages qui les listent.
    pub const ALL: &'static [&'static str] = &["off", "compact", "full", "resume", "narre"];

    /// Le mode passe par le modèle du rôle `trace`.
    pub fn narrates(self) -> bool {
        matches!(self, ToolTrace::Narre)
    }

    /// La bulle tient en une ligne résumée (modèle ou non) plutôt qu'en une liste.
    pub fn summarises(self) -> bool {
        matches!(self, ToolTrace::Resume | ToolTrace::Narre)
    }
}

/// Foyer du propriétaire sur Telegram (issue #143) : le chat privé n'est plus lu dès que
/// la conversation vit dans un groupe à sujets.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct TelegramHome {
    /// Identifiant du chat (un groupe : `-100…`). 0 : le chat privé du propriétaire.
    pub chat: i64,
    /// Sujet du groupe (`message_thread_id`). 0 : le sujet « Général ».
    pub topic: i64,
}

impl TelegramHome {
    /// Foyer réglé, sous la forme attendue par l'envoi.
    pub fn resolved(&self) -> Option<(i64, Option<i64>)> {
        (self.chat != 0).then(|| (self.chat, (self.topic != 0).then_some(self.topic)))
    }
}

impl Default for Telegram {
    fn default() -> Self {
        Telegram {
            token: "${SECRET:telegram_bot_token}".into(),
            mode: "polling".into(),
            topics: true,
            // HTML par défaut : `sendMessage` + `parse_mode` marche sur toutes les versions
            // de la Bot API. Le rendu riche natif est une option à activer.
            rich_messages: false,
            quiet_hours: "22:00-07:00".into(),
            api_base: "https://api.telegram.org".into(),
            poll_timeout_s: 50,
            rate_per_chat_per_s: 1.0,
            text_limit: 4096,
            caption_limit: 1024,
            max_fragments: 3,
            draft_interval_ms: 700,
            webhook_url: String::new(),
            allow_groups: false,
            allowed_chats: Vec::new(),
            text_group_window_ms: 2_000,
            burst_messages: 5,
            burst_chars: 20_000,
            home: TelegramHome::default(),
            tool_trace: ToolTrace::default(),
        }
    }
}
