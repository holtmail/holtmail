#!/usr/bin/env bash
# Usage: sign-manifest.sh <release dir> <key id>
# Signs <dir>/manifest.json with the ed25519 seed in CATEGORIES_SIGNING_KEY (64 hex
# characters) using only stock tools (bash, jq, perl, sha256sum or shasum, OpenSSL 3), and writes
# <dir>/manifest.json.sig. The seed never reaches a file, a command line or the output.
set -euo pipefail
umask 077

dir=${1:?release dir}
key_id=${2:?key id}
manifest="$dir/manifest.json"
signature="$dir/manifest.json.sig"

fail() {
  echo "error: $*" >&2
  exit 1
}

sha256() {
  if command -v sha256sum > /dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

seed=${CATEGORIES_SIGNING_KEY:-}
seed=${seed//[[:space:]]/}
unset CATEGORIES_SIGNING_KEY
[[ $seed =~ ^[0-9a-fA-F]{64}$ ]] || fail "CATEGORIES_SIGNING_KEY is not 64 hex characters"

key_der() {
  printf '%s' "302e020100300506032b657004220420$seed" | perl -e 'local $/; print pack("H*", <STDIN>)'
}

[[ -f $manifest ]] || fail "no manifest.json in $dir"
size=$(wc -c < "$manifest" | tr -d ' ')
((size > 0 && size <= 65536)) || fail "manifest.json is $size bytes"
rm -f "$signature"

jq -e --arg key_id "$key_id" '
  (keys | sort) == ["files", "format", "key_id", "not_after", "version"]
  and .format == 1
  and .key_id == $key_id
  and (.version | test("^[0-9]{1,16}$"))
  and (.not_after | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))
  and ((.not_after | fromdateiso8601) as $t | $t > now and $t <= now + 60 * 86400 + 3600)
  and (.files | length >= 1 and length <= 2)
  and ([.files[].name] | unique | length) == (.files | length)
  and all(.files[]; (keys | sort) == ["license", "name", "rows", "sha256", "size"])
  and all(.files[];
    (.name == "wikidata-categories.tsv.gz" and .license == "CC0-1.0")
    or (.name == "ut1-categories.tsv.gz" and .license == "CC-BY-SA-4.0"))
' "$manifest" > /dev/null || fail "manifest.json is not a manifest for key $key_id"

while IFS=$'\t' read -r name sha size; do
  file="$dir/$name"
  [[ -f $file ]] || fail "$name is listed but missing"
  [[ $(wc -c < "$file" | tr -d ' ') == "$size" ]] || fail "$name has another size"
  [[ $(sha256 "$file") == "$sha" ]] || fail "$name has another sha256"
done < <(jq -r '.files[] | [.name, .sha256, (.size | tostring)] | @tsv' "$manifest")

tmp="$signature.tmp"
key_der | openssl pkeyutl -sign -rawin -inkey /dev/stdin -keyform DER -in "$manifest" -out "$tmp"
[[ $(wc -c < "$tmp" | tr -d ' ') == 64 ]] || fail "the signature is not 64 bytes"
public=$(key_der | openssl pkey -inform DER -in /dev/stdin -pubout)
openssl pkeyutl -verify -rawin -pubin -inkey <(printf '%s\n' "$public") \
  -in "$manifest" -sigfile "$tmp" > /dev/null || fail "the signature does not verify"
seed=
mv "$tmp" "$signature"

public_hex=$(printf '%s\n' "$public" | openssl pkey -pubin -outform DER | tail -c 32 | perl -e 'local $/; print unpack("H*", <STDIN>)')
echo "signed $(jq -r .version "$manifest") with key $key_id (public $public_hex)"
