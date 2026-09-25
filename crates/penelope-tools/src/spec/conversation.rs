//! Catalogue : canal, mémoire, jobs d'outils et historique.

use super::*;

/// Section « canal » du catalogue.
pub(super) fn channel() -> Vec<ToolSpec> {
    vec![
        spec(
            "send_message",
            RiskClass::Write,
            "Envoie un message au propriétaire.",
            obj(
                json!({"text": {"type":"string"}, "template": {"type":"string"}}),
                &["text"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "send_voice",
            RiskClass::Read,
            "Lit un texte en message vocal (voix féminine locale) dans cette conversation. \
             Seulement sur demande explicite (« en vocal », « lis-moi », « à voix haute ») ou \
             en réponse à un vocal si `voice.reply_in_kind` est actif ; jamais pour du code, un \
             tableau ou une réponse longue : un résumé vocal, le détail en texte. Le Markdown, \
             les liens et les emojis sont retirés. Synthèse impossible : la réponse part en \
             texte avec la raison.",
            obj(
                json!({
                    "text": {"type":"string", "description": "Ce qui sera dit."},
                    "voice": {"type":"string", "description": "Voix préréglée ; défaut `voice.tts_voice`."},
                    "caption": {"type":"string", "description": "Légende courte facultative."}
                }),
                &["text"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "send_file",
            RiskClass::Write,
            "Envoie un fichier au propriétaire.",
            obj(
                json!({"path": {"type":"string"}, "caption": {"type":"string"}}),
                &["path"],
            ),
            false,
            false,
            false,
        ),
    ]
}

/// Section « mémoire » du catalogue.
pub(super) fn memory() -> Vec<ToolSpec> {
    vec![
        spec(
            "mem_search",
            RiskClass::Read,
            "Recherche dans la mémoire curée, dans les documents ingérés (`vault/sources`, \
             passages encadrés comme non fiables, `slug` pour un seul document) et, sur \
             demande explicite, épisodique.",
            obj(
                json!({
                    "query": {"type":"string"},
                    "level": {"type":"string"},
                    "projet": {"type":"string"},
                    "slug": {"type":"string"},
                    "include_episodic": {"type":"boolean"},
                    "limit": {"type":"integer","minimum":1,"maximum":50}
                }),
                &["query"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "mem_get",
            RiskClass::Read,
            "Lit une entrée de mémoire par uid ou par slug.",
            obj(
                json!({"uid": {"type":"string"}, "slug": {"type":"string"}}),
                &[],
            ),
            true,
            false,
            false,
        ),
        spec(
            "mem_neighbors",
            RiskClass::Read,
            "Voisins d'une note dans le graphe du vault : concepts d'une source, sources et \
             entrées de mémoire qui citent un concept (liens `[[slug]]` sortants et entrants).",
            obj(json!({"slug": {"type":"string"}}), &["slug"]),
            true,
            false,
            false,
        ),
        spec(
            "mem_note",
            RiskClass::Write,
            "Note une observation dans le journal du jour. N'écrit jamais dans le niveau \
             curé. Pour une règle, une correction ou une décision que le propriétaire vient \
             d'énoncer, `citation` recopie mot pour mot l'extrait de son message : la note \
             compte alors comme venant de lui.",
            obj(
                json!({
                    "type": {"type":"string","enum":["fait","preference","correction","ecart","decision","procedure_candidate"]},
                    "texte": {"type":"string"},
                    "citation": {"type":"string"},
                    "quand": {"type":"string"},
                    "importance": {"type":"integer","minimum":1,"maximum":10}
                }),
                &["type", "texte"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "mem_remember",
            RiskClass::Write,
            "Écrit directement en mémoire. Refusé si le message courant ne le demande pas \
             explicitement. Une entrée par fait, 300 caractères au plus : pour de la \
             matière longue, `mem_note`.",
            obj(
                json!({
                    "niveau": {"type":"string","enum":["profil","coeur","projet","cure"]},
                    "texte": {
                        "type": "string",
                        "maxLength": penelope_memory::quality::MAX_ENTRY_CHARS,
                        "description": "un seul fait, 300 caractères au plus"
                    }
                }),
                &["niveau", "texte"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "mem_forget",
            RiskClass::Destructive,
            "Retire une entrée de mémoire. Toujours soumis à approbation.",
            obj(json!({"uid": {"type":"string"}}), &["uid"]),
            false,
            false,
            false,
        ),
        spec(
            "intent_create",
            RiskClass::Write,
            "Arme une intention événementielle : « quand on reparle de X, rappelle-moi Y ». \
             Elle revient dans le contexte du message qui en parle. Pour un rappel daté, \
             utiliser `schedule_create`.",
            obj(
                json!({"texte": {"type":"string"}, "declencheurs": {"type":"array","items":{"type":"string"}}}),
                &["texte"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "intent_list",
            RiskClass::Read,
            "Liste les intentions armées.",
            obj(json!({}), &[]),
            true,
            false,
            false,
        ),
    ]
}

/// Section « jobs d'outils (#204) » du catalogue.
pub(super) fn jobs() -> Vec<ToolSpec> {
    vec![
        spec(
            "job_list",
            RiskClass::Read,
            "Jobs d'outils de cette session : ceux qui tournent encore, leur outil et leur \
             âge. À relire avant d'en lancer un de plus.",
            obj(
                json!({"all": {"type":"boolean","description":"Toutes les sessions, pas seulement celle-ci."}}),
                &[],
            ),
            true,
            false,
            false,
        ),
        spec(
            "job_status",
            RiskClass::Read,
            "État d'un job lancé en arrière-plan : `working`, `completed`, `failed` ou \
             `cancelled`, et son résultat s'il est terminé.",
            obj(json!({"job": {"type":"string"}}), &["job"]),
            true,
            false,
            false,
        ),
        spec(
            "job_wait",
            RiskClass::Read,
            "Attend la fin d'un job, au plus `timeout_ms` (120 s au maximum, 30 s par \
             défaut). Au-delà, rend l'état courant sans bloquer le tour : inutile de \
             boucler, le résultat revient seul dans la session quand il est prêt.",
            obj(
                json!({
                    "job": {"type":"string"},
                    "timeout_ms": {"type":"integer","minimum":1000,"maximum":120000}
                }),
                &["job"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "job_cancel",
            RiskClass::Write,
            "Annule un job en cours : le processus et son groupe sont tués, le job passe \
             `cancelled`.",
            obj(json!({"job": {"type":"string"}}), &["job"]),
            false,
            false,
            false,
        ),
        spec(
            "intent_cancel",
            RiskClass::Write,
            "Annule une intention.",
            obj(json!({"id": {"type":"string"}}), &["id"]),
            true,
            false,
            false,
        ),
    ]
}

/// Section « historique » du catalogue.
pub(super) fn history() -> Vec<ToolSpec> {
    vec![
        spec(
            "history_grep",
            RiskClass::Read,
            "Recherche plein texte dans les messages bruts et les résumés. Par défaut dans \
             la session en cours ; `scope: \"all\"` cherche dans toutes les sessions, y \
             compris fermées, pour retrouver une conversation antérieure. `query` attend des \
             mots-clés, pas une phrase : chaque mot doit apparaître. Chaque extrait donne le \
             titre et la date de sa session.",
            obj(
                json!({
                    "query": {
                        "type":"string",
                        "description":"mots-clés, tous exigés (ex. « facturation ACME »)"
                    },
                    "scope": {
                        "type":"string",
                        "enum":["session","all"],
                        "description":"session : la conversation en cours (défaut) ; all : \
                                       toutes les sessions"
                    },
                    "depth": {"type":"integer","minimum":0,"maximum":10}
                }),
                &["query"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "history_describe",
            RiskClass::Read,
            "Manifeste d'un nœud de résumé : tokens, intervalle, enfants.",
            obj(json!({"node_id": {"type":"string"}}), &["node_id"]),
            true,
            false,
            false,
        ),
        spec(
            "history_expand",
            RiskClass::Read,
            "Contenu paginé d'un nœud ou d'un intervalle brut.",
            obj(
                json!({"node_id": {"type":"string"}, "page": {"type":"integer","minimum":0}}),
                &["node_id"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "history_expand_query",
            RiskClass::Read,
            "Retrouve, dans toutes les sessions y compris fermées, les passages liés à une \
             question en langage naturel : recherche mot significatif par mot significatif, \
             extraits classés par nombre de mots trouvés, avec le titre et la date de leur \
             session. `history_expand` lit ensuite un passage en entier.",
            obj(
                json!({"question": {"type":"string"}, "budget": {"type":"integer"}}),
                &["question"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "artifact_read",
            RiskClass::Read,
            "Lecture paginée d'un artefact ; le curseur n'avance que des octets renvoyés.",
            obj(
                json!({"id": {"type":"string"}, "cursor": {"type":"integer","minimum":0}}),
                &["id"],
            ),
            true,
            false,
            false,
        ),
    ]
}
