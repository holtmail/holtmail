#!/bin/sh
set -eu
occ() { php /var/www/html/occ "$@"; }
occ config:system:set has_internet_connection --type=boolean --value=false
occ config:system:set auth.bruteforce.protection.enabled --type=boolean --value=false
occ config:system:set ratelimit.protection.enabled --type=boolean --value=false
occ config:system:set overwrite.cli.url --value=https://localhost:46443
for app in password_policy firstrunwizard updatenotification; do
  occ app:disable "$app" || true
done
