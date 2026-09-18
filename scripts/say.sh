#!/usr/bin/env bash
# Pretend someone typed a message in the local server: say.sh <author-id> "text" [days-in-server]
set -euo pipefail
author="$1"; text="$2"; days="${3:-1}"
joined=$(date -u -v-"${days}"d +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d "-${days} days" +%Y-%m-%dT%H:%M:%SZ)
id="local-$(date +%s%N)"
payload=$(python3 -c 'import json,sys; print(json.dumps({"guild_id":"g-local","message_id":sys.argv[1],"channel_id":"c-general","author_id":sys.argv[2],"username":"user"+sys.argv[2],"content":sys.argv[3],"author_is_bot":False,"joined_at":sys.argv[4],"role_ids":[],"mentions_everyone":False}))' "$id" "$author" "$text" "$joined")
redis-cli -p 6391 SET "jev:seen:$id" 1 EX 3600 >/dev/null
redis-cli -p 6391 XADD jev:messages '*' m "$payload" >/dev/null
echo "queued $id"
