//! Doubles de test partagés (épopée #208, lot D, tâche T10) : un canal de message qui
//! enregistre ce qu'on lui confie, des providers qui répondent par un `MockProvider`.
//! Publics, pour les tests d'intégration et `penelope-evals` ; rien ici n'est câblé en
//! production.

use crate::bus::Origin;
use crate::executor::Messenger;
use crate::ports::ProviderSource;
use penelope_llm::Provider;
use penelope_llm::mock::MockProvider;
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Canal de message qui enregistre textes (avec leur origine) et cartes d'approbation.
/// Les cartes de progression et les questions d'étape suivent le repli du trait (un
/// texte), sauf avec [`RecordingMessenger::with_cards`] qui les range à part.
#[derive(Default)]
pub struct RecordingMessenger {
    sent: Mutex<Vec<(Origin, String)>>,
    approvals: Mutex<Vec<String>>,
    /// Cartes et questions rangées à part plutôt que rendues en texte.
    separate: bool,
    questions: Mutex<Vec<(String, String, Vec<String>)>>,
    cards: Mutex<Vec<(String, String)>>,
}

impl RecordingMessenger {
    pub fn new() -> Arc<RecordingMessenger> {
        Arc::new(RecordingMessenger::default())
    }

    /// Cartes de progression et questions d'étape enregistrées à part (workflows).
    pub fn with_cards() -> Arc<RecordingMessenger> {
        Arc::new(RecordingMessenger {
            separate: true,
            ..RecordingMessenger::default()
        })
    }

    /// Textes envoyés, dans l'ordre.
    pub fn texts(&self) -> Vec<String> {
        self.sent().into_iter().map(|(_, t)| t).collect()
    }

    /// Textes envoyés avec leur origine.
    pub fn sent(&self) -> Vec<(Origin, String)> {
        self.sent.lock().unwrap().clone()
    }

    /// Identifiants des demandes d'approbation présentées.
    pub fn approvals(&self) -> Vec<String> {
        self.approvals.lock().unwrap().clone()
    }

    /// Questions d'étape : (`run|visite`, texte, choix). Avec `with_cards` seulement.
    pub fn questions(&self) -> Vec<(String, String, Vec<String>)> {
        self.questions.lock().unwrap().clone()
    }

    /// Cartes de progression : (clé, texte). Avec `with_cards` seulement.
    pub fn cards(&self) -> Vec<(String, String)> {
        self.cards.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Messenger for RecordingMessenger {
    async fn send_text(&self, origin: &Origin, markdown: &str) -> Result<(), String> {
        self.sent
            .lock()
            .unwrap()
            .push((origin.clone(), markdown.to_string()));
        Ok(())
    }

    async fn send_file(&self, _: &Origin, _: &Path, _: Option<&str>) -> Result<(), String> {
        Ok(())
    }

    async fn send_approval(&self, _: &Origin, approval_id: &str) -> Result<(), String> {
        self.approvals.lock().unwrap().push(approval_id.to_string());
        Ok(())
    }

    async fn send_question(
        &self,
        origin: &Origin,
        markdown: &str,
        run_id: &str,
        visit: &str,
        choices: &[String],
        _: bool,
        form: Option<&Value>,
    ) -> Result<(), String> {
        if !self.separate {
            let text = crate::executor::question_text(markdown, run_id, choices, form);
            return self.send_text(origin, &text).await;
        }
        self.questions.lock().unwrap().push((
            format!("{run_id}|{visit}"),
            markdown.to_string(),
            choices.to_vec(),
        ));
        Ok(())
    }

    async fn upsert_card(&self, origin: &Origin, key: &str, markdown: &str) -> Result<(), String> {
        if !self.separate {
            return self.send_text(origin, markdown).await;
        }
        self.cards
            .lock()
            .unwrap()
            .push((key.to_string(), markdown.to_string()));
        Ok(())
    }
}

/// Providers de test : chaque modèle répond par le même `MockProvider`, comme un
/// provider imposé au daemon.
pub struct MockProviders(pub Arc<MockProvider>);

impl MockProviders {
    pub fn new(p: Arc<MockProvider>) -> Arc<MockProviders> {
        Arc::new(MockProviders(p))
    }
}

#[async_trait::async_trait]
impl ProviderSource for MockProviders {
    async fn provider_for(&self, _: &str) -> Result<Arc<dyn Provider>, String> {
        Ok(self.0.clone())
    }

    fn provider_override_active(&self) -> Option<Arc<dyn Provider>> {
        Some(self.0.clone())
    }
}
