//! Photos, albums, documents et notes vocales reçus.

use super::*;

/// Photos d'un même album, regroupées en un seul tour (§14.4).
pub(super) struct Album {
    origin: Origin,
    images: Vec<std::path::PathBuf>,
    caption: Option<String>,
    update_id: i64,
}

/// Fenêtre de regroupement d'un album : Telegram envoie ses photos une par une.
pub(super) const ALBUM_WINDOW: Duration = Duration::from_millis(1_500);

impl TelegramGateway {
    /// Vocal ou fichier audio (§14.4) : téléchargement, transcription par le rôle `stt`,
    /// texte montré en citation, puis traité comme un message tapé.
    /// Photo : enregistrée, puis confiée au tour (vision, §10.4). Les photos d'un album
    /// attendent leurs voisines pendant [`ALBUM_WINDOW`] et partent ensemble.
    pub(super) async fn photo(&self, incoming: Incoming) -> anyhow::Result<()> {
        let Incoming::Photo {
            update_id,
            chat_id,
            message_id,
            topic_id,
            file_ids,
            file_size,
            media_group,
            caption,
            ..
        } = incoming
        else {
            return Ok(());
        };
        let reply_to = Some(message_id);
        let Some(file_id) = file_ids.last() else {
            return Ok(());
        };
        if file_size.unwrap_or(0) as usize > penelope_app::media::IMAGE_MAX_BYTES {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    reply_to,
                    "📷 Photo trop lourde (10 Mo au plus).",
                )
                .await;
        }
        self.react(chat_id, message_id, reaction::RECEIVED);
        let saved = match self.bot.download_file(file_id).await {
            Ok((bytes, _)) => penelope_app::media::save_photo(&self.daemon.services, &bytes),
            Err(e) => Err(format!("téléchargement impossible : {e}")),
        };
        let path = match saved {
            Ok(p) => p,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        reply_to,
                        &format!("📷 Photo ignorée : {e}"),
                    )
                    .await;
            }
        };
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let Some(group) = media_group else {
            return enqueue_photos(&self.daemon, &origin, vec![path], caption, update_id).await;
        };
        let first = {
            let mut albums = self
                .albums
                .lock()
                .map_err(|_| anyhow::anyhow!("albums verrouillés"))?;
            match albums.get_mut(&group) {
                Some(album) => {
                    album.images.push(path);
                    if album.caption.is_none() {
                        album.caption = caption;
                    }
                    false
                }
                None => {
                    albums.insert(
                        group.clone(),
                        Album {
                            origin,
                            images: vec![path],
                            caption,
                            update_id,
                        },
                    );
                    true
                }
            }
        };
        if first {
            let (daemon, albums) = (self.daemon.clone(), self.albums.clone());
            tokio::spawn(async move {
                tokio::time::sleep(ALBUM_WINDOW).await;
                let album = albums.lock().ok().and_then(|mut g| g.remove(&group));
                if let Some(a) = album
                    && let Err(e) =
                        enqueue_photos(&daemon, &a.origin, a.images, a.caption, a.update_id).await
                {
                    tracing::warn!(error = %e, "album Telegram non transmis");
                }
            });
        }
        Ok(())
    }
    /// Document : ingéré dans le vault s'il est lisible (§6.13), sinon rangé comme pièce
    /// jointe. Une légende est une demande : elle part en tour, document joint. `/mien` en
    /// tête de légende déclare un document rédigé par le propriétaire.
    pub(super) async fn document(&self, incoming: Incoming) -> anyhow::Result<()> {
        let Incoming::Document {
            update_id,
            chat_id,
            message_id,
            topic_id,
            file_id,
            file_name,
            file_size,
            caption,
            ..
        } = incoming
        else {
            return Ok(());
        };
        let reply_to = Some(message_id);
        if file_size.unwrap_or(0) as usize > penelope_telegram::api::DOWNLOAD_MAX_BYTES {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    reply_to,
                    "📄 Fichier trop gros : un bot Telegram ne télécharge pas plus de 20 Mo. \
                     Le déposer dans `vault/inbox/` fonctionne aussi.",
                )
                .await;
        }
        self.react(chat_id, message_id, reaction::RECEIVED);
        let bytes = match self.bot.download_file(&file_id).await {
            Ok((b, _)) => b,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        reply_to,
                        &format!("📄 Téléchargement impossible : {e}"),
                    )
                    .await;
            }
        };
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let session = self.daemon.chat_session_for(&origin).await?;
        let caption = caption.unwrap_or_default();
        let caption = caption.trim();
        let (owner, request) = match caption.strip_prefix("/mien") {
            Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
                (true, rest.trim().to_string())
            }
            _ => (false, caption.to_string()),
        };
        // L'ingestion appelle le modèle : la file des updates n'attend pas.
        let daemon = self.daemon.clone();
        tokio::spawn(async move {
            let dedup = Some(format!("tg:{update_id}"));
            let messenger = daemon.hooks.messenger();
            let say = |text: String| {
                let (m, o) = (messenger.clone(), origin.clone());
                async move {
                    if let Some(m) = m {
                        let _ = m.send_text(&o, &text).await;
                    }
                }
            };
            if penelope_memory::ingest::is_ingestible(&file_name) {
                let trust = if owner {
                    penelope_memory::Origin::Owner
                } else {
                    penelope_memory::Origin::Untrusted
                };
                // L'ingestion est déclarée pour cette session : `/stop tout` peut
                // l'interrompre (issue #155). Le jeton est retiré quoi qu'il arrive.
                let (ingest_id, cancel) = daemon.bus.start_ingest(&session);
                let outcome = penelope_dream::ingest::ingest(
                    &daemon.dream(),
                    &file_name,
                    bytes,
                    "telegram",
                    trust,
                    Some(&session),
                    &cancel,
                )
                .await;
                daemon.bus.end_ingest(&session, ingest_id);
                match outcome {
                    Ok(doc) => {
                        say(doc.report()).await;
                        if let (Some(m), Some(id)) = (&messenger, &doc.approval_id) {
                            let _ = m.send_approval(&origin, id).await;
                        }
                        if !request.is_empty() {
                            let text = doc.turn_text(&request);
                            if let Err(e) = daemon
                                .enqueue_message(&session, &text, &origin, dedup)
                                .await
                            {
                                tracing::warn!(error = %e, "demande sur document non transmise");
                            }
                        }
                    }
                    Err(e) => say(format!("📄 `{file_name}` non ingéré : {e}")).await,
                }
                return;
            }
            let (note, joined) = store_attachment(&daemon, &session, &file_name, &bytes).await;
            say(note).await;
            if let (false, Some(joined)) = (request.is_empty(), joined) {
                let text = format!("{request}\n\n{joined}");
                if let Err(e) = daemon
                    .enqueue_message(&session, &text, &origin, dedup)
                    .await
                {
                    tracing::warn!(error = %e, "demande sur pièce jointe non transmise");
                }
            }
        });
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn voice(
        &self,
        update_id: i64,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        file_id: &str,
        file_name: Option<&str>,
        mime_type: Option<&str>,
        file_size: Option<i64>,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        if file_size.unwrap_or(0) as usize > penelope_telegram::api::DOWNLOAD_MAX_BYTES {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    reply_to,
                    "🎙️ Fichier trop gros : un bot Telegram ne télécharge pas plus de 20 Mo.",
                )
                .await;
        }
        self.react(chat_id, message_id, reaction::RECEIVED);
        let (audio, path) = match self.bot.download_file(file_id).await {
            Ok(x) => x,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        reply_to,
                        &format!("🎙️ Téléchargement du vocal impossible : {e}"),
                    )
                    .await;
            }
        };
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let session = self.daemon.chat_session_for(&origin).await?;
        let filename = audio_filename(&path, file_name, mime_type);
        let text = match self.daemon.transcribe(audio, &filename, &session).await {
            Ok(t) => t,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        reply_to,
                        &format!("🎙️ Transcription impossible : {e}"),
                    )
                    .await;
            }
        };
        if text.trim().is_empty() {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    reply_to,
                    "🎙️ Rien d'audible dans ce vocal.",
                )
                .await;
        }
        let quoted: String = text
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        self.reply(chat_id, topic_id, reply_to, &format!("🎙️\n{quoted}"))
            .await?;
        self.daemon
            .enqueue_message(
                &session,
                &if self.daemon.services.config.config().voice.reply_in_kind {
                    format!(
                        "(message vocal transcrit ; réponds en vocal avec `send_voice` si la \
                         réponse s'y prête) {text}"
                    )
                } else {
                    format!("(message vocal transcrit) {text}")
                },
                &origin,
                Some(format!("tg:{update_id}")),
            )
            .await?;
        Ok(())
    }
}

/// Met les photos reçues en file, dans la session de la conversation.
async fn enqueue_photos(
    daemon: &Arc<Daemon>,
    origin: &Origin,
    images: Vec<std::path::PathBuf>,
    caption: Option<String>,
    update_id: i64,
) -> anyhow::Result<()> {
    let session = daemon.chat_session_for(origin).await?;
    daemon
        .enqueue_message_with_images(
            &session,
            caption.as_deref().unwrap_or_default(),
            &images,
            origin,
            Some(format!("tg:{update_id}")),
        )
        .await?;
    Ok(())
}

/// Range une pièce jointe non ingérable. Texte : artefact lisible par `artifact_read` ;
/// binaire : fichier dans le workspace. Renvoie le bilan et la mention à joindre au tour.
async fn store_attachment(
    daemon: &Arc<Daemon>,
    session: &str,
    name: &str,
    bytes: &[u8],
) -> (String, Option<String>) {
    let s = &daemon.services;
    let text = std::str::from_utf8(bytes)
        .ok()
        .filter(|t| bytes.len() <= 1024 * 1024 && !t.contains('\0'));
    if let Some(text) = text {
        let kind = penelope_context::store::guess_kind(text);
        let stored = s
            .context
            .history
            .put_artifact(Some(session), None, kind, Some(name), text)
            .await;
        return match stored {
            Ok(a) => (
                format!("📎 `{name}` enregistré comme artefact `{}`.", a.id),
                Some(format!(
                    "[Fichier joint : `{name}`, artefact `{}` ({} caractères) : `artifact_read` \
                     pour le lire. Contenu non vérifié.]",
                    a.id,
                    text.chars().count()
                )),
            ),
            Err(e) => (format!("📎 `{name}` non enregistré : {e}"), None),
        };
    }
    match penelope_app::media::save_attachment(s, name, bytes) {
        Ok(path) => (
            format!("📎 `{name}` déposé dans `{}`.", path.display()),
            Some(format!(
                "[Fichier joint : `{name}`, déposé dans `{}` ({} octets).]",
                path.display(),
                bytes.len()
            )),
        ),
        Err(e) => (format!("📎 `{name}` non enregistré : {e}"), None),
    }
}

/// Nom de fichier à transmettre au serveur de transcription : l'extension y dit le
/// format. Les vocaux Telegram (`.oga`, Opus dans Ogg) deviennent `.ogg`, que les
/// serveurs OpenAI-compatibles reconnaissent.
pub(super) fn audio_filename(
    file_path: &str,
    file_name: Option<&str>,
    mime_type: Option<&str>,
) -> String {
    let source = file_name.unwrap_or(file_path);
    let ext = std::path::Path::new(source)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase());
    let ext = match (ext.as_deref(), mime_type) {
        (Some("oga") | Some("opus"), _) => "ogg".to_string(),
        (Some(e), _) if !e.is_empty() => e.to_string(),
        (_, Some("audio/mpeg")) => "mp3".into(),
        (_, Some("audio/mp4") | Some("audio/x-m4a") | Some("audio/m4a")) => "m4a".into(),
        (_, Some("audio/wav") | Some("audio/x-wav")) => "wav".into(),
        (_, Some("audio/flac")) => "flac".into(),
        _ => "ogg".into(),
    };
    format!("audio.{ext}")
}
