#!/bin/sh
# Passe le workspace à une nouvelle version (issue #147).
#
#   scripts/bump.sh 0.17.31     ou    make bump V=0.17.31
#
# Réécrit les lignes de version de `Cargo.toml` (la version du workspace et chaque
# dépendance interne), met `Cargo.lock` à jour, vérifie que `docs/progress.md` a bien la
# section de cette version, et commite « Version x.y.z ». Le tag et la release sont posés
# par la CI : rien à faire de plus. Une version à suffixe (`1.0.0-alpha.N`, branche v1)
# n'a ni tag ni release (#212).
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

# Les occurrences de la version courante dans Cargo.toml : la ligne `[workspace.package]`
# et une par `penelope-* = { path = …, version = … }`. Leur nombre suit les crates du
# workspace (dix-sept avec penelope-app) ; elles doivent toutes être réécrites.
expected=$(grep -c "\"${CURRENT}\"" Cargo.toml)
sed -i.bak "s/\"${CURRENT}\"/\"${V}\"/g" Cargo.toml && rm -f Cargo.toml.bak
n=$(grep -c "\"${V}\"" Cargo.toml)
[ "$n" -eq "$expected" ] || {
    echo "Cargo.toml : $n lignes réécrites, $expected attendues" >&2; exit 1; }

cargo update -w --offline
git add Cargo.toml Cargo.lock
git commit -m "Version ${V}"
echo
case "$V" in
    # Une version à suffixe vit sur la branche v1, jamais taguée (#212) : `livraison` est
    # réservé à main et release.yml refuse un tag v1* avant la bascule.
    *-*) echo "Version ${V} commitée. 'git push' : branche v1 : aucun tag, aucune release." ;;
    *) echo "Version ${V} commitée. 'git push' : la CI pose le tag v${V} et publie la release." ;;
esac
