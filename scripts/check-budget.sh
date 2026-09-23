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

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

git show "HEAD:$BUDGET" > "$tmp/head.toml" 2>/dev/null || {
    echo "check-budget : $BUDGET absent de HEAD ; le gel ne se retire pas" >&2; exit 1; }
git show "$base:$BUDGET" > "$tmp/base.toml" 2>/dev/null || {
    echo "check-budget : $BUDGET absent de $short, rien à comparer"; exit 0; }

# Renommages « ancien<TAB>nouveau » : une entrée suit son fichier sous le nouveau chemin.
git diff --name-status --find-renames "$base" HEAD -- crates \
    | awk -F'\t' '$1 ~ /^R/ { print $2 "\t" $3 }' > "$tmp/renames.tsv"

set +e
out=$(cargo run --quiet -p penelope-archtest --bin check-budget -- \
    "$tmp/base.toml" "$tmp/head.toml" "$tmp/renames.tsv" "$short")
status=$?
set -e
case $status in
    0) echo "check-budget : rien ne remonte dans $BUDGET par rapport à $short"; exit 0 ;;
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
    exit 0
fi
echo "check-budget : $BUDGET remonte par rapport à $short sans « Dérogation-budget: #N »" >&2
echo "  dans un commit de $short..HEAD ; abaisser, ou poser le trailer si c'est décidé" >&2
exit 1
