#!/bin/sh
# Issue #28 : signés par `make sign` avec une identité stable, deux binaires différents
# (deux builds) partagent la même exigence désignée ; en ad hoc, elle change.
#
# L'identité de test vit dans un trousseau temporaire, supprimé à la fin. Sur un poste,
# le trousseau n'est pas ajouté à la liste de recherche : seule la CI (CI=true) le fait.
set -eu

if [ "$(uname -s)" != Darwin ]; then
  echo "macOS uniquement : rien à vérifier"
  exit 0
fi

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
kc="$work/penelope-test.keychain-db"
pw=$(openssl rand -hex 16)
search_list=$(security list-keychains -d user | tr -d '"')

cleanup() {
  if [ "${CI:-}" = true ]; then
    # shellcheck disable=SC2086
    security list-keychains -d user -s $search_list >/dev/null 2>&1 || true
  fi
  security delete-keychain "$kc" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

security create-keychain -p "$pw" "$kc"
security set-keychain-settings -lut 600 "$kc"
security unlock-keychain -p "$pw" "$kc"
if [ "${CI:-}" = true ]; then
  # shellcheck disable=SC2086
  security list-keychains -d user -s "$kc" $search_list
fi

cat > "$work/openssl.cnf" <<CNF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = Penelope Test Signing
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
CNF
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -config "$work/openssl.cnf" \
  -keyout "$work/key.pem" -out "$work/cert.pem" 2>/dev/null
legacy=
if openssl version | grep -q '^OpenSSL 3'; then legacy=-legacy; fi
# shellcheck disable=SC2086
openssl pkcs12 -export $legacy -inkey "$work/key.pem" -in "$work/cert.pem" \
  -out "$work/id.p12" -passout "pass:$pw"
security import "$work/id.p12" -k "$kc" -P "$pw" -T /usr/bin/codesign >/dev/null
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$pw" "$kc" >/dev/null
identity=$(openssl x509 -in "$work/cert.pem" -noout -fingerprint -sha1 | cut -d= -f2 | tr -d :)

requirement() {
  codesign -d -r- "$1" 2>/dev/null | sed -n 's/^\(# \)\{0,1\}designated => //p'
}

for n in 1 2; do
  printf 'int main(void) { return %s; }\n' "$n" > "$work/build$n.c"
  cc -o "$work/build$n" "$work/build$n.c"
  cc -o "$work/adhoc$n" "$work/build$n.c"
  codesign --force --sign - "$work/adhoc$n"
  make -s -C "$root" sign BIN="$work/build$n" SIGN_IDENTITY="$identity" \
    SIGN_FLAGS="--keychain $kc"
done

adhoc1=$(requirement "$work/adhoc1")
adhoc2=$(requirement "$work/adhoc2")
signed1=$(requirement "$work/build1")
signed2=$(requirement "$work/build2")
echo "ad hoc : $adhoc1"
echo "         $adhoc2"
echo "signé  : $signed1"
echo "         $signed2"

[ "$adhoc1" != "$adhoc2" ] || { echo "échec : l'exigence ad hoc devrait changer"; exit 1; }
[ -n "$signed1" ] || { echo "échec : exigence désignée absente"; exit 1; }
[ "$signed1" = "$signed2" ] || { echo "échec : l'exigence signée change d'un build à l'autre"; exit 1; }
case "$signed1" in
  *'identifier "io.github.edouard-claude.penelope"'*) ;;
  *) echo "échec : identifiant fixe absent"; exit 1 ;;
esac
echo "ok : exigence désignée stable entre deux builds signés"
