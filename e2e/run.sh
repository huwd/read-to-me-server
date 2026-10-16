#!/usr/bin/env bash
# End-to-end test (ADR-0015): bring up the compose stack with the image under test,
# submit the fixture article, wait for it to be published, then fetch the feed and
# check the episode is the MP3 ADR-0009 specifies.
#
#   docker build --target rtm -t rtm:e2e .
#   e2e/run.sh
#
# Needs docker compose, curl, python3, ffmpeg and ffprobe. RTM_E2E_IMAGE picks a
# different image; RTM_E2E_KEEP=1 leaves the stack running afterwards.
set -euo pipefail

cd "$(dirname "$0")/.."
compose=(docker compose -p rtm-e2e -f compose.yaml -f e2e/compose.yaml)
base=http://127.0.0.1:18080
token=e2e-static-token-0123456789abcdef0123456789
work=$(mktemp -d)
# The base file insists on a token; the overlay sets the one used.
export RTM_STATIC_TOKEN=unused-by-the-e2e-overlay-but-required

finish() {
  status=$?
  if [ "$status" -ne 0 ]; then
    echo "--- end-to-end test failed; service logs follow" >&2
    "${compose[@]}" logs --no-color >&2 || true
  fi
  if [ "${RTM_E2E_KEEP:-}" != 1 ]; then
    "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
  fi
  rm -rf "$work"
  exit "$status"
}
trap finish EXIT

step() { echo "==> $*"; }
json() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }
api() { curl -fsS -H "Authorization: Bearer $token" "$@"; }

step "starting the stack"
# From nothing, even if an earlier run was kept.
"${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1
"${compose[@]}" up --detach --wait --wait-timeout 120 --no-build

step "submitting the fixture article"
item=$(api -H 'content-type: application/json' \
  -d '{"url":"http://origin/canals.html"}' "$base/v1/items")
status_url=$base$(echo "$item" | json "['status_url']")

step "waiting for it to be published"
deadline=$((SECONDS + 180))
while :; do
  view=$(api "$status_url")
  status=$(echo "$view" | json "['status']")
  case "$status" in
    published) break ;;
    failed) echo "item failed: $view" >&2; exit 1 ;;
  esac
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "timed out at status $status: $view" >&2
    exit 1
  fi
  sleep 1
done
echo "$view" | json "['article']['title']"

step "fetching the feed"
feed_url=$(api -X POST "$base/v1/feed/token" | json "['feed_url']")
curl -fsS "$feed_url" -o "$work/feed.xml"
enclosure=$(python3 - "$work/feed.xml" <<'PY'
import sys
import xml.etree.ElementTree as ET
items = ET.parse(sys.argv[1]).getroot().findall("./channel/item")
assert len(items) == 1, f"expected one episode, found {len(items)}"
item = items[0]
title = item.findtext("title")
assert title == "The Quiet Engineering of Canals", title
enclosure = item.find("enclosure")
assert enclosure.get("type") == "audio/mpeg", enclosure.attrib
print(enclosure.get("url"))
PY
)

step "downloading the episode"
curl -fsS "$enclosure" -o "$work/episode.mp3"
# Podcast apps seek with Range requests.
range=$(curl -fsS -o /dev/null -w '%{http_code}' -H 'Range: bytes=0-99' "$enclosure")
[ "$range" = 206 ] || { echo "Range request answered $range, not 206" >&2; exit 1; }

step "checking the MP3 (ADR-0009)"
ffprobe -v error -print_format json -show_streams -show_format "$work/episode.mp3" \
  >"$work/probe.json"
python3 - "$work/probe.json" <<'PY'
import json, sys
info = json.load(open(sys.argv[1]))
stream = info["streams"][0]
checks = {
    "codec_name": "mp3",
    "sample_rate": "44100",
    "channels": 1,
    "bit_rate": "64000",
}
for key, want in checks.items():
    assert stream[key] == want, f"{key}: {stream[key]!r}, expected {want!r}"
seconds = float(info["format"]["duration"])
assert seconds > 10, f"only {seconds} s of audio"
print(f"mp3, 44.1 kHz, mono, 64 kbps, {seconds:.1f} s")
PY
lufs=$(ffmpeg -nostdin -hide_banner -i "$work/episode.mp3" -af ebur128 -f null - 2>&1 \
  | awk '/Summary:/ {s=1} s && $1 == "I:" {print $2; exit}')
python3 -c "import sys; l=float(sys.argv[1]); assert -17.5 <= l <= -14.5, l; print(f'{l} LUFS')" "$lufs"

step "end-to-end test passed"
