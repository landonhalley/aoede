#!/bin/bash
# Entrypoint for Pterodactyl / Pelican. Wings passes the egg's startup command in
# $STARTUP with {{VARIABLE}} placeholders, which are replaced with environment values.
cd /home/container || exit 1

MODIFIED_STARTUP=$(echo "${STARTUP}" | sed -e 's/{{/${/g' -e 's/}}/}/g')
MODIFIED_STARTUP=$(eval echo "${MODIFIED_STARTUP}")
echo "container@aoede~ ${MODIFIED_STARTUP}"

# shellcheck disable=SC2086
exec ${MODIFIED_STARTUP}
