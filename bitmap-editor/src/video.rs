//! Frames from files the image crate does not read, through ffmpeg: video
//! (MP4, MOV, WebM, ...) and still formats such as WebP.
//!
//! `ffprobe` lists the video stream's packets once, when the file is opened;
//! their presentation times, sorted, are the frame index. Frame `n` is then
//! one `ffmpeg` run that seeks to halfway between frames `n - 1` and `n` and
//! writes a single RGBA frame. ffmpeg decodes from the keyframe before the
//! seek point and drops what comes before it, so the frame is the one a
//! decode from the start would give, without decoding the whole file. Tests
//! compare every frame against ffmpeg's own `select=eq(n,N)` filter, which
//! is also the fallback when a file has no usable timestamps.
//!
//! The output is PAM (`-c:v pam`), RGBA with a header that states the size
//! after ffmpeg's rotation of phone video, so the size is read from the
//! frame itself.
//!
//! These calls block until ffmpeg exits. The editor runs them on the host's
//! offload workers, one at a time; the CLI runs them inline.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use loadngo_host_core::DecodedImage;

/// What ffprobe found in a file.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoIndex {
    /// The container, as `--probe` names it: `mp4`, `mov`, `webm`, `webp`...
    pub container: String,
    pub codec: String,
    /// Presentation times of the frames in seconds, sorted. Empty when the
    /// file has no timestamps; frames are then found by count.
    pts: Vec<f64>,
    /// The format's start time, which ffmpeg adds to a seek position.
    start: f64,
    frames: usize,
}

impl VideoIndex {
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Where to seek for `frame`, or `None` to decode from the start (frame
    /// 0) or to count frames with the select filter (no usable timestamps).
    fn seek_seconds(&self, frame: usize) -> Option<f64> {
        if frame == 0 || self.pts.len() != self.frames {
            return None;
        }
        let (before, at) = (self.pts[frame - 1], self.pts[frame]);
        // Two frames at one time: a seek could land on either.
        (at - before > 1e-9).then(|| ((before + at) / 2.0 - self.start).max(0.0))
    }
}

/// `name` (`ffmpeg`, `ffprobe`) on `PATH`, else in the places package
/// managers put it; a program started from the Finder has a short `PATH`.
pub fn find_tool(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let from_path = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    let fallbacks = [
        "/opt/homebrew/bin",
        "/opt/homebrew/opt/ffmpeg-full/bin",
        "/opt/homebrew/opt/ffmpeg/bin",
        "/usr/local/bin",
        "/usr/bin",
    ];
    from_path
        .into_iter()
        .chain(fallbacks.iter().map(PathBuf::from))
        .map(|dir| dir.join(&file))
        .find(|candidate| candidate.is_file())
}

fn tool(name: &str) -> Result<PathBuf> {
    find_tool(name).ok_or_else(|| {
        anyhow!("{name} was not found (on PATH or in the usual places); video needs ffmpeg")
    })
}

/// Lists the first video stream of `path`: its container, codec and frames.
pub fn probe(path: &Path) -> Result<VideoIndex> {
    let output = Command::new(tool("ffprobe")?)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg("stream=codec_name:format=format_name,start_time:packet=pts_time")
        .args(["-of", "json"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .context("cannot run ffprobe")?;
    if !output.status.success() {
        bail!(
            "ffprobe cannot read {}: {}",
            path.display(),
            last_line(&output.stderr)
        );
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("ffprobe printed something other than JSON")?;
    let codec = json["streams"][0]["codec_name"]
        .as_str()
        .ok_or_else(|| anyhow!("{} has no video stream", path.display()))?
        .to_string();
    let packets = json["packets"].as_array().map_or(&[][..], Vec::as_slice);
    let mut pts: Vec<f64> = packets
        .iter()
        .filter_map(|packet| packet["pts_time"].as_str()?.parse().ok())
        .collect();
    let frames = packets.len().max(1);
    if pts.len() == packets.len() {
        pts.sort_by(f64::total_cmp);
    } else {
        pts.clear();
    }
    let start = json["format"]["start_time"]
        .as_str()
        .and_then(|text| text.parse().ok())
        .unwrap_or(0.0);
    let format_name = json["format"]["format_name"].as_str().unwrap_or("");
    Ok(VideoIndex {
        container: container_name(format_name, path),
        codec,
        pts,
        start,
        frames,
    })
}

/// Decodes `frame` of `path` to straight-alpha RGBA, as `ffmpeg -pix_fmt rgba`
/// converts it.
pub fn decode(path: &Path, index: &VideoIndex, frame: usize) -> Result<DecodedImage> {
    if frame >= index.frames {
        bail!(
            "{} has {} frames; there is no frame {frame}",
            path.display(),
            index.frames
        );
    }
    let mut command = Command::new(tool("ffmpeg")?);
    command.args(["-v", "error", "-nostdin"]);
    let seek = index.seek_seconds(frame);
    if let Some(seconds) = seek {
        command.args(["-ss", &format!("{seconds:.6}")]);
    }
    command.arg("-i").arg(path);
    if seek.is_none() && frame > 0 {
        command.args(["-vf", &format!("select=eq(n\\,{frame})")]);
    }
    let output = command
        .args(["-frames:v", "1", "-f", "image2pipe", "-c:v", "pam"])
        .args(["-pix_fmt", "rgba", "-"])
        .stdin(Stdio::null())
        .output()
        .context("cannot run ffmpeg")?;
    if !output.status.success() {
        bail!(
            "ffmpeg cannot decode frame {frame} of {}: {}",
            path.display(),
            last_line(&output.stderr)
        );
    }
    if output.stdout.is_empty() {
        bail!("ffmpeg gave no frame {frame} for {}", path.display());
    }
    parse_pam(&output.stdout)
}

/// An RGBA PAM image (`P7`, `DEPTH 4`, `MAXVAL 255`).
fn parse_pam(bytes: &[u8]) -> Result<DecodedImage> {
    const END: &[u8] = b"ENDHDR\n";
    let header_end = bytes
        .windows(END.len())
        .position(|window| window == END)
        .ok_or_else(|| anyhow!("ffmpeg's frame has no PAM header"))?;
    let header = std::str::from_utf8(&bytes[..header_end]).context("PAM header is not text")?;
    let mut lines = header.lines();
    if lines.next() != Some("P7") {
        bail!("ffmpeg's frame is not PAM");
    }
    let (mut width, mut height, mut depth, mut maxval) = (0u32, 0u32, 0u32, 0u32);
    for line in lines {
        let mut words = line.split_whitespace();
        let (Some(key), Some(value)) = (words.next(), words.next()) else {
            continue;
        };
        let number = || value.parse::<u32>().ok();
        match key {
            "WIDTH" => width = number().unwrap_or(0),
            "HEIGHT" => height = number().unwrap_or(0),
            "DEPTH" => depth = number().unwrap_or(0),
            "MAXVAL" => maxval = number().unwrap_or(0),
            _ => {}
        }
    }
    if depth != 4 || maxval != 255 || width == 0 || height == 0 {
        bail!(
            "ffmpeg's frame is not 8-bit RGBA ({width}x{height}, depth {depth}, maxval {maxval})"
        );
    }
    let pixels = &bytes[header_end + END.len()..];
    let size = width as usize * height as usize * 4;
    if pixels.len() < size {
        bail!("ffmpeg's frame is short: {} of {size} bytes", pixels.len());
    }
    Ok(DecodedImage::new(width, height, pixels[..size].to_vec()))
}

/// The name `--probe` prints for ffprobe's `format_name`.
fn container_name(format_name: &str, path: &Path) -> String {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match format_name {
        // One demuxer for the ISO family; the file's brand tells them apart.
        "mov,mp4,m4a,3gp,3g2,mj2" => {
            if quicktime_brand(path) {
                "mov".to_string()
            } else {
                "mp4".to_string()
            }
        }
        "matroska,webm" => {
            if extension == "webm" {
                "webm".to_string()
            } else {
                "mkv".to_string()
            }
        }
        "image2" if !extension.is_empty() => extension,
        other => {
            let first = other.split(',').next().unwrap_or(other);
            first.trim_end_matches("_pipe").to_string()
        }
    }
}

/// Whether the file's `ftyp` box names QuickTime (`qt  `) as its brand.
fn quicktime_brand(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 12];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok_and(|()| &head[4..8] == b"ftyp" && &head[8..12] == b"qt  ")
}

fn last_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no message")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{container_name, decode, find_tool, parse_pam, probe, VideoIndex};
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn a_pam_frame_is_read_with_its_size() {
        let mut bytes =
            b"P7\nWIDTH 2\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n".to_vec();
        bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let image = parse_pam(&bytes).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.rgba8, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(parse_pam(&bytes[..bytes.len() - 1]).is_err());
        let rgb = String::from_utf8_lossy(&bytes).replace("DEPTH 4", "DEPTH 3");
        assert!(parse_pam(rgb.as_bytes()).is_err());
    }

    #[test]
    fn seeking_goes_halfway_between_frames_and_falls_back_without_timestamps() {
        let index = VideoIndex {
            container: "mp4".into(),
            codec: "h264".into(),
            pts: vec![1.5, 1.6, 1.7, 1.7],
            start: 1.5,
            frames: 4,
        };
        assert_eq!(index.seek_seconds(0), None);
        assert!((index.seek_seconds(2).unwrap() - 0.15).abs() < 1e-9);
        assert_eq!(index.seek_seconds(3), None, "two frames at one time");
        let untimed = VideoIndex {
            pts: Vec::new(),
            ..index
        };
        assert_eq!(untimed.seek_seconds(2), None);
    }

    #[test]
    fn containers_are_named_as_probe_prints_them() {
        let path = Path::new("clip.webm");
        assert_eq!(container_name("matroska,webm", path), "webm");
        assert_eq!(container_name("matroska,webm", Path::new("a.mkv")), "mkv");
        assert_eq!(container_name("webp_pipe", Path::new("a.webp")), "webp");
        assert_eq!(container_name("avi", Path::new("a.avi")), "avi");
        // No such file: no QuickTime brand, so the ISO family reads as mp4.
        assert_eq!(
            container_name("mov,mp4,m4a,3gp,3g2,mj2", Path::new("missing.mp4")),
            "mp4"
        );
    }

    /// ffmpeg's own decode of `frame`, through the select filter.
    fn reference(path: &Path, frame: usize) -> Vec<u8> {
        let output = Command::new(find_tool("ffmpeg").unwrap())
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-vf", &format!("select=eq(n\\,{frame})")])
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .output()
            .unwrap();
        assert!(output.status.success());
        output.stdout
    }

    #[test]
    fn every_seeked_frame_equals_ffmpegs_decode_from_the_start() {
        if find_tool("ffmpeg").is_none() || find_tool("ffprobe").is_none() {
            eprintln!("skipped: ffmpeg and ffprobe are not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // B-frames, a keyframe every 12 frames, NTSC timing and a start time
        // that is not zero: the cases a seek can get wrong.
        let clip = dir.path().join("offset.mp4");
        let made = Command::new(find_tool("ffmpeg").unwrap())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=48x32:rate=30000/1001")
            .args(["-frames:v", "30", "-c:v", "libx264", "-g", "12", "-bf", "3"])
            .args(["-pix_fmt", "yuv420p", "-output_ts_offset", "1.5"])
            .arg(&clip)
            .status()
            .unwrap();
        if !made.success() {
            eprintln!("skipped: this ffmpeg cannot encode H.264");
            return;
        }
        let index = probe(&clip).unwrap();
        assert_eq!(index.container, "mp4");
        assert_eq!(index.codec, "h264");
        assert_eq!(index.frames(), 30);
        for frame in 0..index.frames() {
            let ours = decode(&clip, &index, frame).unwrap();
            assert_eq!((ours.width, ours.height), (48, 32));
            assert_eq!(ours.rgba8, reference(&clip, frame), "frame {frame}");
        }
        assert!(decode(&clip, &index, 30).is_err());
    }
}
