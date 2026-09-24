# Consigne commune des agents de la V1

Texte donné à chaque agent de lot (une branche `v1-<nom>` et un worktree par agent), tel
qu'il était à la vague 9. Les lignes « (vague N) » sont à mettre à jour à chaque vague ; le
chemin `scratchpad/` est celui de la session qui l'a écrit.

RÈGLES COMMUNES (à respecter à la lettre)
- Tu travailles SEUL dans ton worktree ; d'autres agents travaillent en parallèle dans d'autres worktrees. Ne sors JAMAIS de ton périmètre de fichiers : c'est ce qui évite les conflits.
- N'entre jamais dans /Users/edouard/Code/agent/penelope (arbre principal d'une autre session) ni dans /Users/edouard/Code/agent/penelope-wt/v1 (intégration) ; n'y lance jamais cargo.
- Interdits : `git push origin main`, `git checkout main`, `make bump`, modifier une ligne `version =`, créer un tag, rebaser ta branche (l'intégrateur le fait), `cargo clean` hors de ton worktree.
- Lis d'abord CLAUDE.md de ton worktree (sections « Gel de la dette » et « Travailler sur v1 ») et design/v1/README.md (la charte).
- Gel : `crates/penelope-archtest/budget.toml` ne se modifie QUE par `UPDATE_BUDGET=1 cargo test -p penelope-archtest` (qui ne fait que descendre). Si une règle du gel devient rouge à cause de ton travail (fichier qui grossit au-delà de sa borne, nouveau module), découpe plutôt que d'ajouter ; aucun fichier nouveau au-dessus de 800 lignes.
- Déplacements de code = commits sans changement de corps, séparés des commits qui changent une signature ou un comportement.
- Commits en français : ce qui change et pourquoi, avec l'issue ou « (épopée #208, lot X, tâche Tn) ». Pas de ligne d'attribution. Pousse ta branche après chaque commit vert.
- Vérifications : pendant l'itération, builds ciblés (`-p`, `--test`) ; à la fin, UNE fois : `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Sur ce Mac les tests macOS compilent.
- Disque : `export CARGO_PROFILE_DEV_DEBUG=line-tables-only` avant tout cargo ; avant chaque build, `df -g / | awk 'NR==2{print $4}'` ≥ 25 et absence de /private/tmp/claude-501/-Users-edouard-Code-agent-penelope/28fc3c9e-56ef-477e-929a-0af64b8dc5e3/scratchpad/DISK-STOP. À la fin : `cargo clean` dans ton worktree.
- IMPORTANT, leçon de la nuit dernière : ne lance JAMAIS une commande longue « en arrière-plan » en attendant une notification ; les notifications peuvent ne pas arriver. Lance tes `cargo test --workspace` au premier plan, avec un délai long.
- Notes : design/v1/notes/<ton-nom>.md en français, sans tiret cadratin « — » : ce qui est livré, les choix, la section de notes de version prête pour docs/progress.md (titre `#### …`), les blocages. Commite-les.
- Rapport final : 20 lignes maximum (commits, vérifications, reste, blocages). Le dépôt est le livrable.
- (vague 9) Base : `v1` à la 1.0.0-alpha.9. HEURE LIMITE STRICTE : ton lot doit être commité, vérifié et poussé avant 22h35 (heure de la machine, `date`). Si le temps manque, livre moins mais vert : une tâche finie vaut mieux que deux à moitié ; écris ce qui reste dans tes notes. Rouges tolérés en fin de lot, à signaler seulement : `crates_stay_under_their_ceiling` (plafond du crate daemon) et la liste blanche R5 si tu crées un module de premier niveau du daemon prévu par ta tâche ; l'intégrateur ajuste `budget.toml`. Tout autre rouge est à corriger.
- (vague 8) `runtime.rs` et `supervisor.rs` sont touchés par plusieurs agents : n'y ajoute que des appels d'une ligne vers des fonctions qui vivent dans ton module ; ne déplace rien dedans sauf si ta tâche le demande explicitement.
- Le test `the_budget_file_is_readable` n'impose plus de taille minimale à la liste de référence : sortir un fichier de la liste est toujours bienvenu.
- Crate nouvelle : ajoute-la au workspace et aux listes d'archtest, avec `version` alignée sur le workspace (bump.sh compte les lignes, pas besoin d'y toucher). Plafond `[crates]` du daemon : laisse-le rouge, l'intégrateur le pose.
