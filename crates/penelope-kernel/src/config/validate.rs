//! Lecture, validation et accès par chemin de la configuration.

use super::*;

impl Config {
    /// Lecture **tolérante** (issue #76) : une clé que ce binaire ne connaît pas (écrite
    /// par une version plus récente, ou faute de frappe) est ignorée et renvoyée pour être
    /// signalée par `doctor`, `config validate` et le démarrage. Un retour arrière ne
    /// dépend ainsi d'aucun état écrit par la version que l'on quitte. Une valeur mal
    /// typée reste une erreur.
    pub fn parse(s: &str) -> Result<(Config, Vec<String>)> {
        let cfg: Config = toml::from_str(s).map_err(|e| KernelError::config(e.to_string()))?;
        let raw: toml::Table = toml::from_str(s).map_err(|e| KernelError::config(e.to_string()))?;
        let known = toml::Value::try_from(&cfg).map_err(|e| KernelError::config(e.to_string()))?;
        let mut unknown = Vec::new();
        if let Some(known) = known.as_table() {
            unknown_keys("", &raw, known, &mut unknown);
        }
        Ok((cfg, unknown))
    }

    /// Lecture **stricte** : toute clé inconnue est une erreur. Pour une saisie, pas pour
    /// le fichier qu'une autre version a pu écrire.
    pub fn from_toml(s: &str) -> Result<Config> {
        let (cfg, unknown) = Config::parse(s)?;
        if !unknown.is_empty() {
            return Err(KernelError::config(format!(
                "clé inconnue : {}",
                unknown.join(", ")
            )));
        }
        Ok(cfg)
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| KernelError::config(e.to_string()))
    }

    /// Fichier de premier démarrage : seules les clés qui diffèrent des valeurs par
    /// défaut, sous un en-tête qui dit où trouver les autres (#76). Un fichier court
    /// suit les défauts des versions suivantes et se relit par une version antérieure.
    pub fn sample_toml(owner_id: i64) -> Result<String> {
        let body = edit_toml("", &Config::default(), &Config::sample(owner_id))?;
        Ok(format!("{SAMPLE_HEADER}{body}"))
    }

    /// Validation sémantique, au-delà du typage TOML (§4.4 étape 2).
    #[allow(clippy::too_many_lines)] // gel 0.17 : validation clé par clé
    pub fn validate(&self) -> Result<()> {
        if self.owner.telegram_user_id == 0 {
            return Err(KernelError::config(
                "owner.telegram_user_id est obligatoire (aucun propriétaire = bot ouvert)",
            ));
        }
        self.owner.timezone.parse::<chrono_tz::Tz>().map_err(|_| {
            KernelError::config(format!("fuseau inconnu : {}", self.owner.timezone))
        })?;

        if !self.observability.runtime_consumers.is_empty() {
            let bind = self
                .observability
                .runtime_stream_bind
                .parse::<std::net::SocketAddr>()
                .map_err(|_| KernelError::config("observability.runtime_stream_bind invalide"))?;
            if bind.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) || bind.port() == 0
            {
                return Err(KernelError::config(
                    "observability.runtime_stream_bind doit être 127.0.0.1 avec un port non nul",
                ));
            }
            let mut names = std::collections::BTreeSet::new();
            for consumer in &self.observability.runtime_consumers {
                if consumer.name.trim().is_empty()
                    || !names.insert(&consumer.name)
                    || consumer.token_secret.trim().is_empty()
                {
                    return Err(KernelError::config(
                        "chaque consommateur runtime doit avoir un nom unique et un token_secret",
                    ));
                }
            }
        }

        if !matches!(self.telegram.mode.as_str(), "polling" | "webhook") {
            return Err(KernelError::config(
                "telegram.mode doit valoir `polling` ou `webhook`",
            ));
        }
        if self.telegram.mode == "webhook" && self.telegram.webhook_url.is_empty() {
            return Err(KernelError::config(
                "telegram.webhook_url est requis en mode webhook",
            ));
        }
        if !self.telegram.quiet_hours.is_empty() {
            TimeRange::parse(&self.telegram.quiet_hours)?;
        }

        for (role, alias) in &self.models.roles {
            if !self.models.aliases.contains_key(alias) {
                return Err(KernelError::config(format!(
                    "models.roles.{role} pointe vers l'alias inconnu `{alias}`"
                )));
            }
        }
        for target in [
            &self.models.routing.low,
            &self.models.routing.medium,
            &self.models.routing.high,
        ] {
            if !self.models.aliases.contains_key(target) {
                return Err(KernelError::config(format!(
                    "models.routing référence l'alias inconnu `{target}`"
                )));
            }
        }
        for (from, chain) in &self.models.routing.fallback {
            if !self.models.aliases.contains_key(from) {
                return Err(KernelError::config(format!(
                    "models.routing.fallback : alias source inconnu `{from}`"
                )));
            }
            for to in chain {
                if !self.models.aliases.contains_key(to) {
                    return Err(KernelError::config(format!(
                        "models.routing.fallback.{from} : alias cible inconnu `{to}`"
                    )));
                }
            }
        }
        for (alias, id) in &self.models.aliases {
            check_model_id(id)
                .map_err(|e| KernelError::config(format!("alias `{alias}` : {e}")))?;
        }

        if !LOCATE_FRAMES.contains(&self.models.locate_frame.as_str()) {
            return Err(KernelError::config(format!(
                "models.locate_frame doit valoir {} (reçu `{}`)",
                LOCATE_FRAMES.join(", "),
                self.models.locate_frame
            )));
        }
        if !APPROVAL_MODES.contains(&self.tools.approval_mode.as_str()) {
            return Err(KernelError::config(format!(
                "tools.approval_mode doit valoir {} (reçu `{}`)",
                APPROVAL_MODES.join(", "),
                self.tools.approval_mode
            )));
        }
        if !(0.1..=0.95).contains(&self.context.compaction_threshold) {
            return Err(KernelError::config(
                "context.compaction_threshold doit être entre 0.1 et 0.95",
            ));
        }
        if self.context.tail_min_tokens > self.context.tail_max_tokens {
            return Err(KernelError::config(
                "context.tail_min_tokens > context.tail_max_tokens",
            ));
        }
        if self.context.max_prompt_tokens != 0 && self.context.max_prompt_tokens < 20_000 {
            return Err(KernelError::config(
                "context.max_prompt_tokens doit valoir 0 (aucun plafond) ou au moins 20000 : \
                 en dessous, la queue verbatim et le préfixe ne tiennent plus",
            ));
        }
        if !(0.0..=1.0).contains(&self.context.max_tool_result_share) {
            return Err(KernelError::config(
                "context.max_tool_result_share doit être entre 0 et 1",
            ));
        }

        parse_duration(&self.memory.episode_idle)?;
        parse_duration(&self.memory.dream_retry_wait)?;
        // Une valeur mal écrite éteindrait ou garderait le raisonnement au hasard : elle
        // est refusée ici plutôt que devinée à 3 h du matin (issue #152).
        if !matches!(self.memory.consolidation_reasoning.as_str(), "auto" | "off") {
            return Err(KernelError::config(format!(
                "memory.consolidation_reasoning : `{}` inconnu (attendu `auto` ou `off`)",
                self.memory.consolidation_reasoning
            )));
        }
        parse_duration(&self.memory.vault_git_autocommit)?;
        parse_duration(&self.memory.intents.cooldown)?;
        parse_duration(&self.memory.intents.expiry)?;
        parse_duration(&self.mcp.default_timeout)?;
        parse_duration(&self.mcp.idle_timeout)?;
        parse_duration(&self.providers.openrouter.catalog_refresh)?;
        parse_duration(&self.providers.openrouter.stream_idle_timeout)?;
        parse_duration(&self.providers.local.stream_idle_timeout)?;
        parse_duration(&self.providers.codex.stream_idle_timeout)?;
        for (field, value) in [
            ("quota_alert_ratio", self.providers.codex.quota_alert_ratio),
            ("quota_stop_ratio", self.providers.codex.quota_stop_ratio),
        ] {
            if !(0.0..=1.0).contains(&value) {
                return Err(KernelError::config(format!(
                    "providers.codex.{field} doit être entre 0 et 1 (reçu {value})"
                )));
            }
        }
        if self.providers.codex.quota_alert_ratio > self.providers.codex.quota_stop_ratio {
            return Err(KernelError::config(
                "providers.codex.quota_alert_ratio doit valoir au plus quota_stop_ratio : \
                 alerter après s'être mis en retrait n'avertit de rien",
            ));
        }
        let lease_ttl = parse_duration(&self.runners.lease_ttl)?;
        let heartbeat = parse_duration(&self.runners.heartbeat)?;
        // Il faut au moins deux battements par bail : sinon un tour un peu long expire et
        // un autre runner le reprend alors qu'il tourne encore (#43).
        if heartbeat * 2 > lease_ttl {
            return Err(KernelError::config(format!(
                "runners.heartbeat ({}) doit valoir au plus la moitié de runners.lease_ttl \
                 ({}) : au-delà, un tour en cours perd son bail et part en double",
                self.runners.heartbeat, self.runners.lease_ttl
            )));
        }
        parse_duration(&self.tools.shell_timeout)?;
        parse_duration(&self.tools.background_after)?;
        // Un plafond nul interdirait tout job sans le dire (issue #204) ; un plafond
        // global sous celui d'une session promettrait à chaque session ce que le daemon
        // ne peut pas tenir.
        if self.tools.jobs_per_session == 0 {
            return Err(KernelError::config(
                "tools.jobs_per_session doit valoir au moins 1",
            ));
        }
        if self.tools.jobs_total < self.tools.jobs_per_session {
            return Err(KernelError::config(format!(
                "tools.jobs_total ({}) doit valoir au moins tools.jobs_per_session ({})",
                self.tools.jobs_total, self.tools.jobs_per_session
            )));
        }
        crate::cron::Cron::parse(&self.memory.dreaming_cron)?;
        if !self.backup.cron.trim().is_empty() {
            crate::cron::Cron::parse(&self.backup.cron)?;
        }
        crate::cron::Cron::parse(&self.memory.digest_cron)?;

        if !matches!(self.mcp.registry_mode.as_str(), "lazy" | "eager") {
            return Err(KernelError::config(
                "mcp.registry_mode doit valoir `lazy` ou `eager`",
            ));
        }
        if !matches!(
            self.mcp.oauth_redirect_mode.as_str(),
            "paste_back" | "public_callback"
        ) {
            return Err(KernelError::config(
                "mcp.oauth_redirect_mode doit valoir `paste_back` ou `public_callback`",
            ));
        }
        if self.mcp.oauth_redirect_mode == "public_callback"
            && self.mcp.public_callback_url.is_empty()
        {
            return Err(KernelError::config(
                "mcp.public_callback_url est requis en mode public_callback",
            ));
        }
        // Un autre hôte ne serait pas une adresse de bouclage : le code d'autorisation
        // partirait sur le réseau.
        if !matches!(self.mcp.callback_host.as_str(), "127.0.0.1" | "localhost") {
            return Err(KernelError::config(
                "mcp.callback_host doit valoir `127.0.0.1` ou `localhost`",
            ));
        }
        for (k, v) in [
            ("read", &self.mcp.policy.read),
            ("write", &self.mcp.policy.write),
            ("destructive", &self.mcp.policy.destructive),
            ("external", &self.mcp.policy.external),
            ("unknown", &self.mcp.policy.unknown),
        ] {
            if !matches!(v.as_str(), "auto" | "ask" | "ask_twice" | "deny") {
                return Err(KernelError::config(format!(
                    "mcp.policy.{k} : valeur inconnue `{v}`"
                )));
            }
        }
        if self.runners.count == 0 || self.runners.count > 64 {
            return Err(KernelError::config("runners.count doit être entre 1 et 64"));
        }
        if !matches!(
            self.sandbox.default_profile.as_str(),
            "readonly" | "workspace-write" | "mcp-stdio" | "full"
        ) {
            return Err(KernelError::config(format!(
                "sandbox.default_profile inconnu : `{}`",
                self.sandbox.default_profile
            )));
        }
        if self.budget.alert_ratio <= 0.0 || self.budget.alert_ratio > 1.0 {
            return Err(KernelError::config(
                "budget.alert_ratio doit être dans ]0, 1]",
            ));
        }
        Ok(())
    }

    /// Alias effectif d'un rôle, avec repli sur `chat_default` puis `main`.
    pub fn role_alias(&self, role: &str) -> String {
        self.models
            .roles
            .get(role)
            .or_else(|| self.models.roles.get("chat_default"))
            .cloned()
            .unwrap_or_else(|| "main".to_string())
    }

    /// Identifiant `provider:model` d'un alias.
    pub fn alias_model(&self, alias: &str) -> Option<&str> {
        self.models.aliases.get(alias).map(|s| s.as_str())
    }

    /// Seuil de compaction effectif pour un modèle (surcharge §19 `context.model_thresholds`).
    pub fn compaction_threshold_for(&self, model_id: &str) -> f64 {
        self.model_threshold(model_id)
            .unwrap_or(self.context.compaction_threshold)
    }

    /// Seuil propre au modèle, s'il y en a un (identifiant avec ou sans provider).
    pub fn model_threshold(&self, model_id: &str) -> Option<f64> {
        let bare = model_id.split_once(':').map(|(_, m)| m).unwrap_or(model_id);
        self.context
            .model_thresholds
            .get(model_id)
            .or_else(|| self.context.model_thresholds.get(bare))
            .copied()
    }

    pub fn quiet_range(&self) -> Option<TimeRange> {
        TimeRange::parse(&self.telegram.quiet_hours).ok()
    }

    /// Configuration minimale valide, utilisée par les tests et `penelope init`.
    pub fn sample(owner_id: i64) -> Config {
        Config {
            owner: Owner {
                telegram_user_id: owner_id,
                ..Default::default()
            },
            ..Default::default()
        }
    }
}
