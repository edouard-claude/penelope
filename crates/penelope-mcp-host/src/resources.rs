//! Ressources d'un serveur (#293) : abonnement, lecture, et la notification
//! `resources/updated` remise au journal pour l'ordonnanceur.
//!
//! ```text
//! ordonnanceur ──► subscribe_resource(serveur, uri) ─► connexion vivante, posée au besoin
//!                    │   abonnée sur cette connexion ? ─► oui : rien ; non : resources/subscribe
//!                    └─► Ok(false) si le serveur ne déclare pas resources.subscribe
//! serveur ───────► notifications/resources/updated ─► événement `mcp.resource_updated`
//! ordonnanceur ──► read_resource(serveur, uri) ─────► resources/read, contenus en JSON
//! ```
//!
//! Les abonnements vivent dans `Live` : une reconnexion repart sans, et l'ordonnanceur,
//! qui redemande à chaque passage, les repose.

use super::*;

impl McpSupervisor {
    /// Abonne la ressource `uri` du serveur `server`. `Ok(false)` : le serveur ne sait pas
    /// s'abonner (capacité absente, ou méthode inconnue), l'appelant sondera.
    pub async fn subscribe_resource(&self, server: &str, uri: &str) -> Result<bool, String> {
        let slot = self.slot(server).await.ok_or_else(|| unknown(server))?;
        let client = self.ensure_live(&slot).await?;
        if !client.capabilities().resources_subscribe {
            return Ok(false);
        }
        // Déjà posé sur cette connexion : rien à envoyer, et `last_used` reste tel quel pour
        // que la sonde de santé continue de vérifier une connexion qu'on n'utilise pas.
        if slot
            .live
            .lock()
            .await
            .as_ref()
            .is_some_and(|l| l.subscriptions.lock().is_ok_and(|s| s.contains(uri)))
        {
            return Ok(true);
        }
        // 2026-07-28 : les notifications s'ouvrent par `subscriptions/listen` ; un refus
        // n'empêche pas d'essayer l'abonnement lui-même.
        if client.version().is_stateless_core()
            && let Err(e) = client.listen_changes().await
        {
            tracing::debug!(server, error = %e, "subscriptions/listen refusé");
        }
        let now = self.now_ms();
        match client.subscribe_resource(uri).await {
            Ok(()) => {}
            Err(McpError::Rpc { code, .. }) if code == penelope_mcp::protocol::METHOD_NOT_FOUND => {
                return Ok(false);
            }
            Err(e) => {
                slot.info(|i| i.metrics.record(0.0, false));
                if matches!(e, McpError::Transport(_)) {
                    self.connection_lost(&slot, &e).await;
                }
                return Err(format!("abonnement à `{uri}` refusé par `{server}` : {e}"));
            }
        }
        if let Some(l) = slot.live.lock().await.as_ref()
            && let Ok(mut s) = l.subscriptions.lock()
        {
            s.insert(uri.to_string());
        }
        slot.info(|i| i.last_used_ms = now);
        tracing::info!(server, uri, "ressource MCP abonnée");
        Ok(true)
    }

    /// Lit une ressource : `{"contents": [{uri, mimeType, text|blob}, …]}`.
    pub async fn read_resource(&self, server: &str, uri: &str) -> Result<Value, String> {
        let slot = self.slot(server).await.ok_or_else(|| unknown(server))?;
        let client = self.ensure_live(&slot).await?;
        let started = std::time::Instant::now();
        let outcome = client.read_resource(uri).await;
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let now = self.now_ms();
        slot.info(|i| {
            i.metrics.record(ms, outcome.is_ok());
            i.last_used_ms = now;
        });
        match outcome {
            Ok(contents) => Ok(json!({
                "contents": contents.iter().map(content_json).collect::<Vec<_>>()
            })),
            Err(e) => {
                if matches!(e, McpError::Transport(_)) {
                    self.connection_lost(&slot, &e).await;
                }
                Err(format!(
                    "lecture de `{uri}` sur `{server}` : {}",
                    call_error(&e)
                ))
            }
        }
    }

    /// Le serveur signale qu'une ressource abonnée a changé : au journal, où
    /// l'ordonnanceur la verra à son passage.
    pub(super) async fn resource_updated(&self, server: &str, uri: &str) {
        tracing::debug!(server, uri, "ressource MCP mise à jour");
        if let Err(e) = self
            .services
            .events
            .append(penelope_kernel::event::EventDraft::new(
                "mcp.resource_updated",
                json!({"server": server, "uri": uri}),
            ))
            .await
        {
            tracing::warn!(server, uri, error = %e, "notification de ressource non journalisée");
        }
    }
}

/// Un contenu de `resources/read` tel que le protocole l'écrit.
fn content_json(c: &ContentBlock) -> Value {
    match c {
        ContentBlock::Resource {
            uri,
            text,
            blob,
            mime_type,
        } => {
            let mut v = json!({"uri": uri});
            if let Some(m) = mime_type {
                v["mimeType"] = json!(m);
            }
            if let Some(t) = text {
                v["text"] = json!(t);
            }
            if let Some(b) = blob {
                v["blob"] = json!(b);
            }
            v
        }
        ContentBlock::Text { text } => json!({"text": text}),
        ContentBlock::ResourceLink {
            uri,
            name,
            description,
        } => json!({"uri": uri, "name": name, "description": description}),
        ContentBlock::Image { data, mime_type } | ContentBlock::Audio { data, mime_type } => {
            json!({"mimeType": mime_type, "blob": data})
        }
        ContentBlock::Other(v) => v.clone(),
    }
}
