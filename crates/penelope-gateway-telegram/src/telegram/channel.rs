//! Les cartes Telegram vues du cœur (`penelope_app::channel::Cards`, épopée #208, T36) :
//! gabarits de texte et liens profonds. Les gabarits, leurs boutons et les jetons
//! d'action restent ici ; `Services` n'en porte plus que le port.

use crate::bus::Origin;
use crate::helpers::topic_name_key;
use crate::runtime::Services;
use penelope_app::channel::{CardTemplate, Cards};
use penelope_store::Store;
use penelope_telegram::TemplateRegistry;
use std::path::Path;
use std::sync::Arc;

/// Nom du bot, relevé au démarrage : les liens profonds le citent (issue #30).
pub const BOT_USERNAME_KEY: &str = "tg.bot_username";

/// Titre d'un groupe autorisé, pour nommer où livre une planification (#124).
pub fn chat_title_key(chat_id: i64) -> String {
    format!("tg.chat_title.{chat_id}")
}

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

/// Nom lisible d'une conversation Telegram : titre du groupe et nom du sujet quand
/// Pénélope les a vus passer, identifiants sinon (issue #124). `None` : l'origine n'est
/// pas une conversation Telegram.
pub async fn place_name(s: &Services, origin: &Origin) -> Option<String> {
    let Origin::Telegram {
        chat_id, topic_id, ..
    } = origin
    else {
        return None;
    };
    let kv = |k: String| async move {
        s.store
            .read(move |c| penelope_store::kv_get(c, &k))
            .await
            .ok()
            .flatten()
            .filter(|v| !v.trim().is_empty())
    };
    let owner = s.config.config().owner.telegram_user_id;
    let chat = if *chat_id == owner {
        "conversation privée".to_string()
    } else if *chat_id > 0 {
        format!("conversation {chat_id}")
    } else {
        match kv(chat_title_key(*chat_id)).await {
            Some(title) => format!("groupe « {title} »"),
            None => format!("groupe {chat_id}"),
        }
    };
    Some(match topic_id {
        None => chat,
        Some(t) => match kv(topic_name_key(*chat_id, *t)).await {
            Some(name) => format!("sujet « {name} », {chat}"),
            None => format!("sujet {t}, {chat}"),
        },
    })
}

/// Une planification ne se déplace que vers la conversation du propriétaire ou une
/// conversation autorisée (issue #124) ; la destination garde le sujet, pas le message.
pub fn destination_for(s: &Services, origin: &Origin) -> Result<Origin, String> {
    let cfg = s.config.config();
    let (chat_id, topic_id) = origin.telegram_chat().unwrap_or((0, None));
    if chat_id == 0 {
        return Err("Telegram n'est pas configuré (`owner.telegram_user_id`)".into());
    }
    if !(chat_id == cfg.owner.telegram_user_id || cfg.telegram.allowed_chats.contains(&chat_id)) {
        return Err(format!(
            "la conversation {chat_id} n'est pas autorisée : `penelope config set \
             telegram.allowed_chats '[{chat_id}]'`"
        ));
    }
    Ok(Origin::Telegram {
        chat_id,
        topic_id,
        message_id: None,
    })
}
