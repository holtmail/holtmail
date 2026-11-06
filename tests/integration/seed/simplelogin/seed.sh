#!/bin/sh
# Prepares the SimpleLogin of the alias suite: a DKIM key and the DNS zone that publishes it,
# the database, and one invented user with two mailboxes and a known API key.
set -eu
mkdir -p /state/sl
if [ ! -f /state/sl/dkim.key ]; then
  openssl genrsa -traditional -out /state/sl/dkim.key 2048 2>/dev/null
fi
key=$(openssl rsa -in /state/sl/dkim.key -pubout -outform DER 2>/dev/null | base64 | tr -d '\n')
first=$(printf '%s' "$key" | cut -c1-200)
rest=$(printf '%s' "$key" | cut -c201-)
cat > /state/sl/db.sl.example.org <<ZONE
\$ORIGIN sl.example.org.
@ 60 IN SOA ns.sl.example.org. admin.sl.example.org. 1 60 60 600 60
@ 60 IN NS ns
ns 60 IN A 11.88.78.53
dkim._domainkey 60 IN TXT "v=DKIM1; k=rsa; p=$first" "$rest"
ZONE
cd /code
alembic upgrade head
python init_app.py
PYTHONPATH=/code python /seed/seed.py
