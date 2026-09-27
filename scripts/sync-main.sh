#!/bin/sh
# Fusionne origin/main dans la branche v1 (tâche T14 de design/v1/gel-et-outillage.md §4.4).
#
#   scripts/sync-main.sh                     fusionne, résout Cargo.*, teste, commite
#   scripts/sync-main.sh --dry-run           dit ce que la fusion ferait, sans rien toucher
#   scripts/sync-main.sh --continue          reprend une fusion arrêtée, une fois résolue
#   scripts/sync-main.sh --derogation 217    commite avec « Dérogation-budget: #217 »
#
# Chaque bump 0.17.x de main réécrit les lignes de version de Cargo.toml et les
# paquets penelope-* de Cargo.lock, que v1 porte en 1.0.0-alpha.N : ces deux fichiers
# conflictent à chaque fusion. Le script rejoue leur fusion à trois voies après avoir
# ramené les trois côtés (base, v1, main) à la version de v1 : seul ce qui diffère
# vraiment reste, les autres changements de main (dépendances, crates) sont gardés. Puis
# `cargo update -w --offline` et `cargo test -p penelope-archtest` : si le gel est rouge
# (main a fait grossir un fichier que v1 a déjà serré), rien n'est commité.
#
# Sortie : 0 fusion commitée (ou rien à fusionner), 1 à reprendre à la main (conflits
# restants ou archtest rouge, la fusion reste en cours), 2 impossible de commencer.
set -eu

cd "$(dirname "$0")/.."
MAIN_REF=origin/main
BUDGET=crates/penelope-archtest/budget.toml

mode=merge
derogation=
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) mode=dry ;;
        --continue) mode="continue" ;;
        --derogation)
            [ $# -ge 2 ] || { echo "--derogation attend un numéro d'issue" >&2; exit 2; }
            derogation="${2#\#}"; shift ;;
        -h|--help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "option inconnue : $1 (voir --help)" >&2; exit 2 ;;
    esac
    shift
done
case "$derogation" in
    ''|*[!0-9]*) [ -z "$derogation" ] || { echo "--derogation : numéro d'issue attendu" >&2; exit 2; } ;;
esac

# La version du workspace dans un Cargo.toml lu sur l'entrée standard.
workspace_version() { sed -n 's/^version = "\(.*\)"$/\1/p' | head -1; }
escape() { printf '%s' "$1" | sed 's/[.[\*^$/]/\\&/g'; }

# normalize FICHIER DE VERS : réécrit la version DE en VERS, comme bump.sh. Dans
# Cargo.toml, toutes les occurrences entre guillemets ; dans Cargo.lock, la ligne
# `version` qui suit un `name = "penelope-…"` seulement (un paquet externe peut porter
# le même numéro).
normalize() {
    from=$(escape "$2")
    case "$1" in
        *Cargo.lock)
            awk -v from="$2" -v to="$3" '
                /^name = "penelope-/ { ours = 1; print; next }
                ours && $0 == "version = \"" from "\"" { print "version = \"" to "\""; ours = 0; next }
                /^\[\[package\]\]/ { ours = 0 }
                { print }' "$1" > "$1.norm" && mv "$1.norm" "$1" ;;
        *) sed "s/\"${from}\"/\"$3\"/g" "$1" > "$1.norm" && mv "$1.norm" "$1" ;;
    esac
}

# resolve FICHIER BASE OURS THEIRS SORTIE : fusion à trois voies des trois révisions de
# FICHIER, versions ramenées à celle de v1. Rend 0 si la fusion est propre.
resolve() {
    file=$1; out=$5; name=$(basename "$file")
    for side in base ours theirs; do
        case $side in base) rev=$2 ;; ours) rev=$3 ;; theirs) rev=$4 ;; esac
        git show "$rev:$file" > "$tmp/$side.$name" 2>/dev/null || : > "$tmp/$side.$name"
    done
    base_v=$(git show "$2:Cargo.toml" 2>/dev/null | workspace_version)
    [ -z "$base_v" ] || [ "$base_v" = "$V1" ] || normalize "$tmp/base.$name" "$base_v" "$V1"
    [ "$MAIN_V" = "$V1" ] || normalize "$tmp/theirs.$name" "$MAIN_V" "$V1"
    cp "$tmp/ours.$name" "$out"
    git merge-file -q "$out" "$tmp/base.$name" "$tmp/theirs.$name"
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

branch=$(git symbolic-ref --quiet --short HEAD) || {
    echo "sync-main : HEAD détachée ; se placer sur v1" >&2; exit 2; }
V1=$(workspace_version < Cargo.toml)
case "$V1" in
    1.0.0-*) ;;
    *) echo "sync-main : le workspace est en « $V1 » ; ce script fusionne main dans v1 (1.0.0-…)" >&2
       exit 2 ;;
esac

# Reprise : la fusion est en cours, l'utilisateur a résolu ce qui restait.
if [ "$mode" = continue ]; then
    git rev-parse -q --verify MERGE_HEAD > /dev/null || {
        echo "sync-main : aucune fusion en cours à reprendre" >&2; exit 2; }
    theirs=$(git rev-parse MERGE_HEAD)
    MAIN_V=$(git show "$theirs:Cargo.toml" | workspace_version)
else
    [ "$mode" = dry ] || [ -z "$(git status --porcelain --untracked-files=no)" ] || {
        echo "sync-main : l'arbre de travail n'est pas propre ; commiter ou ranger d'abord" >&2
        exit 2; }
    ! git rev-parse -q --verify MERGE_HEAD > /dev/null || {
        echo "sync-main : une fusion est déjà en cours ; la finir (--continue) ou" >&2
        echo "  l'abandonner (git merge --abort)" >&2; exit 2; }
    git fetch --quiet origin main || { echo "sync-main : git fetch origin main a échoué" >&2; exit 2; }
    theirs=$(git rev-parse "$MAIN_REF")
    MAIN_V=$(git show "$theirs:Cargo.toml" | workspace_version)
    if git merge-base --is-ancestor "$theirs" HEAD; then
        echo "sync-main : $MAIN_REF ($MAIN_V) est déjà dans $branch, rien à fusionner"
        exit 0
    fi
fi
base=$(git merge-base HEAD "$theirs")
echo "sync-main : $MAIN_REF $MAIN_V ($(git rev-parse --short "$theirs")) dans $branch $V1"

# Les conflits que la fusion aurait sans rien résoudre, sans toucher à l'arbre.
if [ "$mode" = dry ]; then
    if git merge-tree --write-tree --name-only --no-messages HEAD "$theirs" > "$tmp/tree"; then
        echo "  fusion sans conflit"
    fi
    tail -n +2 "$tmp/tree" | sed '/^$/d' > "$tmp/conflicts"
    others=0
    while IFS= read -r f; do
        case "$f" in
            Cargo.toml|Cargo.lock)
                if resolve "$f" "$base" HEAD "$theirs" "$tmp/out"; then
                    echo "  $f : conflit de version, résolu par le script"
                else
                    echo "  $f : conflit hors des lignes de version, à résoudre à la main"
                    others=$((others + 1))
                fi ;;
            "$BUDGET")
                echo "  $f : à résoudre à la main, valeur la plus stricte clé par clé (CLAUDE.md)"
                others=$((others + 1)) ;;
            *) echo "  $f : à résoudre à la main"; others=$((others + 1)) ;;
        esac
    done < "$tmp/conflicts"
    echo "  $(git rev-list --count HEAD.."$theirs") commits de main à fusionner"
    if [ "$others" -eq 0 ]; then
        echo "sync-main --dry-run : la fusion passerait sans intervention"
    else
        echo "sync-main --dry-run : $others fichier(s) à résoudre à la main"
    fi
    exit 0
fi

if [ "$mode" = merge ]; then
    if git merge --no-ff --no-commit "$theirs" > "$tmp/merge.log" 2>&1; then
        echo "  fusion sans conflit"
    else
        git rev-parse -q --verify MERGE_HEAD > /dev/null || {
            cat "$tmp/merge.log" >&2; echo "sync-main : git merge a échoué" >&2; exit 2; }
    fi

    # Cargo.toml et Cargo.lock : la fusion rejouée sur des versions ramenées à celle de v1.
    for f in Cargo.toml Cargo.lock; do
        git diff --name-only --diff-filter=U -- "$f" | grep -q . || continue
        if resolve "$f" "$base" HEAD "$theirs" "$tmp/out"; then
            cp "$tmp/out" "$f"
            git add "$f"
            echo "  $f : conflit de version résolu ($MAIN_V -> $V1)"
        else
            echo "  $f : des conflits restent hors des lignes de version"
        fi
    done
fi

remaining=$(git diff --name-only --diff-filter=U)
if [ -n "$remaining" ]; then
    echo "sync-main : fusion arrêtée, conflits à résoudre à la main :" >&2
    echo "$remaining" | sed 's/^/  /' >&2
    case "$remaining" in *"$BUDGET"*)
        echo "  $BUDGET : la valeur la plus stricte clé par clé (minimum des plafonds," >&2
        echo "  intersection des listes de dette, union de [ca].required), jamais un seul côté" >&2 ;;
    esac
    echo "  puis : git add <fichiers> && scripts/sync-main.sh --continue" >&2
    echo "  (ou git merge --abort ; une fusion en conflit depuis plus de 24 h bloque v1)" >&2
    exit 1
fi

# La version de v1 partout, les dépendances de main gardées.
[ "$(workspace_version < Cargo.toml)" = "$V1" ] || {
    echo "sync-main : Cargo.toml n'est plus en $V1 après la fusion" >&2; exit 1; }
if [ "$MAIN_V" != "$V1" ] && grep -q "\"$(escape "$MAIN_V")\"" Cargo.toml; then
    echo "sync-main : Cargo.toml contient encore « $MAIN_V »" >&2; exit 1
fi
cargo update -w --offline
git add Cargo.toml Cargo.lock

if ! cargo test -q -p penelope-archtest > "$tmp/archtest.log" 2>&1; then
    grep -E 'panicked|budget|^test .* FAILED|attendu|passe de' "$tmp/archtest.log" | head -40 >&2 || true
    cat >&2 <<EOF

sync-main : cargo test -p penelope-archtest est rouge, rien n'est commité (fusion en cours).
  main a apporté ce que le gel de v1 refuse (un fichier plus long que sa borne, un
  module ou un usage de Daemon de plus). Deux sorties :
  - réduire côté v1 (déplacer un test dans <module>/tests.rs, découper) ;
  - sinon inscrire l'apport de main dans $BUDGET (la borne à la valeur
    mesurée, un commentaire « fusion de main $MAIN_V : #N ») et commiter avec le
    trailer : git add $BUDGET && scripts/sync-main.sh --continue --derogation N
EOF
    exit 1
fi

msg="Fusion de main $MAIN_V dans $branch"
if [ -n "$derogation" ]; then
    git commit -q -m "$msg" -m "Dérogation-budget: #$derogation"
else
    git commit -q -m "$msg"
fi
echo "sync-main : $(git log -1 --format='%h %s')"
echo "  avant de pousser : cargo fmt --all --check, cargo clippy --workspace --all-targets"
echo "  -- -D warnings, cargo test --workspace (CLAUDE.md, « Avant de pousser »)"
