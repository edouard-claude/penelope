#!/bin/sh
# Les critères de bascule de v1 vers main (tâche T16, design/v1/gel-et-outillage.md §4.5).
#
#   scripts/switch-check.sh            sur l'arbre courant, branche CI « v1 »
#   SWITCH_BRANCH=v1 scripts/switch-check.sh
#
# Vérifie mécaniquement ce qui se mesure : la CI de v1 verte sur son dernier commit
# (points 1 et 6, le test docs en fait partie), budget.toml sans dette (point 2), les
# [ca].required tous présents (point 3), le filet de migration sur la dernière 0.17
# (point 4, sa partie automatique), les scénarios complets (point 7, sa partie
# budget.toml). Liste chaque critère manquant, puis rappelle ceux qui ne se vérifient
# qu'à la main (l'essai réel du point 4, les suites réseau du point 5).
#
# Sortie : 0 tous les critères mesurables sont remplis, 1 il en manque (l'état normal
# jusqu'à la bascule), 2 impossible de lire le budget.
set -eu

cd "$(dirname "$0")/.."
BUDGET=crates/penelope-archtest/budget.toml
BRANCH="${SWITCH_BRANCH:-v1}"
[ -f "$BUDGET" ] || { echo "switch-check : $BUDGET absent" >&2; exit 2; }

missing=0
ok() { echo "  ok       $1"; }
ko() { echo "  MANQUE   $1"; missing=$((missing + 1)); }

# Les lignes d'une table de budget.toml (sans commentaires ni lignes vides).
table() {
    awk -v t="[$1]" '
        /^\[/ { inside = ($0 == t); next }
        inside { sub(/[ \t]*#.*/, ""); if ($0 ~ /[^ \t]/) print }' "$BUDGET"
}
# Les chaînes d'un tableau `clé = [ … ]` d'une table, une par ligne.
array() {
    table "$1" | awk -v k="$2" '
        $0 ~ "^" k "[ \t]*=" { on = 1 }
        on { s = $0; while (match(s, /"[^"]*"/)) { print substr(s, RSTART + 1, RLENGTH - 2);
             s = substr(s, RSTART + RLENGTH) } if ($0 ~ /\]/) exit }'
}

echo "Bascule de $BRANCH vers main : critères mesurables (§4.5)"

# 1 et 6. La CI de la branche, verte sur son dernier commit poussé.
if ! command -v gh > /dev/null 2>&1; then
    ko "1. CI de $BRANCH : gh absent, statut illisible"
else
    git fetch --quiet origin "$BRANCH" 2> /dev/null || true
    tip=$(git rev-parse -q --verify "origin/$BRANCH" 2> /dev/null || true)
    if run=$(gh run list --branch "$BRANCH" --workflow ci.yml --event push --limit 1 \
            --json status,conclusion,headSha,url \
            --jq '.[0] | "\(.status)|\(.conclusion)|\(.headSha)|\(.url)"' 2> /dev/null) \
        && [ -n "$run" ] && [ "$run" != "null|null|null|null" ]; then
        IFS='|' read -r st concl sha url << EOF2
$run
EOF2
        set -- "$st" "${concl:-aucune}" "$sha" "$url"
        if [ -n "$tip" ] && [ "$3" != "$tip" ]; then
            ko "1. CI de $BRANCH : le dernier run ($(echo "$3" | cut -c1-7)) n'est pas sur origin/$BRANCH ($(echo "$tip" | cut -c1-7)) : $4"
        elif [ "$1" != completed ]; then
            ko "1. CI de $BRANCH : dernier run en cours ($1) : $4"
        elif [ "$2" != success ]; then
            ko "1. CI de $BRANCH : dernier run « $2 » : $4"
        else
            ok "1. CI de $BRANCH verte sur $(echo "$3" | cut -c1-7) (tests, verification, dependances, docs)"
        fi
    else
        ko "1. CI de $BRANCH : aucun run lisible par gh run list (gh auth status ?)"
    fi
fi

# 2. budget.toml : plus de dette.
oversized=$(table files.oversized | grep -c '=' || true)
if [ "$oversized" -eq 0 ]; then
    ok "2. [files.oversized] vide"
else
    ko "2. [files.oversized] : $oversized fichier(s) au-dessus de 1 000 lignes, dont $(table files.oversized | head -1 | cut -d'"' -f2)"
fi
impl=$(array daemon impl_daemon)
extra=$(table daemon.daemon_users | cut -d'"' -f2 | while IFS= read -r f; do
    echo "$impl" | grep -qx "$(basename "$f")" || echo "$f"; done)
if [ -z "$extra" ]; then
    ok "2. [daemon.daemon_users] réduit aux fichiers de impl_daemon"
else
    ko "2. [daemon.daemon_users] : $(echo "$extra" | wc -l | tr -d ' ') fichier(s) hors impl_daemon nomment encore Daemon, dont $(echo "$extra" | head -1)"
fi
allows=$(table lints | sed -n 's/^allow_too_many_lines[ \t]*=[ \t]*\([0-9]*\).*/\1/p')
if [ "${allows:-x}" = 0 ]; then
    ok "2. [lints].allow_too_many_lines = 0"
else
    ko "2. [lints].allow_too_many_lines = ${allows:-absent} (attendu 0)"
fi

# 3. Les CA figés : au moins les 71 du gel, chacun présent comme fonction dans crates/.
required=$(array ca required)
count=$(echo "$required" | grep -c . || true)
absent=$(echo "$required" | while IFS= read -r ca; do
    [ -z "$ca" ] || grep -rqE --include='*.rs' "fn ${ca}[[:space:]]*\(" crates || echo "$ca"; done)
if [ "$count" -lt 71 ]; then
    ko "3. [ca].required : $count noms, 71 attendus au moins"
elif [ -n "$absent" ]; then
    ko "3. [ca].required : $(echo "$absent" | wc -l | tr -d ' ') introuvable(s), dont $(echo "$absent" | head -1)"
else
    ok "3. [ca].required : les $count critères sont présents (matrice vérifiée par la CI)"
fi

# 4. Le filet de migration, sur une fixture de la dernière 0.17 publiée dans progress.md.
last=$(sed -n 's/^### \(0\.17\.[0-9]*\)$/\1/p' docs/progress.md | sort -t. -k3 -n | tail -1)
test_file=crates/penelope-store/tests/migration_from_0_17.rs
fixture=crates/penelope-store/tests/fixtures/penelope-$last.db
if [ ! -f "$test_file" ]; then
    ko "4. test de migration absent ($test_file)"
elif [ ! -f "$fixture" ]; then
    have=$(for f in crates/penelope-store/tests/fixtures/penelope-0.17.*.db; do
        [ -f "$f" ] && basename "$f"; done | tr '\n' ' ' | sed 's/ $//')
    ko "4. fixture de la dernière 0.17 absente : $fixture (présentes : ${have:-aucune})"
else
    ok "4. test de migration et fixture penelope-$last.db présents"
fi

# 7. Les scénarios rejouables : [scenarios].missing vide.
if ! grep -q '^\[scenarios\]' "$BUDGET"; then
    ko "7. [scenarios] absent de budget.toml (R10, R11 pas encore posées)"
elif [ -n "$(array scenarios missing)" ]; then
    ko "7. [scenarios].missing : $(array scenarios missing | wc -l | tr -d ' ') scénario(s) manquant(s)"
else
    ok "7. [scenarios].missing vide"
fi

cat << 'EOF'
À vérifier à la main (hors de ce script) :
  4. essai réel : fusion de v1 dans main, make bump V=1.0.0-rc.1 avec V1_RELEASES=1,
     installation par penelope upgrade --tag sur le MBP, penelope doctor propre, 24 h
  5. suites réseau lancées sur v1, résultats au moins égaux à la ligne de base (T12)
  7. planchers de couverture de v1 au moins ceux de main au point de fourche (R9)
EOF

if [ "$missing" -gt 0 ]; then
    echo "switch-check : $missing critère(s) mesurable(s) manquant(s), pas de bascule"
    exit 1
fi
echo "switch-check : tous les critères mesurables sont remplis"
