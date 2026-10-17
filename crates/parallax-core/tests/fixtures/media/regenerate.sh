#!/bin/bash
# Rebuilds the media fixtures with ffmpeg (6.x with libx264, libvpx, libopus,
# libvorbis, libmp3lame). Each file is a fraction of a second at a tiny size.
set -eu
cd "$(dirname "$0")"
ff() { ffmpeg -hide_banner -loglevel error -y -bitexact "$@"; }
TONE=(-f lavfi -i sine=frequency=440:sample_rate=22050:duration=0.5)
VIDEO=(-f lavfi -i testsrc=size=64x48:rate=10:duration=1)
TAGS=(-metadata "title=Test tone" -metadata artist=Parallax -metadata album=Fixtures -metadata date=2024 -metadata "comment=First version")

# Chapters for MP4 and Matroska.
cat > chapters.txt <<'META'
;FFMETADATA1
title=Chaptered video
[CHAPTER]
TIMEBASE=1/1000
START=0
END=400
title=Intro
[CHAPTER]
TIMEBASE=1/1000
START=400
END=1000
title=Main part
META
printf 'Attached notes for the Matroska fixture.\n' > notes.txt

# MP3: v1, the same audio with other tags, another bitrate.
ff "${TONE[@]}" -c:a libmp3lame -b:a 32k -ac 1 "${TAGS[@]}" -id3v2_version 3 -write_id3v1 1 tone-v1.mp3
ff -i tone-v1.mp3 -c copy -map_metadata -1 -metadata title="Test tone (remastered)" -metadata artist=Parallax \
   -metadata genre=Test -id3v2_version 4 -write_id3v1 1 tone-retagged.mp3
ff "${TONE[@]}" -c:a libmp3lame -b:a 64k -ac 1 "${TAGS[@]}" -id3v2_version 3 tone-64k.mp3
ff "${TONE[@]}" -c:a libmp3lame -q:a 6 -ac 1 -write_xing 1 tone-vbr.mp3

# FLAC with a cover picture; retagged copy; other compression level (same samples).
ff -f lavfi -i color=c=red:size=8x8:duration=0.1 -frames:v 1 cover.png
ff "${TONE[@]}" -i cover.png -map 0 -map 1 -c:a flac -c:v copy -disposition:v attached_pic "${TAGS[@]}" tone.flac
ff -i tone.flac -map 0 -c copy -metadata comment="Second version" tone-retagged.flac
ff -i tone.flac -map 0:a -c:a flac -compression_level 0 "${TAGS[@]}" tone-level0.flac

# Ogg Vorbis and Opus.
ff "${TONE[@]}" -c:a libvorbis -q:a 0 "${TAGS[@]}" tone.ogg
ff "${TONE[@]}" -c:a libopus -b:a 16k "${TAGS[@]}" tone.opus

# WAV and AIFF with tags, retagged copies.
ff "${TONE[@]}" -c:a pcm_s16le "${TAGS[@]}" tone.wav
ff -i tone.wav -c copy -metadata title="Other title" tone-retagged.wav
ff "${TONE[@]}" -c:a pcm_s16be "${TAGS[@]}" -write_id3v2 1 tone.aiff

# MP4 (moov at the end), the same with moov first and another title, M4A, MOV, 3GP.
ff "${VIDEO[@]}" "${TONE[@]}" -i chapters.txt -map 0 -map 1 -map_metadata 2 -map_chapters 2 \
   -c:v libx264 -preset ultrafast -crf 40 -pix_fmt yuv420p -c:a aac -b:a 24k -ac 1 -metadata:s:a language=eng "${TAGS[@]}" video.mp4
ff "${VIDEO[@]}" "${TONE[@]}" -i chapters.txt -map 0 -map 1 -map_metadata 2 -map_chapters 2 \
   -c:v libx264 -preset ultrafast -crf 40 -pix_fmt yuv420p -c:a aac -b:a 24k -ac 1 -metadata:s:a language=eng "${TAGS[@]}" \
   -metadata "title=Second title" -movflags +faststart video-v2.mp4
ff "${TONE[@]}" -c:a aac -b:a 24k "${TAGS[@]}" -metadata album_artist=Parallax -metadata track=3/12 tone.m4a
ff "${VIDEO[@]}" -c:v mpeg4 -q:v 20 "${TAGS[@]}" video.mov
ff "${VIDEO[@]}" "${TONE[@]}" -c:v libx264 -preset ultrafast -crf 40 -pix_fmt yuv420p -c:a aac -b:a 16k -ac 1 video.3gp

# Matroska with chapters, attachment and per-track tags; WebM; AVI.
ff "${VIDEO[@]}" "${TONE[@]}" -i chapters.txt -map 0 -map 1 -map_metadata 2 -map_chapters 2 \
   -c:v libx264 -preset ultrafast -crf 40 -pix_fmt yuv420p -c:a libopus -b:a 16k \
   -attach notes.txt -metadata:s:t mimetype=text/plain -metadata:s:a language=ger -metadata:s:a title=Commentary "${TAGS[@]}" video.mkv
ff "${VIDEO[@]}" "${TONE[@]}" -i chapters.txt -map 0 -map 1 -map_metadata 2 -map_chapters 2 \
   -c:v libx264 -preset ultrafast -crf 40 -pix_fmt yuv420p -c:a libopus -b:a 16k \
   -attach notes.txt -metadata:s:t mimetype=text/plain -metadata:s:a language=eng -metadata:s:a title=Commentary "${TAGS[@]}" \
   -metadata "title=Second title" video-v2.mkv
ff "${VIDEO[@]}" "${TONE[@]}" -c:v libvpx-vp9 -b:v 50k -deadline realtime -c:a libopus -b:a 16k "${TAGS[@]}" video.webm
ff "${VIDEO[@]}" "${TONE[@]}" -c:v mpeg4 -q:v 20 -c:a libmp3lame -b:a 32k -ac 1 "${TAGS[@]}" video.avi
rm -f cover.png
