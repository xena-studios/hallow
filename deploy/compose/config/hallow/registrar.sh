#!/bin/bash
# Dev only: registers the Pumpkin servers with the control plane and
# heartbeats them every 5 s, standing in for the Hallow server plugin.
# A server is only registered and heartbeated while its port accepts
# connections, so a stopped server expires after 15 s like a real one.
set -u

cp="${HALLOW_CONTROL_PLANE:-http://control-plane:8080}"
token="$(tr -d '[:space:]' < "${HALLOW_TOKEN_FILE:?}")"
# name kind role address, one server per line.
servers="${HALLOW_SERVERS:-lobby hub lobby lobby:25565
game-1 game game game-1:25565}"

post() { # path body; prints the HTTP status
  curl -s -o /dev/null -w '%{http_code}' --max-time 5 -X POST \
    -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
    -d "$2" "$cp$1"
}

up() { # host:port
  timeout 1 bash -c "exec 3<>/dev/tcp/${1%:*}/${1##*:}" 2>/dev/null
}

register() { # name kind role address
  code=$(post /v1/servers "{\"name\":\"$1\",\"kind\":\"$2\",\"version\":\"dev\",\"role\":\"$3\",\"address\":\"$4\",\"max_players\":100}")
  echo "registrar: register $1 -> $code"
}

while true; do
  while read -r name kind role address; do
    [ -n "$name" ] || continue
    if ! up "$address"; then
      echo "registrar: $name ($address) is not accepting connections; skipping"
      continue
    fi
    code=$(post "/v1/servers/$name/heartbeat" '{"players":0,"state":"ready"}')
    case "$code" in
      204) ;;
      404) register "$name" "$kind" "$role" "$address" ;;
      *) echo "registrar: heartbeat $name -> $code" ;;
    esac
  done <<< "$servers"
  sleep 5
done
