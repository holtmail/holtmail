#!/bin/sh
set -eu
umask 022
cd /state
: >> radicale-users
shared_key() {
  while IFS= read -r line; do printf '%s\n' "$line"; done < server.key > server-shared.key
}
extras() {
  [ -s server-shared.key ] || shared_key
  [ -s oauth.p12 ] || openssl pkcs12 -export -in server.crt -inkey server.key -certfile ca.crt \
    -name oauth -passout pass:it-oauth-keystore > oauth.p12
}
san="subjectAltName = ${SERVER_NAMES}"
names_unchanged() {
  [ -s server.ext ] || return 1
  while IFS= read -r line; do [ "$line" = "$san" ] && return 0; done < server.ext
  return 1
}
if [ -s ca.crt ] && [ -s server.crt ] && [ -s server.key ] && names_unchanged; then
  extras
  exit 0
fi
printf '%s\n' \
  'basicConstraints = critical, CA:FALSE' \
  'keyUsage = critical, digitalSignature' \
  'extendedKeyUsage = serverAuth' \
  "$san" > server.ext
if [ ! -s ca.crt ] || [ ! -s ca.key ]; then
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 365 \
    -subj '/CN=Holtmail integration CA' \
    -addext 'basicConstraints = critical, CA:TRUE' \
    -addext 'keyUsage = critical, keyCertSign, cRLSign' \
    -keyout ca.key -out ca.crt
fi
: > server-shared.key
: > oauth.p12
openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -subj '/CN=localhost' -keyout server.key -out server.csr
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 365 \
  -extfile server.ext -out server.crt
shared_key
extras
