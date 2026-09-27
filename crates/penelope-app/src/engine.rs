//! Ports du moteur de tours (épopée #208, T33, `design/v1/decoupage-daemon.md` §6) :
//! ce que la passerelle et le RPC demandent au moteur, au lieu de nommer le daemon.
//!
//! ```text
//!  passerelle, rpc ──TurnIntake──►  file des tours, session du canal
//!                  ──SessionModels─► alias épinglé, état du modèle d'une session
//!                  ──Transcriber───► audio → texte, images → description
//! ```
//!
//! Le daemon les implémente sur son cœur (`penelope_daemon::runtime::Core`).

use crate::bus::Origin;
use penelope_kernel::ids::TurnId;
use penelope_llm::StickyModel;
use serde_json::Value;
use std::path::PathBuf;

/// L'entrée des tours : un message, une relance ou une reprise mis en file, et la
/// session de chat d'un canal.
#[async_trait::async_trait]
pub trait TurnIntake: Send + Sync {
    /// Met un message en file pour une session. `dedup` rend l'ajout idempotent.
    async fn enqueue_message(
        &self,
        session_id: &str,
        text: &str,
        origin: &Origin,
        dedup: Option<String>,
    ) -> anyhow::Result<Option<TurnId>>;

    /// Met en file un message accompagné de photos (chemins enregistrés par
    /// [`crate::media::save_photo`]).
    async fn enqueue_message_with_images(
        &self,
        session_id: &str,
        text: &str,
        images: &[PathBuf],
        origin: &Origin,
        dedup: Option<String>,
    ) -> anyhow::Result<Option<TurnId>>;

    /// Relance la réponse d'une session sur son transcript, sans nouveau message
    /// (bouton « Réessayer » après un échec).
    async fn enqueue_retry(
        &self,
        session_id: &str,
        origin: &Origin,
        token: &str,
    ) -> anyhow::Result<Option<TurnId>>;

    /// Remet en file la suite d'un tour suspendu par une approbation.
    async fn enqueue_resume(
        &self,
        session_id: &str,
        approval_id: &str,
        origin: &Origin,
    ) -> anyhow::Result<Option<TurnId>>;

    /// Session de chat active d'un canal ; créée au besoin.
    async fn chat_session_for(&self, origin: &Origin) -> anyhow::Result<String>;
}

/// Le modèle d'une session : épinglé ou automatique (`/model`, `model.*`).
#[async_trait::async_trait]
pub trait SessionModels: Send + Sync {
    /// Alias épinglé sur une session, s'il existe encore dans la configuration.
    async fn pinned_model(&self, session_id: &str) -> Option<StickyModel>;

    /// Épingle un alias sur une session, ou revient à l'automatique (`None`).
    async fn pin_model(&self, session_id: &str, alias: Option<&str>) -> anyhow::Result<()>;

    /// État du modèle d'une session : épinglé ou automatique, dernier alias utilisé, choix.
    async fn session_model_view(&self, session_id: &str) -> anyhow::Result<Value>;
}

/// Les médias qu'un canal reçoit : un vocal à transcrire, des images à décrire.
#[async_trait::async_trait]
pub trait Transcriber: Send + Sync {
    /// Transcrit un audio avec le modèle du rôle `stt` (§14.4) et en compte le coût.
    async fn transcribe(
        &self,
        audio: Vec<u8>,
        filename: &str,
        session_id: &str,
    ) -> Result<String, String>;

    /// Décrit des images avec le modèle du rôle `image_describe` (alias `vision`).
    async fn describe_images(
        &self,
        urls: &[String],
        caption: &str,
        session_id: &str,
        turn_id: &str,
    ) -> Result<String, String>;
}
