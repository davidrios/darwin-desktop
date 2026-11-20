#!/usr/bin/env bash
# Run the M0 server/client pair and check that the server saw the client's pixels.
# Exits 0 on success. Binaries are looked up next to this script, then on PATH.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
server="$here/wl-spike-server"; [[ -x $server ]] || server=wl-spike-server
client="$here/wl-spike-client"; [[ -x $client ]] || client=wl-spike-client

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/wlrt.XXXXXX")}"
chmod 700 "$XDG_RUNTIME_DIR"
sock="wl-spike-$$"
export WAYLAND_DISPLAY="$sock"
log="$(mktemp "${TMPDIR:-/tmp}/wl-spike-server.XXXXXX")"

"$server" --once "$sock" >"$log" 2>&1 &
server_pid=$!
trap 'kill $server_pid 2>/dev/null || true; rm -f "$log"' EXIT

for _ in $(seq 100); do
  [[ -S $XDG_RUNTIME_DIR/$sock ]] && break
  kill -0 $server_pid 2>/dev/null || { cat "$log"; echo "server died"; exit 1; }
  sleep 0.05
done
[[ -S $XDG_RUNTIME_DIR/$sock ]] || { cat "$log"; echo "server socket never appeared"; exit 1; }

client_out="$("$client")"
wait $server_pid
echo "$client_out"
cat "$log"

want="$(sed -n 's/.*checksum=\([0-9a-f]*\).*/\1/p' <<<"$client_out")"
got="$(sed -n 's/.*checksum=\([0-9a-f]*\).*/\1/p' "$log")"
if [[ -n $want && $want == "$got" ]]; then
  echo "OK: server received the client's buffer (checksum $want)"
else
  echo "FAIL: client checksum '$want', server checksum '$got'"
  exit 1
fi
