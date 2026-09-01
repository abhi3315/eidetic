#!/bin/bash
# Rebuild the manual-test corpus + dogfood library for eidetic.
#
# The corpus lives in tmpfs scratch space and systemd-tmpfiles eats it on
# long gaps (it has died twice now), so the recipe lives here instead.
# Downloads: LFW faces (243 MB, figshare — the same source scikit-learn
# uses), three ~1 MB test videos, and the whisper.cpp JFK sample. Synthetic
# media (click tracks, blurred copies, HEVC portrait pan) is generated with
# ffmpeg.
#
# Usage:
#   scripts/rebuild-test-corpus.sh <workdir>
#   # then build the dogfood library:
#   export EIDETIC_DATABASE_PATH=<workdir>/dogfood.db
#   export EIDETIC_LIBRARY_DIR=<workdir>/dogfood-lib
#   eidetic import <workdir>/media && eidetic embed && eidetic faces
set -euo pipefail

S="${1:?usage: rebuild-test-corpus.sh <workdir>}"
mkdir -p "$S/media/photos" "$S/media/videos" "$S/media/faces"
cd "$S"

# --- LFW faces: 6 identities x 10 photos + 5 singletons -------------------
[ -f lfw-funneled.tgz ] || curl -sL -o lfw-funneled.tgz --max-time 300 \
    "https://ndownloader.figshare.com/files/5976015"
tar tzf lfw-funneled.tgz > all_files.txt
for p in George_W_Bush Colin_Powell Tony_Blair Serena_Williams Gerhard_Schroeder Junichiro_Koizumi; do
    grep "lfw_funneled/${p}/.*jpg" all_files.txt | head -10
done > extract_list.txt
grep -E "_0001.jpg" all_files.txt \
    | grep -vE "George_W_Bush|Colin_Powell|Tony_Blair|Serena_Williams|Gerhard_Schroeder|Junichiro_Koizumi" \
    | head -5 >> extract_list.txt
tar xzf lfw-funneled.tgz -T extract_list.txt
find lfw_funneled -name "*.jpg" -exec cp {} media/faces/ \;

# --- videos ----------------------------------------------------------------
cd "$S/media/videos"
[ -f bunny.mp4 ]     || curl -sL --max-time 120 -o bunny.mp4     "https://test-videos.co.uk/vids/bigbuckbunny/mp4/h264/360/Big_Buck_Bunny_360_10s_1MB.mp4"
[ -f jellyfish.mp4 ] || curl -sL --max-time 120 -o jellyfish.mp4 "https://test-videos.co.uk/vids/jellyfish/mp4/h264/360/Jellyfish_360_10s_1MB.mp4"
[ -f sintel.mp4 ]    || curl -sL --max-time 120 -o sintel.mp4    "https://test-videos.co.uk/vids/sintel/mp4/h264/360/Sintel_360_10s_1MB.mp4"
head -c 400000 bunny.mp4 > truncated_corrupt.mp4
ffmpeg -y -v error -i bunny.mp4 -c copy bunny_remux.mkv
ffmpeg -y -v error -loop 1 -i "$S/media/faces/Serena_Williams_0001.jpg" \
    -vf "zoompan=z='min(zoom+0.002,1.4)':d=125:s=640x480,format=yuv420p" \
    -t 5 -c:v libx264 serena_pan.mp4
ffmpeg -y -v error -loop 1 -i "$S/media/faces/George_W_Bush_0001.jpg" \
    -vf "zoompan=z='min(zoom+0.002,1.4)':d=125:s=480x640,format=yuv420p" \
    -t 5 -c:v libx265 -tag:v hvc1 bush_pan_hevc_portrait.mp4

# jfk speech video (voiceover-mode + transcript tests)
[ -f "$S/jfk.wav" ] || curl -sL --max-time 60 -o "$S/jfk.wav" \
    "https://github.com/ggml-org/whisper.cpp/raw/master/samples/jfk.wav"
ffmpeg -y -v error -f lavfi -i "testsrc=duration=11:size=320x240:rate=10" \
    -i "$S/jfk.wav" -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest jfk_speech.mp4

# --- blurred copies (sharpness-gate tests) ----------------------------------
ffmpeg -y -v error -i "$S/media/faces/George_W_Bush_0001.jpg" -vf "gblur=sigma=3"   "$S/media/photos/bush_blur3.jpg"
ffmpeg -y -v error -i "$S/media/faces/George_W_Bush_0001.jpg" -vf "gblur=sigma=1.5" "$S/media/photos/bush_blur15.jpg"
ffmpeg -y -v error -i bunny.mp4 -vf "gblur=sigma=4" -c:v libx264 -t 4 bunny_blur.mp4

# --- music (beat-sync tests) -------------------------------------------------
# 120 BPM click, flat energy:
ffmpeg -y -v error -f lavfi \
    -i "aevalsrc='0.7*sin(2*PI*200*t)*lt(mod(t,0.5),0.1)+0.2*sin(2*PI*80*t)*lt(mod(t,2),0.15)':d=24" \
    -c:a aac "$S/click120.m4a"
# Same click with a loud 8-16 s section: triggers high_energy detection,
# denser cuts, section crossfades, and hero-on-the-drop assignment.
ffmpeg -y -v error -f lavfi \
    -i "aevalsrc='(0.7*sin(2*PI*200*t)*lt(mod(t,0.5),0.1)+0.25*sin(2*PI*80*t))*(1+2.5*between(t,8,16))':d=24" \
    -c:a aac "$S/click_dyn.m4a"

echo "corpus ready: $(find "$S/media" -type f | wc -l) files under $S/media"
echo "next: EIDETIC_DATABASE_PATH=$S/dogfood.db EIDETIC_LIBRARY_DIR=$S/dogfood-lib \\"
echo "      eidetic import $S/media && eidetic embed && eidetic faces"
