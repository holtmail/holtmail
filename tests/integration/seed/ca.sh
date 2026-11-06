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
if [ -s ca.crt ] && [ -s server.crt ] && [ -s server.key ]; then
  extras
  exit 0
fi
printf '%s\n' \
  'basicConstraints = critical, CA:FALSE' \
  'keyUsage = critical, digitalSignature' \
  'extendedKeyUsage = serverAuth' \
  "subjectAltName = ${SERVER_NAMES}" > server.ext
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 365 \
  -subj '/CN=Holtmail integration CA' \
  -addext 'basicConstraints = critical, CA:TRUE' \
  -addext 'keyUsage = critical, keyCertSign, cRLSign' \
  -keyout ca.key -out ca.crt
openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -subj '/CN=localhost' -keyout server.key -out server.csr
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 365 \
  -extfile server.ext -out server.crt
shared_key
extras
