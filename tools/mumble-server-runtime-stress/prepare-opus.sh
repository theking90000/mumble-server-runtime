#!/bin/sh
set -eu

if [ "$#" -ne 4 ]; then
    echo "usage: $0 URL START_SECONDS DURATION_SECONDS OUTPUT.opuspack" >&2
    exit 2
fi

url=$1
start=$2
duration=$3
output=$4
end=$((start + duration))
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

yt-dlp \
    --no-playlist \
    --no-simulate \
    -x \
    --audio-format wav \
    --download-sections "*${start}-${end}" \
    --force-keyframes-at-cuts \
    -o "${temporary}/source.%(ext)s" \
    "$url"

ffmpeg \
    -hide_banner \
    -loglevel error \
    -y \
    -i "${temporary}/source.wav" \
    -ar 48000 \
    -ac 1 \
    -c:a libopus \
    -application voip \
    -frame_duration 10 \
    -b:a 24k \
    -vbr off \
    "${temporary}/encoded.ogg"

ffmpeg \
    -hide_banner \
    -loglevel error \
    -y \
    -i "${temporary}/encoded.ogg" \
    -map 0:a:0 \
    -c:a copy \
    -f data \
    "$output"

bytes=$(wc -c < "$output" | tr -d ' ')
if [ "$bytes" -eq 0 ] || [ $((bytes % 30)) -ne 0 ]; then
    echo "prepared stream is not made of 30-byte CBR Opus packets" >&2
    exit 1
fi

echo "prepared $((bytes / 30)) Opus packets in $output"
