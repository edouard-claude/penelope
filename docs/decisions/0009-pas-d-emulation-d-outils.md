# 0009 — Pas d'émulation d'outils : un modèle sans tool calling est refusé

Statut : acceptée. Portée : §10.1 (providers), §4.2 (ledger d'effets).

## Contexte

Le §10.1 prévoyait d'émuler le tool calling pour les modèles qui ne le déclarent pas :
décrire les outils dans le prompt système, demander un bloc JSON, le relire avec un
parseur tolérant. Le code existait (`emulation_preamble`, `parse_emulated`,
`needs_tool_emulation`) mais n'avait aucun appelant hors de son crate : trois cents lignes
et neuf tests figeaient un comportement que rien n'exerçait, et la documentation promettait
une fonctionnalité absente.

Branché tel quel, il aurait introduit un défaut de correction : l'appel émulé recevait
l'identifiant `emul_<hash(nom, arguments)>`, qui entre dans la clé d'idempotence du ledger
d'effets. Deux appels identiques dans une même session (`shell_exec {"command": "git
status"}` avant et après une modification) auraient partagé la même clé : le second aurait
été rejoué depuis le premier résultat, sans jamais s'exécuter.

## Décision

1. L'émulation est **retirée**. Le parseur JSON tolérant reste (`json_scan::extract_json`) :
   il sert à relire des arguments d'outil mal formés, ce qui arrive avec tous les modèles.
2. Un modèle dont le catalogue déclare qu'il n'appelle pas d'outils est **refusé** pour un
   alias qui sert un rôle à outils : `penelope model set` répond pourquoi, et `doctor`
   signale une configuration déjà en place.
3. Un modèle absent du catalogue reste accepté : on ne préjuge pas d'un modèle récent que
   le catalogue ne connaît pas encore.
4. Les rôles de service (`stt`, `tts`, `embeddings`, `classifier`, `summarizer`, `titler`,
   `vision`) n'appellent pas d'outils : un modèle sans tool calling y reste bienvenu.

## Raisons

Un agent dont chaque tour passe par des outils n'a rien à gagner à faire semblant avec un
modèle qui ne sait pas les appeler : la boucle devient un analyseur de texte, les erreurs
deviennent silencieuses, et l'idempotence des effets ne tient plus. Mieux vaut un refus qui
dit quoi faire, au moment où le modèle est choisi.

## Conséquences

- `penelope-llm` perd `emulation.rs` ; `json_scan.rs` garde la lecture tolérante.
- `Router::needs_tool_emulation` devient `Router::supports_tools`, qui rend `None` pour un
  modèle inconnu.
- Si un jour un modèle local sans tool calling devient souhaitable, cette décision est à
  rouvrir avec un identifiant d'appel unique (ULID) et un test de bout en bout.
