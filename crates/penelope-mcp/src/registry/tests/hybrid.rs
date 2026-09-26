//! `tool_search` hybride (issue #11) : le repli lexical quand FTS ne trouve rien, et les
//! outils proches par le sens qui complètent ceux trouvés par les mots.

use super::*;

/// Registre de trois outils ; les vecteurs sont posés à la main, comme les écrit
/// l'indexation des embeddings.
async fn indexed(vectors: &[(&str, [f32; 3])]) -> ToolRegistry {
    let r = registry().await;
    r.replace_server_tools(
        "redmine",
        vec![
            RegisteredTool::from_descriptor(
                "redmine",
                &descriptor("get_issue", "Lit un ticket", json!({})),
            ),
            RegisteredTool::from_descriptor(
                "redmine",
                &descriptor("log_time", "Déclare du temps passé", json!({})),
            ),
        ],
        "t",
    )
    .await
    .unwrap();
    r.replace_server_tools(
        "forge",
        vec![RegisteredTool::from_descriptor(
            "forge",
            &descriptor("merge_request", "Fusionne une branche", json!({})),
        )],
        "t",
    )
    .await
    .unwrap();
    let rows: Vec<(String, Vec<u8>)> = vectors
        .iter()
        .map(|(q, v)| (q.to_string(), penelope_store::encode_embedding(v)))
        .collect();
    r.store
        .write(move |tx| {
            for (q, blob) in &rows {
                tx.execute(
                    "INSERT INTO mcp_tools_vec(qualified, dim, embedding) VALUES(?1, 3, ?2)",
                    params![q, blob],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    r
}

fn names(hits: &[SearchHit]) -> Vec<String> {
    hits.iter().map(|h| h.tool.name.clone()).collect()
}

/// Une requête d'un seul caractère n'a pas de requête FTS : le repli lexical trouve
/// quand même, par le nom d'abord, et n'invente rien.
#[tokio::test]
async fn a_one_letter_query_falls_back_to_lexical_scoring() {
    let r = indexed(&[]).await;
    let hits = r.search("g", None, 10).await.unwrap();
    // `merge_request` gagne : son serveur `forge` contient aussi la lettre.
    assert_eq!(names(&hits), ["merge_request", "get_issue", "log_time"]);
    assert!(hits[0].score > hits[1].score);
    assert!(r.search("z", None, 10).await.unwrap().is_empty());
    assert!(r.search("", None, 10).await.unwrap().is_empty());
    let scoped = r.search("g", Some("forge"), 10).await.unwrap();
    assert_eq!(names(&scoped), ["merge_request"]);
}

/// Sans vecteur de requête, la recherche hybride est la recherche par mots.
#[tokio::test]
async fn without_a_query_vector_hybrid_search_is_keyword_search() {
    let r = indexed(&[]).await;
    let a = r.search_hybrid("ticket", None, None, 5).await.unwrap();
    let b = r.search("ticket", None, 5).await.unwrap();
    assert_eq!(names(&a), names(&b));
}

/// Un outil proche par le sens complète ceux trouvés par les mots ; un outil trop
/// éloigné (sous le seuil de similarité) n'est pas proposé, et un outil déjà trouvé
/// n'apparaît pas deux fois.
#[tokio::test]
async fn near_tools_complete_keyword_hits() {
    let get = qualified_name("redmine", "get_issue");
    let log = qualified_name("redmine", "log_time");
    let merge = qualified_name("forge", "merge_request");
    let r = indexed(&[
        (&get, [1.0, 0.0, 0.0]),
        (&log, [0.9, 0.1, 0.0]),
        (&merge, [0.0, 0.0, 1.0]),
    ])
    .await;
    let q = [1.0, 0.0, 0.0];
    let hits = r.search_hybrid("ticket", Some(&q), None, 5).await.unwrap();
    assert_eq!(names(&hits), ["get_issue", "log_time"], "{hits:?}");

    // Portée à un serveur : les voisins d'un autre serveur ne comptent pas.
    let hits = r
        .search_hybrid("fusion", Some(&q), Some("forge"), 5)
        .await
        .unwrap();
    assert!(!names(&hits).contains(&"log_time".to_string()), "{hits:?}");
}

/// Le sens a au moins la moitié des places : avec une limite de 2, un voisin chasse la
/// dernière correspondance par mots.
#[tokio::test]
async fn near_tools_keep_half_of_the_places() {
    let get = qualified_name("redmine", "get_issue");
    let log = qualified_name("redmine", "log_time");
    let merge = qualified_name("forge", "merge_request");
    let r = indexed(&[
        (&merge, [1.0, 0.0, 0.0]),
        (&get, [1.0, 0.0, 0.0]),
        (&log, [0.0, 1.0, 0.0]),
    ])
    .await;
    // « e » : un caractère, tout passe par le repli lexical ; à égalité de score,
    // l'ordre des noms qualifiés départage.
    let words = r.search("e", None, 2).await.unwrap();
    assert_eq!(names(&words), ["merge_request", "get_issue"]);
    let hits = r
        .search_hybrid("e", Some(&[0.0, 1.0, 0.0]), None, 2)
        .await
        .unwrap();
    assert_eq!(names(&hits), ["merge_request", "log_time"]);
}
