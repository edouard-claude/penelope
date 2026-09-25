//! Les cartes Telegram vues du cœur (`penelope_app::channel::Cards`, épopée #208, T36) :
//! gabarits de texte et liens profonds. Les gabarits, leurs boutons et les jetons
//! d'action restent ici ; `Services` n'en porte plus que le port.

use penelope_app::channel::{CardTemplate, Cards};
use penelope_store::Store;
use penelope_telegram::TemplateRegistry;
use std::path::Path;
use std::sync::Arc;

/// Nom du bot, relevé au démarrage : les liens profonds le citent (issue #30).
pub const BOT_USERNAME_KEY: &str = "tg.bot_username";

/// Gabarits intégrés, puis ceux que l'utilisateur a déposés (§14.6).
pub fn load_templates(dir: &Path) -> TemplateRegistry {
    let mut templates = TemplateRegistry::with_builtins();
    templates.load_dir(dir);
    templates
}

/// Les cartes du canal, pour la composition : le cœur valide les workflows sur leur
/// catalogue dès le démarrage, avant que la passerelle ne soit construite.
pub fn cards(templates_dir: &Path, store: Store) -> Arc<dyn Cards> {
    Arc::new(TelegramCards {
        templates: Arc::new(load_templates(templates_dir)),
        store,
    })
}

pub(crate) struct TelegramCards {
    pub(crate) templates: Arc<TemplateRegistry>,
    pub(crate) store: Store,
}

#[async_trait::async_trait]
impl Cards for TelegramCards {
    fn catalog(&self) -> Vec<String> {
        penelope_telegram::templates::CATALOG
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn template(&self, id: &str) -> Option<CardTemplate> {
        self.templates.get(id).map(|t| CardTemplate {
            body: t.body.clone(),
            variables: t.variables.clone(),
        })
    }

    /// Lien `https://t.me/<bot>?start=<charge>`, quand le bot est connu.
    async fn deep_link(&self, payload: &str) -> Option<String> {
        let bot = self
            .store
            .read(|c| penelope_store::kv_get(c, BOT_USERNAME_KEY))
            .await
            .ok()
            .flatten()?;
        (!bot.is_empty() && bot != "?").then(|| penelope_telegram::render::deep_link(&bot, payload))
    }
}
