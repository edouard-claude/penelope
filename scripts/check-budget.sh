#!/bin/sh
# Le cliquet de budget.toml (issue #209, règle R3 de design/v1/gel-et-outillage.md §3).
#
#   scripts/check-budget.sh [BASE]
#
# Compare crates/penelope-archtest/budget.toml de HEAD à celui de BASE (un commit ; par
# défaut le point de fourche entre HEAD et origin/$GITHUB_BASE_REF, sinon origin/main) et
# refuse tout ce qui remonte : une valeur plus haute, une entrée ajoutée dans
# [files.oversized], [daemon].modules, [daemon.daemon_users] ou [channel.allowed], une
# suppression dans [ca].required. Un fichier renommé garde son entrée sous son nouveau
# chemin (git diff --find-renames). La dérogation : un commit de la plage porte le
# trailer « Dérogation-budget: #<issue> », auditable par `git log --grep`.
#
# R11 (design/v1/gel-et-outillage.md §R11, épopée #208) : une plage qui touche un
# catalogue de surfaces visibles (commandes Telegram, outils natifs, méthodes RPC) sans
# toucher crates/penelope-evals/scenarios/ est refusée, sauf trailer « Sans-scénario:
# <raison> » (un déplacement, une correction de tests sans changement visible) ou
# « Dérogation-budget: #N ». La couverture elle-même (chaque surface a un scénario) est
# R10, tenue par archtest ; ici, seulement « dans le même lot ».
#
# La comparaison elle-même est faite par le binaire `check-budget` de penelope-archtest
# (crates/penelope-archtest/src/ratchet.rs, couvert par cargo test) ; ici, la partie git.
# Sortie : 0 rien ne remonte (ou dérogation), 1 refus, 2 impossible de comparer.
set -eu

cd "$(dirname "$0")/.."
BUDGET=crates/penelope-archtest/budget.toml

base="${1:-}"
if [ -z "$base" ]; then
    ref="origin/${GITHUB_BASE_REF:-main}"
    base=$(git merge-base HEAD "$ref" 2>/dev/null) || {
        echo "check-budget : pas de point de fourche entre HEAD et $ref (historique tronqué ?)" >&2
        echo "  git fetch origin ${ref#origin/}, ou donner la base en argument" >&2
        exit 2
    }
fi
case "$base" in
    0000000000000000000000000000000000000000)
        echo "check-budget : pas de base (branche nouvelle), rien à comparer"; exit 0 ;;
esac
base=$(git rev-parse --verify --quiet "$base^{commit}") || {
    echo "check-budget : base « ${1:-$base} » introuvable" >&2; exit 2; }
short=$(git rev-parse --short "$base")

# R11 : les catalogues que lit crates/penelope-archtest/src/scenarios.rs (COMMANDS_FILE,
# TOOLS_FILE et leurs sous-modules, API_FILE) ; les fichiers de tests n'en sont pas.
r11=0
catalogs=$(git diff --name-only "$base" HEAD -- \
        crates/penelope-telegram/src/commands.rs crates/penelope-telegram/src/commands \
        crates/penelope-tools/src/spec.rs crates/penelope-tools/src/spec \
        crates/penelope-kernel/src/api.rs \
    | grep -v '/tests\.rs$' || true)
if [ -n "$catalogs" ] \
    && [ -z "$(git diff --name-only "$base" HEAD -- crates/penelope-evals/scenarios)" ]; then
    waived=$(git log --format='%h %s' -i -E \
        --grep='^(Sans-scénario: *[^ ]|Dérogation-budget: *#[0-9])' "$base..HEAD")
    if [ -n "$waived" ]; then
        echo "check-budget : R11, catalogue touché sans scénario, accepté par trailer :"
        echo "$waived" | sed 's/^/  /'
    else
        echo "check-budget : R11, la plage $short..HEAD touche un catalogue de surfaces" >&2
        echo "$catalogs" | sed 's/^/  /' >&2
        echo "  sans toucher crates/penelope-evals/scenarios/ : ajouter ou mettre à jour un" >&2
        echo "  scénario (UPDATE_SCENARIOS=1), ou « Sans-scénario: <raison> » si rien de" >&2
        echo "  visible ne change" >&2
        r11=1
    fi
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

git show "HEAD:$BUDGET" > "$tmp/head.toml" 2>/dev/null || {
    echo "check-budget : $BUDGET absent de HEAD ; le gel ne se retire pas" >&2; exit 1; }
git show "$base:$BUDGET" > "$tmp/base.toml" 2>/dev/null || {
    echo "check-budget : $BUDGET absent de $short, rien à comparer"; exit "$r11"; }

# Renommages « ancien<TAB>nouveau » : une entrée suit son fichier sous le nouveau chemin.
git diff --name-status --find-renames "$base" HEAD -- crates \
    | awk -F'\t' '$1 ~ /^R/ { print $2 "\t" $3 }' > "$tmp/renames.tsv"

set +e
out=$(cargo run --quiet -p penelope-archtest --bin check-budget -- \
    "$tmp/base.toml" "$tmp/head.toml" "$tmp/renames.tsv" "$short")
status=$?
set -e
case $status in
    0) echo "check-budget : rien ne remonte dans $BUDGET par rapport à $short"; exit "$r11" ;;
    1) ;;
    *) echo "check-budget : comparaison impossible (sortie $status)" >&2
       [ -z "$out" ] || echo "$out" >&2
       exit 2 ;;
esac

echo "$out"
derogations=$(git log --format='%h %s' -i --grep='^Dérogation-budget: *#[0-9]' "$base..HEAD")
if [ -n "$derogations" ]; then
    echo "check-budget : accepté par dérogation (trailer « Dérogation-budget » dans la plage) :"
    echo "$derogations" | sed 's/^/  /'
    exit "$r11"
fi
echo "check-budget : $BUDGET remonte par rapport à $short sans « Dérogation-budget: #N »" >&2
echo "  dans un commit de $short..HEAD ; abaisser, ou poser le trailer si c'est décidé" >&2
exit 1
