#!/bin/sh
set -eu
root=/var/www/baikal
db=$root/Specific/db/db.sqlite
mkdir -p "$root/Specific/db"
if [ ! -s "$root/config/baikal.yaml" ]; then
  cp /seed/baikal.yaml "$root/config/baikal.yaml"
fi
if [ ! -s "$db" ]; then
  sqlite3 "$db" < "$root/Core/Resources/Db/SQLite/db.sql"
fi
