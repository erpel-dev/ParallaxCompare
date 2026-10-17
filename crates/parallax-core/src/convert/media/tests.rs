use super::*;

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media/", $name))
    };
}

fn view(name: &str, data: &[u8]) -> String {
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    summary(ext, data).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn has(text: &str, expected: &[&str]) {
    for e in expected {
        assert!(text.contains(e), "missing {e:?} in\n{text}");
    }
}

fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
    a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
}

#[test]
fn mp4_with_moov_at_the_end() {
    let v = view("video.mp4", fixture!("video.mp4"));
    has(
        &v,
        &[
            "Container:         MPEG-4\n",
            "Brand:             isom (version 512; compatible: isom, iso2, avc1, mp41)",
            "Duration:          0:00:01.000",
            "=== Track 1: Video ===\nCodec:             H.264 (avc1), Constrained Baseline profile, level 1\nResolution:        64 × 48",
            "Frame rate:        10 fps",
            "Codec:             AAC LC (mp4a)\nSample rate:       22050 Hz\nChannels:          1 (mono)",
            "Language:          eng",
            "Title:             Test tone",
            "Artist:            Parallax",
            "0:00:00.000 – 0:00:00.400  Intro\n0:00:00.400 – 0:00:01.000  Main part",
        ],
    );
    // moov moved to the front (faststart) and another title: only the title line changes.
    let v2 = view("video-v2.mp4", fixture!("video-v2.mp4"));
    let changed = lines_only_in(&v2, &v);
    assert!(changed.iter().all(|l| l.starts_with("Title:") || l.starts_with("Overall bitrate:")), "{changed:?}");
    assert!(changed.contains(&"Title:             Second title"), "{changed:?}");
}

#[test]
fn other_iso_media() {
    has(
        &view("tone.m4a", fixture!("tone.m4a")),
        &["Container:         MPEG-4 audio (M4A)", "Track:             3/12", "Album artist:      Parallax"],
    );
    has(
        &view("video.mov", fixture!("video.mov")),
        &["Container:         QuickTime movie", "Codec:             MPEG-4 Visual (mp4v)"],
    );
    has(&view("video.3gp", fixture!("video.3gp")), &["Container:         3GPP", "Brand:             3gp6"]);
}

#[test]
fn matroska_and_webm() {
    let v = view("video.mkv", fixture!("video.mkv"));
    has(
        &v,
        &[
            "Container:         Matroska",
            "Title:             Test tone",
            "Duration:          0:00:01.000",
            "Codec:             H.264 (V_MPEG4/ISO/AVC), Constrained Baseline profile, level 1",
            "Codec:             Opus (A_OPUS)",
            "Language:          ger\nTitle:             Commentary",
            "--- Tags ---\nDURATION:",
            "ARTIST:            Parallax",
            "0:00:00.400 – 0:00:01.000  Main part",
            "notes.txt: text/plain, 41 bytes, crc",
        ],
    );
    let v2 = view("video-v2.mkv", fixture!("video-v2.mkv"));
    let changed = lines_only_in(&v2, &v);
    assert_eq!(
        changed.iter().filter(|l| !l.starts_with("Overall bitrate:")).copied().collect::<Vec<_>>(),
        ["Title:             Second title", "Language:          eng"]
    );
    has(&view("video.webm", fixture!("video.webm")), &["Container:         WebM", "Codec:             VP9 (V_VP9)"]);
}

#[test]
fn mp3_tags_and_frames() {
    let v = view("tone-v1.mp3", fixture!("tone-v1.mp3"));
    has(
        &v,
        &[
            "Container:         MP3",
            "Tags:              ID3v2.3, ID3v1",
            "Codec:             MPEG-2 Layer III",
            "Bitrate:           32 kb/s (constant)",
            "Title (TIT2):      Test tone",
            "=== ID3v1 tags ===\nAlbum:             Fixtures",
        ],
    );
    has(&view("tone-vbr.mp3", fixture!("tone-vbr.mp3")), &["(variable, average)", "VBR header:        Xing"]);
    let retagged = view("tone-retagged.mp3", fixture!("tone-retagged.mp3"));
    has(
        &retagged,
        &["Tags:              ID3v2.4, ID3v1", "Title (TIT2):      Test tone (remastered)", "Genre (TCON):      Test"],
    );
}

#[test]
fn flac_ogg_wav_aiff_avi() {
    has(
        &view("tone.flac", fixture!("tone.flac")),
        &[
            "Bits per sample:   16\nSamples:           11025\nAudio MD5:",
            "TITLE:             Test tone",
            "image/png, 8 × 8",
        ],
    );
    has(
        &view("tone.ogg", fixture!("tone.ogg")),
        &["Container:         Ogg (Vorbis)", "Duration:          0:00:00.500", "ARTIST:"],
    );
    has(&view("tone.opus", fixture!("tone.opus")), &["Container:         Ogg (Opus)", "Sample rate:       48000 Hz"]);
    has(
        &view("tone.wav", fixture!("tone.wav")),
        &["Codec:             PCM (0x0001)", "Duration:          0:00:00.500", "Title (INAM):      Test tone"],
    );
    has(
        &view("tone.aiff", fixture!("tone.aiff")),
        &["Container:         AIFF", "Samples:           11025", "Title (TIT2):"],
    );
    has(
        &view("video.avi", fixture!("video.avi")),
        &["Container:         AVI", "Codec:             MPEG-4 Visual (FMP4)", "Codec:             MP3 (0x0055)"],
    );
}

#[test]
fn same_audio_with_other_tags() {
    let same = Some("same audio, only tags differ");
    assert_eq!(same_media("mp3", fixture!("tone-v1.mp3"), fixture!("tone-retagged.mp3")), same);
    assert_eq!(same_media("mp3", fixture!("tone-v1.mp3"), fixture!("tone-64k.mp3")), None);
    assert_eq!(same_media("flac", fixture!("tone.flac"), fixture!("tone-retagged.flac")), same);
    assert_eq!(
        same_media("flac", fixture!("tone.flac"), fixture!("tone-level0.flac")),
        Some("same audio (equal MD5 of the decoded samples)")
    );
    assert_eq!(same_media("wav", fixture!("tone.wav"), fixture!("tone-retagged.wav")), same);
    assert_eq!(same_media("wav", fixture!("tone.wav"), fixture!("tone.aiff")), None);
    // A changed sample.
    let mut edited = fixture!("tone-retagged.wav").to_vec();
    let n = edited.len();
    edited[n - 100] ^= 1;
    assert_eq!(same_media("wav", fixture!("tone.wav"), &edited), None);
    let mut edited = fixture!("tone-retagged.mp3").to_vec();
    edited[1500] ^= 1;
    assert_eq!(same_media("mp3", fixture!("tone-v1.mp3"), &edited), None);
    assert_eq!(same_media("mp4", fixture!("video.mp4"), fixture!("video-v2.mp4")), None);
}

#[test]
fn detection() {
    assert_eq!(detect("mp3", fixture!("tone-vbr.mp3")), Some(Container::Mp3));
    assert_eq!(detect("bin", fixture!("tone-v1.mp3")), Some(Container::Mp3)); // ID3 tag
    assert_eq!(detect("mkv", fixture!("video.webm")), Some(Container::Matroska));
    assert_eq!(detect("txt", b"ID3 is a tag format\n"), None);
    assert_eq!(detect("mp3", b"\xFF\xFB not really a frame"), None);
    assert_eq!(detect("txt", b"RIFF\x10\0\0\0WEBPVP8 "), None);
    assert_eq!(detect("bin", b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"), None);
    assert_eq!(detect("mov", b"\0\0\0\x08wide\0\0\0\x08mdat"), Some(Container::Bmff));
    assert_eq!(detect("bin", b"\0\0\0\x08wide\0\0\0\x08mdat"), None);
}

/// Every fixture cut at many lengths, and with flipped bytes: no panic.
#[test]
fn truncated_and_corrupt_files_do_not_panic() {
    let all: &[(&str, &[u8])] = &[
        ("mp4", fixture!("video.mp4")),
        ("mp4", fixture!("video-v2.mp4")),
        ("mkv", fixture!("video.mkv")),
        ("mp3", fixture!("tone-v1.mp3")),
        ("mp3", fixture!("tone-vbr.mp3")),
        ("flac", fixture!("tone.flac")),
        ("ogg", fixture!("tone.ogg")),
        ("opus", fixture!("tone.opus")),
        ("wav", fixture!("tone.wav")),
        ("aiff", fixture!("tone.aiff")),
        ("avi", fixture!("video.avi")),
        ("m4a", fixture!("tone.m4a")),
    ];
    for (ext, data) in all {
        for len in (0..data.len()).step_by(7).chain(0..64) {
            let _ = summary(ext, &data[..len]);
            let _ = same_media(ext, &data[..len], data);
        }
        let mut seed = 0x2545F491u32;
        for _ in 0..300 {
            let mut d = data.to_vec();
            for _ in 0..8 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let i = seed as usize % d.len();
                d[i] = (seed >> 8) as u8;
            }
            let _ = summary(ext, &d);
        }
    }
}
