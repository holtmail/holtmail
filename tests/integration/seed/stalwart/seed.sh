#!/bin/sh
set -eu
marker=/var/lib/stalwart/.it-seeded
if [ -f "$marker" ]; then
  exit 0
fi

STALWART_RECOVERY_MODE=1 stalwart --config /etc/stalwart/config.json &
server=$!
trap 'kill "$server" 2>/dev/null || true' EXIT

api() {
  curl -fsS http://127.0.0.1:8080/jmap/ -u "$STALWART_RECOVERY_ADMIN" \
    -H 'content-type: application/json' --data-binary @-
}

tries=0
until curl -fs -o /dev/null http://127.0.0.1:8080/jmap/session -u "$STALWART_RECOVERY_ADMIN"; do
  tries=$((tries + 1))
  if [ "$tries" -gt 60 ]; then
    echo "Stalwart did not start in recovery mode" >&2
    exit 1
  fi
  sleep 0.5
done

answer=$(api < /seed/plan.json)
echo "$answer"
case "$answer" in
  *notCreated* | *notUpdated* | *'"error"'*)
    echo "Stalwart refused part of the plan" >&2
    exit 1
    ;;
esac

application=$(printf '%s' "$answer" | sed -n 's/.*"x:Application\/get".*"id":"\([^"]*\)".*/\1/p')
if [ -n "$application" ]; then
  api <<JSON
{"using":["urn:ietf:params:jmap:core","urn:stalwart:jmap"],
 "methodCalls":[["x:Application/set",{"update":{"$application":{"enabled":false}}},"webui"]]}
JSON
  echo
fi

kill "$server"
wait "$server" || true
trap - EXIT
touch "$marker"
