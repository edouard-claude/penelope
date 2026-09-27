//! Réponses vocales (issue #41) : synthèse locale (rôle `tts`), conversion en OGG/Opus et
//! envoi en message vocal dans la conversation d'origine.
//!
//! ```text
//!  texte ─► lisible (sans Markdown, code, liens, emojis ; « 8,5 % » → « 8,5 pour cent »)
//!        ─► phrases par tranches ─► /audio/speech (voix `voice.tts_voice`) ─► WAV assemblés
//!        ─► ffmpeg → OGG/Opus ─► sendVoice
//!  échec ─► la réponse part en texte, avec « vocal indisponible : <raison> »
//! ```

use crate::executor::Messenger;
use penelope_app::bus::Origin;
use penelope_app::ports::ProviderSource;
use penelope_app::services::Services;
use regex::Regex;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::OnceLock;

/// Taille d'une tranche envoyée à la synthèse, en caractères.
const CHUNK_CHARS: usize = 600;
/// Délai d'une synthèse, par tranche.
const SPEAK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Modèle de synthèse : alias du rôle `tts`, sinon l'alias `tts`, sinon le défaut livré.
/// Le rôle ne se replie jamais sur le modèle de conversation, qui ne sait pas parler.
pub fn tts_model(cfg: &penelope_kernel::config::Config) -> String {
    cfg.models
        .roles
        .get("tts")
        .and_then(|alias| cfg.alias_model(alias))
        .or_else(|| cfg.alias_model("tts"))
        .unwrap_or(penelope_kernel::config::DEFAULT_TTS_MODEL)
        .to_string()
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("motif valide"))
}

/// Texte à lire : sans blocs de code, tableaux, liens, balises Markdown ni emojis ; les
/// symboles courants dits en toutes lettres.
pub fn speech_text(markdown: &str) -> String {
    static LINK: OnceLock<Regex> = OnceLock::new();
    static URL: OnceLock<Regex> = OnceLock::new();
    static PERCENT: OnceLock<Regex> = OnceLock::new();
    static EURO: OnceLock<Regex> = OnceLock::new();
    static DOLLAR: OnceLock<Regex> = OnceLock::new();
    static SPACES: OnceLock<Regex> = OnceLock::new();
    let mut lines = Vec::new();
    let mut in_code = false;
    for line in markdown.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code || t.starts_with('|') {
            continue;
        }
        let t = t
            .trim_start_matches('#')
            .trim_start_matches(['-', '*', '>', '+'])
            .trim();
        if t.is_empty() {
            continue;
        }
        let mut line = t.to_string();
        if !line.ends_with(['.', '!', '?', ':', ';', ',']) {
            line.push('.');
        }
        lines.push(line);
    }
    let text = lines.join(" ");
    let text = re(&LINK, r"\[([^\]]+)\]\([^)]*\)").replace_all(&text, "$1");
    let text = re(&URL, r"https?://\S+").replace_all(&text, "");
    // Symboles dits en toutes lettres avant de retirer les pictogrammes (→ en fait partie).
    let text = text
        .replace("°C", " degrés")
        .replace(" & ", " et ")
        .replace(" → ", ", puis ")
        .replace(" / ", " ou ");
    let text: String = text
        .chars()
        .filter(|c| !matches!(c, '*' | '_' | '`' | '~'))
        .filter(|c| !is_emoji(*c))
        .collect();
    let text = re(&PERCENT, r"(\d)\s*%").replace_all(&text, "$1 pour cent");
    let text = re(&EURO, r"(\d)\s*€").replace_all(&text, "$1 euros");
    let text = re(&DOLLAR, r"(\d)\s*\$").replace_all(&text, "$1 dollars");
    let text = re(&SPACES, r"\s+").replace_all(&text, " ");
    text.replace(" .", ".").trim().to_string()
}

/// Pictogrammes, symboles décoratifs, sélecteurs de variante et liaisons d'emoji.
fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0xFE00..=0xFE0F | 0x200D
        | 0x2190..=0x21FF | 0x2300..=0x23FF | 0x25A0..=0x25FF | 0xE0020..=0xE007F)
}

/// Tranches de phrases d'au plus `max` caractères (une phrase plus longue reste entière).
pub fn chunks(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut sentence = String::new();
    let flush = |sentence: &mut String, current: &mut String, out: &mut Vec<String>| {
        let s = sentence.trim();
        if s.is_empty() {
            return;
        }
        if !current.is_empty() && current.chars().count() + 1 + s.chars().count() > max {
            out.push(std::mem::take(current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(s);
        sentence.clear();
    };
    for c in text.chars() {
        sentence.push(c);
        if matches!(c, '.' | '!' | '?') {
            flush(&mut sentence, &mut current, &mut out);
        }
    }
    flush(&mut sentence, &mut current, &mut out);
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// WAV PCM lu : format et échantillons.
#[derive(Debug, Clone, PartialEq)]
pub struct Wav {
    /// 1 : PCM entier ; 3 : flottant (mlx-audio peut rendre l'un ou l'autre).
    pub format: u16,
    pub channels: u16,
    pub rate: u32,
    pub bits: u16,
    pub data: Vec<u8>,
}

impl Wav {
    pub fn parse(bytes: &[u8]) -> Result<Wav, String> {
        if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err("la synthèse n'a pas rendu un WAV".into());
        }
        let (mut fmt, mut data) = (None, None);
        let mut at = 12;
        while at + 8 <= bytes.len() {
            let id = &bytes[at..at + 4];
            let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            let body = at + 8;
            let end = (body + len).min(bytes.len());
            match id {
                b"fmt " if end - body >= 16 => {
                    let f = &bytes[body..end];
                    fmt = Some((
                        u16::from_le_bytes([f[0], f[1]]),
                        u16::from_le_bytes([f[2], f[3]]),
                        u32::from_le_bytes([f[4], f[5], f[6], f[7]]),
                        u16::from_le_bytes([f[14], f[15]]),
                    ));
                }
                b"data" => data = Some(bytes[body..end].to_vec()),
                _ => {}
            }
            at = body + len + (len % 2);
        }
        let (format, channels, rate, bits) = fmt.ok_or("WAV sans format")?;
        Ok(Wav {
            format,
            channels,
            rate,
            bits,
            data: data.ok_or("WAV sans données")?,
        })
    }

    pub fn seconds(&self) -> f64 {
        let per_second = self.rate as f64 * self.channels as f64 * (self.bits as f64 / 8.0);
        if per_second <= 0.0 {
            0.0
        } else {
            self.data.len() as f64 / per_second
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let data_len = self.data.len() as u32;
        let block = self.channels * (self.bits / 8);
        let mut out = Vec::with_capacity(44 + self.data.len());
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&self.format.to_le_bytes());
        out.extend_from_slice(&self.channels.to_le_bytes());
        out.extend_from_slice(&self.rate.to_le_bytes());
        out.extend_from_slice(&(self.rate * block as u32).to_le_bytes());
        out.extend_from_slice(&block.to_le_bytes());
        out.extend_from_slice(&self.bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.extend_from_slice(&self.data);
        out
    }

    /// Assemble des WAV de même format, bout à bout.
    pub fn concat(parts: Vec<Wav>) -> Result<Wav, String> {
        let mut iter = parts.into_iter();
        let mut first = iter.next().ok_or("aucune tranche synthétisée")?;
        for w in iter {
            if (w.format, w.channels, w.rate, w.bits)
                != (first.format, first.channels, first.rate, first.bits)
            {
                return Err("tranches de formats différents".into());
            }
            first.data.extend(w.data);
        }
        Ok(first)
    }
}

/// Synthétise un texte lisible ; rend le WAV assemblé.
pub async fn synthesize(
    s: &Services,
    providers: &dyn ProviderSource,
    text: &str,
    voice: &str,
) -> Result<Wav, String> {
    let cfg = s.config.config();
    let model = tts_model(&cfg);
    if penelope_llm::catalog::provider_of(&model) != "openrouter"
        && !cfg.providers.local.enabled
        && providers.provider_override_active().is_none()
    {
        return Err(format!(
            "la synthèse vise un serveur local (`{model}`) mais `providers.local` n'est pas \
             activé : `penelope config set providers.local.enabled true`"
        ));
    }
    let model = penelope_app::codex_scope::background(s, &model, "synthèse vocale").await;
    let provider = providers.provider_for(&model).await?;
    let mut parts = Vec::new();
    for chunk in chunks(text, CHUNK_CHARS) {
        let bytes =
            tokio::time::timeout(SPEAK_TIMEOUT, provider.speak(&model, &chunk, voice, "wav"))
                .await
                .map_err(|_| "synthèse trop longue (plus de 3 min)".to_string())?
                .map_err(|e| format!("{} ({model})", e.message))?;
        parts.push(Wav::parse(&bytes)?);
    }
    Wav::concat(parts)
}

/// Lit `text` en vocal dans la conversation `session_id`. Rend la durée et le fichier.
#[allow(clippy::too_many_arguments)]
pub async fn send(
    s: &Services,
    providers: &dyn ProviderSource,
    messenger: Option<Arc<dyn Messenger>>,
    session_id: &str,
    origin: &Origin,
    text: &str,
    voice: Option<&str>,
    caption: Option<&str>,
) -> Result<Value, String> {
    let cfg = s.config.config();
    let spoken = speech_text(text);
    let voice = voice
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(&cfg.voice.tts_voice)
        .to_string();
    let wav = synthesize(s, providers, &spoken, &voice).await?;
    let seconds = wav.seconds();
    let dir = s.platform.dirs.data().join("media").join("voice");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let id = penelope_kernel::ids::Ulid::new().to_string();
    let (wav_path, ogg_path) = (dir.join(format!("{id}.wav")), dir.join(format!("{id}.ogg")));
    std::fs::write(&wav_path, wav.to_bytes()).map_err(|e| e.to_string())?;
    let (w, o) = (wav_path.clone(), ogg_path.clone());
    let converted = tokio::task::spawn_blocking(move || {
        penelope_platform::audio::wav_to_ogg_opus(&w, &o).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&wav_path);
    converted?;
    let messenger = messenger.ok_or("aucun canal de message disponible")?;
    messenger
        .send_session_voice(
            session_id,
            origin,
            &ogg_path,
            seconds.ceil() as u32,
            caption,
        )
        .await?;
    let _ = s
        .budget
        .record(penelope_kernel::budget::UsageRecord {
            session_id: Some(session_id.to_string()),
            model: penelope_llm::catalog::strip_provider(&tts_model(&cfg)).to_string(),
            provider: "openai_compat".into(),
            role: Some("tts".into()),
            cost_usd: 0.0,
            ..Default::default()
        })
        .await;
    let _ = s
        .events
        .append(
            penelope_kernel::event::EventDraft::new(
                "voice.sent",
                json!({"seconds": (seconds * 10.0).round() / 10.0, "chars": spoken.chars().count(), "voice": voice}),
            )
            .session(session_id),
        )
        .await;
    Ok(json!({
        "sent": true,
        "seconds": (seconds * 10.0).round() / 10.0,
        "chars": spoken.chars().count(),
        "voice": voice,
    }))
}

/// Contrôle `doctor` (issue #41) : `ffmpeg` présent et synthèse joignable, en lisant une
/// phrase d'une seconde.
pub async fn doctor_check(
    s: &Services,
    providers: &dyn ProviderSource,
) -> penelope_kernel::api::DoctorCheck {
    use penelope_kernel::api::DoctorCheck;
    const ID: &str = "voice";
    const LABEL: &str = "Réponses vocales";
    let cfg = s.config.config();
    let model = tts_model(&cfg);
    if penelope_platform::audio::ffmpeg().is_none() {
        return DoctorCheck::fail(
            ID,
            LABEL,
            "`ffmpeg` absent : un vocal ne peut pas être converti au format Telegram",
            Some("brew install ffmpeg".into()),
        );
    }
    let probe = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        synthesize(s, providers, "Bonjour.", &cfg.voice.tts_voice),
    )
    .await;
    match probe {
        Ok(Ok(wav)) => DoctorCheck::ok(
            ID,
            LABEL,
            format!(
                "`{model}`, voix `{}` : {:.1} s de synthèse d'essai",
                cfg.voice.tts_voice,
                wav.seconds()
            ),
        ),
        Ok(Err(e)) => DoctorCheck::fail(
            ID,
            LABEL,
            format!("synthèse indisponible : {e}"),
            Some("docs/install-headless.md, « Messages vocaux »".into()),
        ),
        Err(_) => DoctorCheck::fail(
            ID,
            LABEL,
            format!("synthèse sans réponse en 30 s (`{model}`)"),
            Some("vérifier le serveur mlx-audio".into()),
        ),
    }
}

/// Outil `send_voice` : un texte trop long est renvoyé au modèle (résumé vocal à proposer) ;
/// une synthèse impossible envoie la réponse en texte, avec la raison.
pub async fn tool(
    s: &Services,
    providers: &dyn ProviderSource,
    messenger: Option<Arc<dyn Messenger>>,
    session_id: &str,
    origin: &Origin,
    args: &Value,
) -> Result<Value, String> {
    let cfg = s.config.config();
    let text = args["text"].as_str().unwrap_or_default().trim().to_string();
    if text.is_empty() {
        return Err("`text` vide".into());
    }
    let chars = speech_text(&text).chars().count();
    if chars > cfg.voice.max_chars {
        return Err(format!(
            "texte trop long pour un vocal ({chars} caractères, {} au plus, \
             `voice.max_chars`) : envoie un résumé vocal et garde le détail en texte",
            cfg.voice.max_chars
        ));
    }
    match send(
        s,
        providers,
        messenger.clone(),
        session_id,
        origin,
        &text,
        args["voice"].as_str(),
        args["caption"].as_str(),
    )
    .await
    {
        Ok(v) => Ok(v),
        Err(reason) => {
            tracing::warn!(session = %session_id, error = %reason, "vocal indisponible, repli en texte");
            let messenger = messenger.ok_or("aucun canal de message disponible")?;
            messenger
                .send_session_text(
                    session_id,
                    origin,
                    &format!("{text}\n\n_(vocal indisponible : {reason})_"),
                )
                .await?;
            Ok(json!({
                "sent": false,
                "fallback": "texte",
                "reason": reason,
                "note": "la réponse est partie en texte : ne la répète pas",
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_text_keeps_what_is_said_out_loud() {
        let md = "## Veille du jour 🚀\n\n\
                  - **Mistral** publie [Voxtral](https://mistral.ai/news) : 8,5 % plus rapide.\n\
                  - Budget : 12 € → 30 $ / mois\n\n\
                  ```rust\nfn main() {}\n```\n\
                  | a | b |\n|---|---|\n\
                  Voir https://example.org/x ✅";
        let t = speech_text(md);
        assert_eq!(
            t,
            "Veille du jour. Mistral publie Voxtral : 8,5 pour cent plus rapide. Budget : 12 \
             euros, puis 30 dollars ou mois. Voir."
        );
        assert!(!t.contains("fn main"));
    }

    #[test]
    fn long_texts_are_cut_between_sentences() {
        let text = "Une phrase courte. ".repeat(80);
        let parts = chunks(text.trim(), 200);
        assert!(parts.len() > 1);
        assert!(
            parts
                .iter()
                .all(|p| p.chars().count() <= 200 && p.ends_with('.'))
        );
        assert_eq!(parts.join(" "), text.trim());
    }

    #[test]
    fn wav_parts_are_joined_with_their_duration() {
        let one = Wav::parse(&penelope_llm::mock::silent_wav(1.0)).unwrap();
        assert_eq!((one.channels, one.rate, one.bits), (1, 16_000, 16));
        assert!((one.seconds() - 1.0).abs() < 1e-6);
        let two = Wav::concat(vec![one.clone(), one]).unwrap();
        assert!((two.seconds() - 2.0).abs() < 1e-6);
        let back = Wav::parse(&two.to_bytes()).unwrap();
        assert_eq!(back, two);
        assert!(Wav::parse(b"OggS").is_err());
    }

    #[test]
    fn the_tts_model_never_falls_back_to_the_chat_model() {
        let mut cfg = penelope_kernel::config::Config::default();
        assert_eq!(tts_model(&cfg), penelope_kernel::config::DEFAULT_TTS_MODEL);
        cfg.models.roles.remove("tts");
        cfg.models.aliases.remove("tts");
        assert_eq!(tts_model(&cfg), penelope_kernel::config::DEFAULT_TTS_MODEL);
        cfg.models
            .aliases
            .insert("voix".into(), "openai_compat:kokoro".into());
        cfg.models.roles.insert("tts".into(), "voix".into());
        assert_eq!(tts_model(&cfg), "openai_compat:kokoro");
    }
}
