#!/bin/sh
# Passe le workspace à une nouvelle version (issue #147).
#
#   scripts/bump.sh 0.17.31     ou    make bump V=0.17.31
#
# Réécrit les seize lignes de `Cargo.toml` (la version du workspace et les quinze
# dépendances internes), met `Cargo.lock` à jour, vérifie que `docs/progress.md` a bien la
# section de cette version, et commite « Version x.y.z ». Le tag et la release sont posés
# par la CI : rien à faire de plus.
set -eu

V="${1:-}"
case "$V" in
    '') echo "usage : scripts/bump.sh <version>   (exemple : 0.17.31)" >&2; exit 2 ;;
    v*) echo "donner la version sans « v » : ${V#v}" >&2; exit 2 ;;
esac
echo "$V" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' || {
    echo "version « $V » : attendu x.y.z" >&2; exit 2; }

cd "$(dirname "$0")/.."

[ -z "$(git status --porcelain)" ] || {
    echo "l'arbre de travail n'est pas propre : commiter ou ranger d'abord" >&2; exit 1; }

grep -q "^### ${V}$" docs/progress.md || {
    echo "docs/progress.md n'a pas de section « ### ${V} » : écrire les notes de la" >&2
    echo "version avant de la poser (une issue fermée = une section)." >&2; exit 1; }

CURRENT=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
[ "$CURRENT" != "$V" ] || { echo "le workspace est déjà en $V" >&2; exit 1; }

# Les seize occurrences sont toutes celles de la version courante dans Cargo.toml : la
# ligne `[workspace.package]` et les quinze `penelope-* = { path = …, version = … }`.
sed -i.bak "s/\"${CURRENT}\"/\"${V}\"/g" Cargo.toml && rm -f Cargo.toml.bak
n=$(grep -c "\"${V}\"" Cargo.toml)
[ "$n" -eq 16 ] || { echo "Cargo.toml : $n lignes réécrites, 16 attendues" >&2; exit 1; }

cargo update -w --offline
git add Cargo.toml Cargo.lock
git commit -m "Version ${V}"
echo
echo "Version ${V} commitée. 'git push' : la CI pose le tag v${V} et publie la release."
