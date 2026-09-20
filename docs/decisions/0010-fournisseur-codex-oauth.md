# 0010 — Fournisseur Codex : identité empruntée, périmètre du propriétaire, quota du plan

Statut : acceptée (19 septembre 2026). Portée : §10.1 (providers), §10.3 (routage), §13.1
(secrets), §9 (budget).

## Contexte

Pénélope ne savait parler aux modèles que par deux portes, toutes deux à clé d'API :
OpenRouter et les endpoints « compatibles OpenAI ». Un abonnement ChatGPT (Plus, Pro,
Business) n'en ouvre aucune : il ne donne ni clé d'API ni crédits Platform. Il ouvre en
revanche celle de Codex, `https://chatgpt.com/backend-api/codex`, par « Sign in with
ChatGPT » : les modèles du plan, 272 k de contexte, outils et images, sans facturation à
l'appel.

OpenAI tolère publiquement que d'autres harnais que Codex CLI l'empruntent — la page
« Codex for Open Source » nomme OpenCode, Cline, pi et OpenClaw, et des déclarations
publiques chiffrent leur part du trafic. Rien de tout cela n'est contractuel.

Trois questions se posaient, chacune tranchée ci-dessous : sous quelle identité appeler,
pour quels tours, et comment compter.

## Décision

1. **L'identité est empruntée, et c'est dit.** Le backend filtre l'en-tête `originator`
   (liste blanche) et sert un catalogue qui en dépend : un client honnête reçoit 403 sur
   toutes ses requêtes. Pénélope envoie donc `originator: codex_cli_rs`, un `User-Agent`
   cohérent et un identifiant d'installation stable, **la même identité sur toutes les
   requêtes** — une identité incohérente a valu des heures de « servers overloaded » à un
   client tiers. C'est une usurpation assumée : elle est documentée comme telle dans
   `install-headless.md`, et `penelope doctor` porte un avertissement permanent qui ne se
   tait jamais. Tout est en configuration (`providers.codex.originator`,
   `client_version`) : une liste blanche qui change se rattrape sans recompiler, et un 403
   nomme la cause probable.

2. **L'abonnement ne sert que les tours du propriétaire.** OpenAI tolère « un compte, un
   humain, un usage interactif » et traque la conversion d'un abonnement en trafic
   automatisé. Le fournisseur `codex` ne sert donc que les tours ouverts par le
   propriétaire — un message Telegram ou CLI, et les sous-agents de ce tour. Tout ce qui
   tourne sans lui — planification à cible `prompt`, rêve nocturne, veille, résumeur de
   compaction, relecture d'épisode, consolidation, classifieur, embeddings, transcription,
   synthèse vocale, titre automatique, run de workflow — se replie sur le modèle OpenRouter
   de l'alias, sans carte ni bruit, en laissant l'événement `llm.codex_scope_fallback`.
   **Un seul compte**, jamais de rotation ni de second jeu de jetons : `penelope model auth
   codex` refuse un second compte tant que le premier n'est pas déconnecté.

3. **Le compteur est le quota du plan, pas le dollar.** Un appel par abonnement coûte 0 $ :
   les plafonds `budget.*` ne comptent rien pour ce fournisseur et ne le freinent donc pas.
   La vraie limite est le quota du plan, que le backend annonce à chaque réponse (une
   fenêtre de cinq heures, une fenêtre hebdomadaire). Pénélope les range en `kv`, alerte
   une fois par fenêtre à `quota_alert_ratio`, et se met en retrait à `quota_stop_ratio` :
   le fournisseur répond alors `RateLimited` **avant** l'appel, le routeur se replie, et le
   message distingue un quota atteint d'une panne (issue #139). Les lignes d'usage portent
   `provider = codex`, `cost_usd = 0`, `estimated = false` — le coût est connu, il vaut
   zéro.

## Raisons

Emprunter une identité sans le dire serait le genre de dette qu'on découvre le jour où
elle casse. L'écrire dans l'ADR, dans la documentation et dans `doctor` rend le compromis
visible : celui qui installe Pénélope sait ce qu'il accepte, et sait quoi faire le jour où
la porte se ferme (une clé d'API sur `openai_compat` vers `api.openai.com`).

Borner le périmètre aux tours du propriétaire n'est pas une politesse : c'est ce qui
distingue un usage interactif d'une conversion d'abonnement en API, exactement ce
qu'OpenAI dit traquer. La garde est unique et se pose juste avant le choix du fournisseur,
pour qu'aucun chemin ne l'oublie ; `penelope model set` refuse d'en arriver là, et
`doctor` signale une configuration qui l'aurait contournée.

Compter en dollars un appel qui n'en coûte pas donnerait un budget toujours vert et une
panne surprise : les jauges du plan sont la seule limite qui existe, donc la seule qui
mérite une alerte.

## Conséquences

- `penelope-llm` gagne `codex.rs` (dialecte Responses, accumulateur d'événements, jauges)
  et un trait `TokenSource` : la connexion, sa rotation et son verrou restent au daemon, ce
  qui évite une dépendance de `penelope-llm` vers `penelope-mcp` (test d'architecture).
- Le `refresh_token` est **à usage unique** : les rotations sont sérialisées par un verrou
  de processus et écrites avant tout usage. Un jeton rejoué (`refresh_token_reused`)
  déconnecte le compte pour de bon ; Pénélope le dit et se replie.
- Livré derrière `providers.codex.enabled = false` : sans compte connecté, rien ne change.
- Si OpenAI ferme la porte, la sortie est déjà écrite : `penelope model auth codex
  --logout`, et un `openai_compat` vers `api.openai.com` avec une clé d'API.
