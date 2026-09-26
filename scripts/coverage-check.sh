#!/bin/sh
# Planchers de couverture par crate (R9, design/v1/gel-et-outillage.md §R9 ; critère 7
# de bascule, §4.5).
#
#   scripts/coverage-check.sh                 mesure et compare aux planchers
#   scripts/coverage-check.sh --update        remonte les planchers à la mesure
#   COVERAGE_LCOV=f.lcov scripts/coverage-check.sh   relit une mesure déjà faite
#   scripts/coverage-check.sh -- --skip nom   arguments passés aux binaires de test
#
# Mesure `cargo llvm-cov --workspace` (couverture de lignes, filtre par défaut de
# cargo-llvm-cov : `tests/`, `tests.rs` et `*_tests.rs` ne comptent pas), additionne les
# lignes par crate et compare à [coverage.crates] de budget.toml : un plancher entier,
# la mesure arrondie à l'inférieur, qui ne descend jamais (cliquet tenu par
# scripts/check-budget.sh). `--update` réécrit une valeur seulement si la mesure arrondie
# la dépasse, et ajoute les crates qui n'en ont pas encore. Les crates listées dans
# [coverage.main] (relevé de main au point de fourche) doivent aussi l'égaler.
#
# Trop long pour la CI (vingt minutes et plus) : lancé à la main, et par
# scripts/switch-check.sh pour le critère 7.
#
# Sortie : 0 tout est au-dessus, 1 une crate est sous son plancher ou sous main,
# 2 impossible de mesurer.
set -eu
export LC_ALL=C

cd "$(dirname "$0")/.."
BUDGET=crates/penelope-archtest/budget.toml
[ -f "$BUDGET" ] || { echo "coverage-check : $BUDGET absent" >&2; exit 2; }

update=0
while [ $# -gt 0 ]; do
    case "$1" in
        --update) update=1; shift ;;
        --) shift; break ;;
        *) echo "coverage-check : argument inconnu « $1 »" >&2; exit 2 ;;
    esac
done

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

lcov="${COVERAGE_LCOV:-}"
if [ -z "$lcov" ]; then
    command -v cargo-llvm-cov > /dev/null 2>&1 || {
        echo "coverage-check : cargo-llvm-cov absent (cargo install cargo-llvm-cov)" >&2
        exit 2
    }
    lcov="$tmp/couverture.lcov"
    echo "coverage-check : cargo llvm-cov --workspace (long)…" >&2
    if ! cargo llvm-cov --workspace --no-fail-fast --lcov --output-path "$lcov" -- "$@" \
            < /dev/null > "$tmp/tests.log" 2>&1; then
        # Un test rouge ne fausse pas la mesure des autres, mais il se dit.
        echo "coverage-check : des tests ont échoué pendant la mesure :" >&2
        grep -E '^(test .* FAILED|failures:|error)' "$tmp/tests.log" | head -20 >&2 || true
        [ -s "$lcov" ] || exit 2
    fi
fi
[ -s "$lcov" ] || { echo "coverage-check : $lcov vide ou absent" >&2; exit 2; }

# Lignes trouvées et couvertes par crate : « crate trouvées couvertes », une par ligne.
awk '
    /^SF:/ { crate = ""; if (match($0, /\/crates\/[^\/]+\//)) crate = substr($0, RSTART + 8, RLENGTH - 9) }
    /^LF:/ && crate != "" { found[crate] += substr($0, 4) }
    /^LH:/ && crate != "" { hit[crate] += substr($0, 4) }
    END { for (c in found) if (found[c] > 0) print c, found[c], hit[c] }' "$lcov" \
    | sort > "$tmp/mesure"

# Les paires « clé valeur » d'une table de budget.toml, sans commentaires ni guillemets.
table() {
    awk -v t="[$1]" '
        /^\[/ { inside = ($0 == t); next }
        inside { sub(/[ \t]*#.*/, ""); gsub(/"/, ""); if ($0 ~ /=/) { split($0, kv, /[ \t]*=[ \t]*/); print kv[1], kv[2] } }' "$BUDGET"
}
table coverage.crates > "$tmp/planchers"
table coverage.main > "$tmp/main"

fail=0
echo "Couverture de lignes par crate (plancher budget.toml [coverage.crates], main au point de fourche)"
while read -r crate found hit; do
    # Comparée à quatre décimales, affichée à deux : 92,125 % n'égale pas 92,13 %.
    raw=$(awk -v h="$hit" -v f="$found" 'BEGIN { printf "%.4f", 100 * h / f }')
    pct=$(awk -v r="$raw" 'BEGIN { printf "%.2f", int(r * 100) / 100 }')
    floor=$(awk -v c="$crate" '$1 == c { print $2 }' "$tmp/planchers")
    main=$(awk -v c="$crate" '$1 == c { print $2 }' "$tmp/main")
    state=ok
    if [ -n "$floor" ] && awk -v p="$raw" -v f="$floor" 'BEGIN { exit !(p < f) }'; then
        state=SOUS
        echo "  $crate : couverture de lignes $pct %, plancher $floor % (budget.toml [coverage.crates]) : les lignes ajoutées ne sont pas testées, ou du code mort est resté" >&2
    fi
    if [ -n "$main" ] && awk -v p="$raw" -v m="$main" 'BEGIN { exit !(p < m) }'; then
        state=SOUS
        echo "  $crate : couverture de lignes $pct %, main au point de fourche $main % (budget.toml [coverage.main], critère 7)" >&2
    fi
    [ "$state" = ok ] || fail=$((fail + 1))
    printf '  %-8s %-28s %6s %%  plancher %-4s main %s\n' "$state" "$crate" "$pct" \
        "${floor:--}" "${main:--}"
    echo "$crate $raw" >> "$tmp/pourcents"
done < "$tmp/mesure"

if [ "$update" = 1 ]; then
    # Réécrit [coverage.crates] : max(plancher, mesure arrondie à l'inférieur), trié.
    awk -v meas="$tmp/pourcents" '
        BEGIN { while ((getline l < meas) > 0) { split(l, a, " "); m[a[1]] = int(a[2]) } }
        /^\[/ { if (inside) { flush(); print "" } inside = ($0 == "[coverage.crates]"); print; next }
        inside && /=/ { k = $0; sub(/^[ \t]*"/, "", k); sub(/".*/, "", k)
                        v = $0; sub(/^[^=]*=[ \t]*/, "", v); sub(/[ \t]*#.*/, "", v)
                        cur[k] = v + 0; next }
        inside && /^[ \t]*$/ { next }
        { print }
        END { if (inside) flush() }
        function flush(   k, n, keys, i, j, t, v) {
            n = 0
            for (k in m) cur[k] = (k in cur && cur[k] > m[k]) ? cur[k] : m[k]
            for (k in cur) keys[++n] = k
            for (i = 2; i <= n; i++) { t = keys[i]; for (j = i - 1; j > 0 && keys[j] > t; j--) keys[j + 1] = keys[j]; keys[j + 1] = t }
            for (i = 1; i <= n; i++) printf "\"%s\" = %d\n", keys[i], cur[keys[i]]
            inside = 0
        }' "$BUDGET" > "$tmp/budget.toml"
    cp "$tmp/budget.toml" "$BUDGET"
    echo "coverage-check : [coverage.crates] remonté à la mesure (jamais descendu)"
fi

if [ "$fail" -gt 0 ]; then
    echo "coverage-check : $fail crate(s) sous leur plancher ou sous main"
    exit 1
fi
echo "coverage-check : toutes les crates tiennent leur plancher"
