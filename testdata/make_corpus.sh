#!/usr/bin/env bash
# Generates a small corpus of synthetic clips for frame-accuracy tests.
# Requires a system ffmpeg with libx264, libx265, libsvtav1, libvpx-vp9, mpeg2video.
#
# The cases are chosen to break naive seeking: open-GOP with B-pyramids, long GOPs, 10-bit HEVC,
# MPEG-TS (m2ts-like) timestamps, VFR timestamps, anamorphic MPEG-2 (DVD-like) and AV1.
set -euo pipefail
OUT="${1:-$(cd "$(dirname "$0")" && pwd)/corpus}"
mkdir -p "$OUT"
FF=(ffmpeg -hide_banner -loglevel error -y)
# 10 s of moving test pattern (every frame differs) at 24000/1001.
SRC=(-f lavfi -i "testsrc2=size=640x360:rate=24000/1001:duration=10")
SRC_HD=(-f lavfi -i "testsrc2=size=1920x1080:rate=24000/1001:duration=4")

# Clips whose encoder isn't available in the local ffmpeg are skipped with a warning.
gen() {
  local name="$1"; shift
  [[ -f "$OUT/$name" ]] && return 0
  echo ">> $name"
  "${FF[@]}" "$@" "$OUT/$name" || { echo "!! skipped $name (encoder missing?)" >&2; rm -f "$OUT/$name"; }
}

gen h264_opengop.mkv "${SRC[@]}" -c:v libx264 -preset fast -pix_fmt yuv420p \
  -x264-params "open-gop=1:keyint=120:min-keyint=12:bframes=5:b-pyramid=normal:scenecut=40" -colorspace bt709 -color_range tv
gen h264_opengop.mp4 "${SRC[@]}" -c:v libx264 -preset fast -pix_fmt yuv420p \
  -x264-params "open-gop=1:keyint=120:bframes=5:b-pyramid=normal"
gen h264_ts.m2ts "${SRC[@]}" -c:v libx264 -preset fast -pix_fmt yuv420p \
  -x264-params "open-gop=1:keyint=48:bframes=3:b-pyramid=strict:nal-hrd=vbr" -b:v 2M -maxrate 4M -bufsize 4M -f mpegts -mpegts_m2ts_mode 1
gen h264_vfr.mkv "${SRC[@]}" -vf "setpts='if(lt(N,80),N,if(lt(N,160),80+(N-80)*2,240+(N-160)))/(24000/1001)/TB'" \
  -fps_mode passthrough -c:v libx264 -preset fast -pix_fmt yuv420p -x264-params "bframes=3:keyint=60"
gen hevc_10bit.mkv "${SRC[@]}" -c:v libx265 -preset fast -pix_fmt yuv420p10le \
  -x265-params "log-level=error:open-gop=1:keyint=96:bframes=8:b-pyramid=1"
gen hevc_10bit_hd.mkv "${SRC_HD[@]}" -c:v libx265 -preset ultrafast -pix_fmt yuv420p10le \
  -x265-params "log-level=error:keyint=48:bframes=4" -colorspace bt709 -color_primaries bt709 -color_trc bt709
gen av1.mkv "${SRC[@]}" -c:v libsvtav1 -preset 10 -pix_fmt yuv420p10le -g 120 -svtav1-params "enable-overlays=1"
gen vp9.webm "${SRC[@]}" -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -g 90 -pix_fmt yuv420p
# DVD-like: 720x480 anamorphic 16:9 MPEG-2 in a program stream, with unspecified colour metadata.
gen dvd_mpeg2.mpg -f lavfi -i "testsrc2=size=720x480:rate=30000/1001:duration=6" \
  -c:v mpeg2video -b:v 6M -g 15 -bf 2 -aspect 16:9 -pix_fmt yuv420p -f vob
gen mpeg2_opengop.mkv "${SRC[@]}" -c:v mpeg2video -b:v 4M -g 30 -bf 3 -pix_fmt yuv420p
gen mpeg2_ts.m2ts "${SRC[@]}" -c:v mpeg2video -b:v 4M -g 15 -bf 2 -pix_fmt yuv420p -f mpegts -mpegts_m2ts_mode 1
gen mpeg2_es.m2v "${SRC[@]}" -c:v mpeg2video -b:v 4M -g 15 -bf 2 -pix_fmt yuv420p
# Same content as the 640x360 clips but starting 24 frames (1.001 s) later, as a different codec:
# exercises offsets in the comparison runner.
gen late24_hevc.mkv -f lavfi -i "testsrc2=size=640x360:rate=24000/1001:duration=11" \
  -vf "trim=start_frame=24:end_frame=240,setpts=PTS-STARTPTS" -c:v libx265 -preset fast -pix_fmt yuv420p \
  -x265-params "log-level=error:keyint=48"
# --- Telecine (IVTC) cases. Ground truth: the progressive 23.976 frames, losslessly (FFV1).
# The same frames are 3:2-telecined to 29.97i (top field first), losslessly and as lossy interlaced
# H.264 (BD-like, in M2TS) and MPEG-2 (DVD-like, in a program stream).
# Blurred: testsrc2's per-pixel noise square trips fieldmatch's combing detector (false positives).
TC_SRC=(-f lavfi -i "testsrc2=size=640x360:rate=24000/1001:duration=10,gblur=sigma=1.5,format=yuv420p")
TC_FILTER="telecine=first_field=top:pattern=23,setfield=tff"
gen ivtc_truth_ffv1.mkv "${TC_SRC[@]}" -c:v ffv1 -level 3
gen ivtc_telecined_ffv1.mkv "${TC_SRC[@]}" -vf "$TC_FILTER" -c:v ffv1 -level 3 -field_order tt
gen ivtc_telecined_h264.m2ts "${TC_SRC[@]}" -vf "$TC_FILTER" -c:v libx264 -preset fast -crf 14 -flags +ildct+ilme \
  -x264-params "tff=1:keyint=30:bframes=3" -field_order tt -f mpegts -mpegts_m2ts_mode 1
gen ivtc_telecined_dvd.mpg "${TC_SRC[@]}" -vf "$TC_FILTER" -c:v mpeg2video -b:v 8M -g 15 -bf 2 -flags +ildct+ilme -alternate_scan 1 \
  -field_order tt -f vob
# Scene cuts and black screens for the frame-picking rules: 4 s pattern A, 2 s black, 4 s pattern B,
# hard cuts between them (frames 0-95 A, 96-143 black, 144-239 B at 23.976).
gen cuts_black.mkv -f lavfi -i "testsrc2=size=640x360:rate=24000/1001:duration=4.004" \
  -f lavfi -i "color=black:size=640x360:rate=24000/1001:duration=2.002" \
  -f lavfi -i "mandelbrot=size=640x360:rate=24000/1001,trim=end_frame=96" \
  -filter_complex "[0][1][2]concat=n=3:v=1,format=yuv420p" -c:v libx264 -preset fast -crf 16 -x264-params keyint=48
echo ">> corpus ready in $OUT"
