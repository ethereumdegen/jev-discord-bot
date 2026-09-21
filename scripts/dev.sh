#!/usr/bin/env bash
# Run Degen Guard locally: Postgres (127.0.0.1:5432), a Redis on 6391, the
# Discord/Jev stand-ins on 3121, and the web + worker on 3120. No gateway: use
# scripts/say.sh to post a message as if someone typed it in Discord.
set -euo pipefail
cd "$(dirname "$0")/.."
# Your Jev key (and anything else) from .env; without one, the Jev stand-in answers.
if [ -f .env ]; then set -a; . ./.env; set +a; fi
unset APP_ENV APP_BASE_URL DATABASE_URL REDIS_URL DISCORD_CLIENT_ID DISCORD_CLIENT_SECRET DISCORD_BOT_TOKEN SSO_BASE_URL SSO_CLIENT_ID SSO_CLIENT_SECRET
export PORT="${PORT:-3120}" STATIC_DIR="$PWD/frontend/dist"
export DATABASE_URL="${DATABASE_URL:-postgres://127.0.0.1:5432/degen_guard_dev}"
export REDIS_URL="${REDIS_URL:-redis://127.0.0.1:6391}"
export DISCORD_CLIENT_ID=local DISCORD_CLIENT_SECRET=local DISCORD_BOT_TOKEN=local
export DISCORD_API_BASE=http://127.0.0.1:3121/api/v10 DISCORD_AUTHORIZE_BASE=http://127.0.0.1:3121
if [ -z "${TYPESAFE_API_KEY:-}" ]; then export TYPESAFE_API_KEY=local TYPESAFE_ENDPOINT=http://127.0.0.1:3121/jev; fi
export OPERATOR_EMAILS="${OPERATOR_EMAILS:-dev@example.com}"
redis-server --port 6391 --save "" --appendonly no --daemonize yes >/dev/null
python3 scripts/stubs.py 3121 &
STUBS=$!
trap 'kill $STUBS 2>/dev/null; redis-cli -p 6391 shutdown nosave >/dev/null 2>&1 || true' EXIT INT TERM
psql "${DATABASE_URL%/*}/postgres" -tAc "SELECT 1 FROM pg_database WHERE datname='${DATABASE_URL##*/}'" | grep -q 1 || psql "${DATABASE_URL%/*}/postgres" -qc "CREATE DATABASE ${DATABASE_URL##*/}"
cargo run --quiet -- migrate
cargo run --quiet -- local
