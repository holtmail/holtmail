#!/bin/sh
set -eu
mkdir -p /tmp/certs/CAs /tmp/mc/certs/CAs
cp /certs/server.crt /tmp/certs/public.crt
cp /certs/server-shared.key /tmp/certs/private.key
cp /certs/ca.crt /tmp/certs/CAs/ca.crt
cp /certs/ca.crt /tmp/mc/certs/CAs/ca.crt
minio server /data --certs-dir /tmp/certs --address :9000 --console-address :9001 &
pid=$!
trap 'kill -TERM "$pid"' TERM INT
export MC_CONFIG_DIR=/tmp/mc
until mc alias set it https://localhost:9000 "$MINIO_ROOT_USER" "$MINIO_ROOT_PASSWORD" >/dev/null 2>&1; do
  sleep 0.5
done
mc mb --ignore-existing it/holtmail-it
touch /tmp/ready
wait "$pid"
