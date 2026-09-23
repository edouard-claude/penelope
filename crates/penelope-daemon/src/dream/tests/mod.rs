use super::*;

use penelope_kernel::clock::TestClock;

use penelope_llm::mock::MockProvider;

use penelope_memory::CandidateType;

mod apply;
mod batches;
mod candidates;
mod digest;

async fn daemon() -> (tempfile::TempDir, Arc<Daemon>, Arc<MockProvider>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::new(1_789_516_800_000));
    let s = Arc::new(
        crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    (dir, d, p)
}

async fn note(
    d: &Daemon,
    ctype: CandidateType,
    text: &str,
    origin: Origin,
    session: &str,
    importance: u8,
) {
    let s = &d.services;
    let c = Candidate::new(ctype, text, origin, "interactive", &s.clock.now_rfc3339())
        .in_session(session)
        .with_importance(importance);
    s.candidates.record(vec![c], 5).await.unwrap();
}

/// Réponses « gardé » pour `n` candidats d'un lot, chacune écrite dans `projets.md` :
/// de quoi vérifier ce qu'une passe a réellement posé dans le vault (issue #152).
fn promotions(user: &str) -> String {
    let lines = candidate_lines(user);
    let tri: Vec<String> = (1..=lines.len())
        .map(|k| {
            format!(
                r#"{{"candidat": {k}, "durable": true, "utile": true, "precis": true,
                      "introuvable": true, "endosse": true, "justification": "fait stable"}}"#
            )
        })
        .collect();
    // Le texte écrit reprend celui du candidat : deux lots ne doivent pas proposer la
    // même entrée, sinon c'est la garde anti-doublon qu'on mesure, pas l'écriture.
    let ops: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, text)| {
            format!(
                r#"{{"op": "add_entry", "candidat": {}, "file": "projets.md",
                      "section": "Infrastructure", "text": "{text}",
                      "importance": 7, "declencheurs": ["port"]}}"#,
                i + 1
            )
        })
        .collect();
    format!(
        r#"{{"tri": [{}], "operations": [{}]}}"#,
        tri.join(","),
        ops.join(",")
    )
}

/// Textes des candidats d'un prompt de consolidation, dans l'ordre où ils sont
/// soumis.
fn candidate_lines(user: &str) -> Vec<String> {
    user.lines()
        .filter_map(|l| {
            let (n, rest) = l.split_once(". [")?;
            (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then(|| {
                rest.split_once("] ")
                    .map(|(_, t)| t.trim().to_string())
                    .unwrap_or_default()
            })
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// Réponse « gardé » d'un lot d'un candidat, avec son écriture dans `profil.md`.
fn keep(text: &str) -> String {
    format!(
        r#"{{"tri": [{{"candidat": 1, "durable": true, "utile": true, "precis": true,
                "introuvable": true, "endosse": true, "justification": "règle dite"}}],
              "operations": [{{"op": "add_entry", "candidat": 1, "file": "profil.md",
                "section": "Préférences", "text": "{text}", "importance": 7,
                "declencheurs": ["règle"]}}]}}"#
    )
}

fn passing_error() -> penelope_llm::mock::Scripted {
    penelope_llm::mock::Scripted::Error(
        LlmErrorKind::Transient,
        "Upstream idle timeout exceeded (NextBit)".into(),
    )
}

#[derive(Default)]
struct Recorder(std::sync::Mutex<Vec<String>>);

#[async_trait::async_trait]
impl crate::executor::Messenger for Recorder {
    async fn send_text(&self, _o: &crate::bus::Origin, markdown: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(markdown.to_string());
        Ok(())
    }
    async fn send_file(
        &self,
        _o: &crate::bus::Origin,
        _p: &std::path::Path,
        _c: Option<&str>,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// Modèle simulé de #140 : `easy` tokens par candidat, `hard` pour un candidat
/// « épineux », coupé à `max_tokens` comme un vrai fournisseur ; au-delà de `garble`
/// candidats, une réponse illisible quelle que soit la sortie.
fn verbose_model(easy: u64, hard: u64, garble: usize) -> penelope_llm::mock::Responder {
    Arc::new(move |req: &ChatRequest| {
        let user = req.messages.last().map(|m| m.text()).unwrap_or_default();
        let lines: Vec<&str> = user
            .lines()
            .filter(|l| {
                l.split_once(". [")
                    .is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_digit()))
            })
            .collect();
        let n = lines.len() as u64;
        let heavy = lines.iter().filter(|l| l.contains("épineux")).count() as u64;
        let need = easy * (n - heavy) + hard * heavy;
        // La requête partage son plafond : `reasoning.max_tokens` pour réfléchir, le
        // reste pour écrire (issue #152). Ce modèle-ci ne réfléchit pas, mais il
        // honore le partage demandé — c'est la sortie utile que #140 mesure.
        let limit = req.max_tokens.map_or(u64::MAX, |m| {
            u64::from(m.saturating_sub(req.reasoning_max_tokens.unwrap_or(0)))
        });
        let partial = r#"{"tri": [{"candidat": 1, "dur"#.to_string();
        if lines.len() > garble {
            return penelope_llm::mock::Scripted::Written {
                text: partial,
                completion: easy,
                cut: false,
            };
        }
        if need > limit {
            return penelope_llm::mock::Scripted::Written {
                text: partial,
                completion: limit,
                cut: true,
            };
        }
        let tri: Vec<String> = (1..=n)
            .map(|k| {
                format!(
                    r#"{{"candidat": {k}, "durable": false, "utile": false, "precis": true,
                          "introuvable": true, "endosse": true, "justification": "passager"}}"#
                )
            })
            .collect();
        penelope_llm::mock::Scripted::Written {
            text: format!(r#"{{"tri": [{}], "operations": []}}"#, tri.join(",")),
            completion: need,
            cut: false,
        }
    })
}

/// Verdicts « rien à garder » pour `n` candidats : de quoi faire aboutir un lot sans
/// écrire dans le vault.
fn verdicts(n: u64) -> String {
    let tri: Vec<String> = (1..=n)
        .map(|k| {
            format!(
                r#"{{"candidat": {k}, "durable": false, "utile": false, "precis": true,
                      "introuvable": true, "endosse": true, "justification": "passager"}}"#
            )
        })
        .collect();
    format!(r#"{{"tri": [{}], "operations": []}}"#, tri.join(","))
}

/// Candidats de #140 : `n` projets distincts, jugés dans l'ordre, épineux quand
/// `thorny` le dit.
async fn projects(d: &Arc<Daemon>, n: usize, thorny: impl Fn(usize) -> bool) {
    for k in 0..n {
        let thorny = if thorny(k) { ", cas épineux" } else { "" };
        note(
            d,
            CandidateType::Preference,
            &format!("Pour le projet{k:03}, le propriétaire décide seul et sans réunion{thorny}"),
            Origin::Owner,
            "s1",
            6,
        )
        .await;
    }
}

/// `(taille, « rien à en tirer »)` : coupé, ou affamé de raisonnement (#152) — dans
/// les deux cas le lot n'a pas jugé ses candidats.
fn dream_batches(events: &[penelope_kernel::event::Event]) -> Vec<(u64, bool)> {
    events
        .iter()
        .filter(|e| e.kind == "memory.dream_batch")
        .map(|e| {
            (
                e.payload["size"].as_u64().unwrap(),
                e.payload["truncated"] == true || e.payload["reasoning_starved"] == true,
            )
        })
        .collect()
}

/// Numéro d'un candidat dans l'ordre soumis au modèle.
async fn number(s: &Services, needle: &str) -> usize {
    let order = submission_order(s).await.unwrap();
    order
        .iter()
        .position(|t| t.contains(needle))
        .unwrap_or_else(|| panic!("« {needle} » absent de {order:?}"))
        + 1
}
