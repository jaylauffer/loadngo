//! Pictures: opening a file at one of its frames, the pixel edits the C++
//! BitmapEditor made, and saving.
//!
//! PNG, JPEG, BMP, TIFF and GIF are read and written with the `image` crate;
//! GIF frames come out composited onto the full canvas, as a viewer shows
//! them. Anything else ffmpeg can read (MP4, MOV, WebM, WebP...) comes
//! through [`crate::video`], frame by frame. Pixels are straight-alpha RGBA8
//! (`DecodedImage`, what the host uploads and the software rasterizer
//! blends), top row first.
//!
//! The C++ editor held Windows DIB pixels, which `AlphaBlend` expects
//! premultiplied, so setting a pixel's alpha there also premultiplied that
//! pixel. Here alpha is set on its own and Pre-Multiply Alpha is the
//! explicit command, so a PNG's colours survive an alpha edit.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use image::{AnimationDecoder, ColorType, ImageEncoder, ImageFormat};
use loadngo_host_core::DecodedImage;

use crate::video::{self, VideoIndex};

/// A file's kind, as `--probe` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Bmp,
    Tiff,
    Gif,
    /// Read through ffmpeg: a video container (`mp4`, `mov`, `webm`...) or a
    /// still format the image crate does not read (`webp`...).
    Ffmpeg(String),
}

impl Format {
    pub fn name(&self) -> &str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
            Self::Gif => "gif",
            Self::Ffmpeg(name) => name,
        }
    }

    /// The format a file saved at `path` gets, from its extension.
    pub fn for_saving(path: &Path) -> Result<Self> {
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        Ok(match extension.as_str() {
            "png" => Self::Png,
            "jpg" | "jpeg" => Self::Jpeg,
            "bmp" | "dib" => Self::Bmp,
            "tif" | "tiff" => Self::Tiff,
            "gif" => Self::Gif,
            _ => bail!(
                "cannot save {}: name it .png, .jpg, .bmp, .tiff or .gif",
                path.display()
            ),
        })
    }

    /// Whether [`save`] writes this format.
    pub fn writable(&self) -> bool {
        !matches!(self, Self::Ffmpeg(_))
    }

    fn from_image(format: ImageFormat) -> Option<Self> {
        Some(match format {
            ImageFormat::Png => Self::Png,
            ImageFormat::Jpeg => Self::Jpeg,
            ImageFormat::Bmp => Self::Bmp,
            ImageFormat::Tiff => Self::Tiff,
            ImageFormat::Gif => Self::Gif,
            _ => return None,
        })
    }
}

/// What the editor knows of a file without decoding it again.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub path: PathBuf,
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub frames: usize,
    video: Option<VideoIndex>,
}

impl Source {
    /// The file a save that kept every frame wrote, standing in for this one.
    pub fn saved_as(&self, saved: &Saved) -> Self {
        Self {
            path: saved.path.clone(),
            format: saved.format.clone(),
            width: self.width,
            height: self.height,
            frames: saved.frames,
            video: None,
        }
    }
}

/// A file opened at one frame.
#[derive(Debug, Clone)]
pub struct Opened {
    pub source: Arc<Source>,
    pub frame: usize,
    pub image: DecodedImage,
}

/// Opens `path` at `frame`: reads it, finds its format and frame count, and
/// decodes that frame.
pub fn open(path: &Path, frame: usize) -> Result<Opened> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let still = image::guess_format(&bytes)
        .ok()
        .and_then(Format::from_image)
        .map(|format| decode_still(&bytes, &format, frame).map(|found| (format, found)));
    let failure = match still {
        Some(Ok((format, (image, frames)))) => {
            let source = Source {
                path: path.to_path_buf(),
                format,
                width: image.width,
                height: image.height,
                frames,
                video: None,
            };
            return Ok(Opened {
                source: Arc::new(source),
                frame,
                image,
            });
        }
        Some(Err(error)) => Some(error),
        None => None,
    };
    // Not a still the image crate reads (or a variant it rejects): ffmpeg.
    let through_ffmpeg = video::probe(path).and_then(|index| {
        let image = video::decode(path, &index, frame)?;
        Ok((index, image))
    });
    match (through_ffmpeg, failure) {
        (Ok((index, image)), _) => Ok(Opened {
            source: Arc::new(Source {
                path: path.to_path_buf(),
                format: Format::Ffmpeg(index.container.clone()),
                width: image.width,
                height: image.height,
                frames: index.frames(),
                video: Some(index),
            }),
            frame,
            image,
        }),
        (Err(_), Some(still_error)) => Err(still_error),
        (Err(video_error), None) => Err(anyhow!(
            "{} is not a PNG, JPEG, BMP, TIFF or GIF image, and ffmpeg could not read it: {video_error}",
            path.display()
        )),
    }
}

/// Decodes `frame` of a file already opened, without probing it again.
pub fn load_frame(source: &Source, frame: usize) -> Result<DecodedImage> {
    if let Some(index) = &source.video {
        return video::decode(&source.path, index, frame);
    }
    let bytes = std::fs::read(&source.path)
        .with_context(|| format!("cannot read {}", source.path.display()))?;
    decode_still(&bytes, &source.format, frame).map(|(image, _)| image)
}

/// `frame` of an image-crate format and the file's frame count.
fn decode_still(bytes: &[u8], format: &Format, frame: usize) -> Result<(DecodedImage, usize)> {
    if *format == Format::Gif {
        let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
        let mut wanted = None;
        let mut frames = 0;
        for (index, decoded) in decoder.into_frames().enumerate() {
            let decoded = decoded?;
            if index == frame {
                wanted = Some(decoded.into_buffer());
            }
            frames += 1;
        }
        let buffer = wanted.ok_or_else(|| no_such_frame(frame, frames))?;
        let (width, height) = buffer.dimensions();
        return Ok((DecodedImage::new(width, height, buffer.into_raw()), frames));
    }
    if frame != 0 {
        return Err(no_such_frame(frame, 1));
    }
    let image_format = match format {
        Format::Png => ImageFormat::Png,
        Format::Jpeg => ImageFormat::Jpeg,
        Format::Bmp => ImageFormat::Bmp,
        Format::Tiff => ImageFormat::Tiff,
        _ => bail!("{} is not read by the image crate", format.name()),
    };
    let rgba = image::load_from_memory_with_format(bytes, image_format)?.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok((DecodedImage::new(width, height, rgba.into_raw()), 1))
}

fn no_such_frame(frame: usize, frames: usize) -> anyhow::Error {
    anyhow!("the file has {frames} frame(s); there is no frame {frame}")
}

/// The RGBA of pixel (`x`, `y`), or `None` outside the image.
pub fn pixel(image: &DecodedImage, x: u32, y: u32) -> Option<[u8; 4]> {
    let index = offset(image, x, y)?;
    let p = &image.rgba8[index..index + 4];
    Some([p[0], p[1], p[2], p[3]])
}

/// Replaces pixel (`x`, `y`), returning what it was.
pub fn set_pixel(image: &mut DecodedImage, x: u32, y: u32, rgba: [u8; 4]) -> Option<[u8; 4]> {
    let index = offset(image, x, y)?;
    let before = pixel(image, x, y)?;
    image.rgba8[index..index + 4].copy_from_slice(&rgba);
    Some(before)
}

/// Sets pixel (`x`, `y`)'s alpha and keeps its colour, returning what it was.
pub fn set_alpha(image: &mut DecodedImage, x: u32, y: u32, alpha: u8) -> Option<[u8; 4]> {
    let [r, g, b, _] = pixel(image, x, y)?;
    set_pixel(image, x, y, [r, g, b, alpha])
}

/// Multiplies each colour by its alpha, as the C++ `MultiplyAlpha` did
/// (`c * a / 255`, rounded down).
pub fn premultiply(image: &mut DecodedImage) {
    for p in image.rgba8.as_chunks_mut::<4>().0 {
        let alpha = u16::from(p[3]);
        for channel in &mut p[..3] {
            *channel = (u16::from(*channel) * alpha / 255) as u8;
        }
    }
}

/// Sets every alpha to 255, as the C++ `MakeOpaque` did.
pub fn make_opaque(image: &mut DecodedImage) {
    for p in image.rgba8.as_chunks_mut::<4>().0 {
        p[3] = 255;
    }
}

fn offset(image: &DecodedImage, x: u32, y: u32) -> Option<usize> {
    (x < image.width && y < image.height)
        .then(|| (y as usize * image.width as usize + x as usize) * 4)
        .filter(|index| index + 4 <= image.rgba8.len())
}

/// What to write: the frame being edited, and every frame edited so far
/// (including that one), for formats that keep every frame.
#[derive(Debug, Clone)]
pub struct SaveRequest {
    pub out: PathBuf,
    pub source: Arc<Source>,
    pub frame: usize,
    pub edited: Vec<(usize, DecodedImage)>,
}

/// What [`save`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub path: PathBuf,
    pub format: Format,
    pub frames: usize,
    /// Every frame of the source went into the file, so it can stand in for
    /// the source from now on.
    pub whole: bool,
}

/// Writes the request: an animated GIF saved as GIF keeps every frame, with
/// the edited ones replaced; anything else writes the frame being edited.
/// The file is written beside its destination and renamed into place.
pub fn save(request: &SaveRequest) -> Result<Saved> {
    let format = Format::for_saving(&request.out)?;
    let edited = |index: usize| {
        request
            .edited
            .iter()
            .find(|(frame, _)| *frame == index)
            .map(|(_, image)| image)
    };
    let source = &request.source;
    let (bytes, frames) = if format == Format::Gif
        && source.format == Format::Gif
        && source.frames > 1
    {
        let original = std::fs::read(&source.path)
            .with_context(|| format!("cannot read {}", source.path.display()))?;
        let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(&original))?;
        let mut frames = Vec::with_capacity(source.frames);
        for (index, decoded) in decoder.into_frames().enumerate() {
            let decoded = decoded?;
            frames.push(match edited(index) {
                Some(image) => image::Frame::from_parts(rgba_buffer(image)?, 0, 0, decoded.delay()),
                None => decoded,
            });
        }
        let count = frames.len();
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder.set_repeat(image::codecs::gif::Repeat::Infinite)?;
            encoder.encode_frames(frames)?;
        }
        (bytes, count)
    } else {
        let image = match edited(request.frame) {
            Some(image) => image.clone(),
            None => load_frame(source, request.frame)?,
        };
        (encode(&image, &format)?, 1)
    };
    write_replacing(&request.out, &bytes)?;
    Ok(Saved {
        path: request.out.clone(),
        format,
        frames,
        whole: frames == source.frames,
    })
}

/// One frame in a still format. JPEG has no alpha, so it is dropped.
pub fn encode(image: &DecodedImage, format: &Format) -> Result<Vec<u8>> {
    let (width, height) = (image.width, image.height);
    let mut bytes = Vec::new();
    match format {
        Format::Png => image::codecs::png::PngEncoder::new(&mut bytes).write_image(
            &image.rgba8,
            width,
            height,
            ColorType::Rgba8,
        )?,
        Format::Jpeg => {
            let rgb: Vec<u8> = image
                .rgba8
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 92).write_image(
                &rgb,
                width,
                height,
                ColorType::Rgb8,
            )?;
        }
        Format::Bmp => image::codecs::bmp::BmpEncoder::new(&mut bytes).write_image(
            &image.rgba8,
            width,
            height,
            ColorType::Rgba8,
        )?,
        Format::Tiff => image::codecs::tiff::TiffEncoder::new(Cursor::new(&mut bytes))
            .write_image(&image.rgba8, width, height, ColorType::Rgba8)?,
        Format::Gif => {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder.encode_frame(image::Frame::new(rgba_buffer(image)?))?;
        }
        Format::Ffmpeg(name) => bail!("cannot write {name}; save as PNG, JPEG, BMP, TIFF or GIF"),
    }
    Ok(bytes)
}

fn rgba_buffer(image: &DecodedImage) -> Result<image::RgbaImage> {
    image::RgbaImage::from_raw(image.width, image.height, image.rgba8.clone()).ok_or_else(|| {
        anyhow!(
            "the frame's pixels do not fill {}x{}",
            image.width,
            image.height
        )
    })
}

/// Writes `bytes` to a new file beside `path`, then renames it over `path`,
/// so a failed save leaves the old file whole.
fn write_replacing(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("{} names no file", path.display()))?;
    let mut partial = name.to_os_string();
    partial.push(format!(".partial-{}", std::process::id()));
    let partial = path.with_file_name(partial);
    std::fs::write(&partial, bytes)
        .with_context(|| format!("cannot write {}", partial.display()))?;
    std::fs::rename(&partial, path).with_context(|| {
        let _ = std::fs::remove_file(&partial);
        format!("cannot replace {}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w`x`h` image whose pixel (x, y) is (x*10, y*10, x+y, 200 + x).
    fn pattern(width: u32, height: u32) -> DecodedImage {
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&[
                    (x * 10) as u8,
                    (y * 10) as u8,
                    (x + y) as u8,
                    (200 + x) as u8,
                ]);
            }
        }
        DecodedImage::new(width, height, rgba)
    }

    fn saved_as(dir: &Path, name: &str, image: &DecodedImage) -> PathBuf {
        let path = dir.join(name);
        let format = Format::for_saving(&path).unwrap();
        std::fs::write(&path, encode(image, &format).unwrap()).unwrap();
        path
    }

    #[test]
    fn lossless_formats_open_with_the_pixels_they_were_saved_with() {
        let dir = tempfile::tempdir().unwrap();
        let image = pattern(7, 5);
        for (name, format) in [
            ("a.png", Format::Png),
            ("a.bmp", Format::Bmp),
            ("a.tiff", Format::Tiff),
        ] {
            let opened = open(&saved_as(dir.path(), name, &image), 0).unwrap();
            assert_eq!(opened.source.format, format, "{name}");
            assert_eq!(
                (
                    opened.source.width,
                    opened.source.height,
                    opened.source.frames
                ),
                (7, 5, 1)
            );
            assert_eq!(opened.image.rgba8, image.rgba8, "{name}");
        }
    }

    #[test]
    fn a_jpeg_opens_close_to_what_was_saved_and_opaque() {
        let dir = tempfile::tempdir().unwrap();
        let mut image = DecodedImage::new(16, 16, [90, 140, 200, 255].repeat(256));
        image.rgba8[3] = 10; // JPEG drops alpha
        let opened = open(&saved_as(dir.path(), "a.jpg", &image), 0).unwrap();
        assert_eq!(opened.source.format, Format::Jpeg);
        let p = pixel(&opened.image, 8, 8).unwrap();
        for (ours, theirs) in p.iter().zip([90, 140, 200]) {
            assert!(ours.abs_diff(theirs) <= 3, "{p:?}");
        }
        assert!(opened
            .image
            .rgba8
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[3] == 255));
    }

    fn three_frame_gif(dir: &Path) -> PathBuf {
        let colours = [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]];
        let frames: Vec<image::Frame> = colours
            .iter()
            .map(|colour| {
                image::Frame::from_parts(
                    image::RgbaImage::from_raw(4, 3, colour.repeat(12)).unwrap(),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(100, 1),
                )
            })
            .collect();
        let path = dir.join("anim.gif");
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder.encode_frames(frames).unwrap();
        }
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn a_gif_opens_at_any_of_its_frames_and_counts_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_frame_gif(dir.path());
        let first = open(&path, 0).unwrap();
        assert_eq!(first.source.format, Format::Gif);
        assert_eq!(first.source.frames, 3);
        assert_eq!(pixel(&first.image, 0, 0), Some([255, 0, 0, 255]));
        let third = open(&path, 2).unwrap();
        assert_eq!(pixel(&third.image, 3, 2), Some([0, 0, 255, 255]));
        assert_eq!(
            load_frame(&first.source, 1).unwrap().rgba8,
            [0, 255, 0, 255].repeat(12)
        );
        assert!(open(&path, 3).is_err());
    }

    #[test]
    fn saving_an_animated_gif_keeps_every_frame_with_the_edited_one_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_frame_gif(dir.path());
        let opened = open(&path, 1).unwrap();
        let mut image = opened.image.clone();
        set_pixel(&mut image, 0, 0, [255, 255, 255, 255]);
        let out = dir.path().join("out.gif");
        let saved = save(&SaveRequest {
            out: out.clone(),
            source: opened.source.clone(),
            frame: 1,
            edited: vec![(1, image)],
        })
        .unwrap();
        assert_eq!((saved.frames, saved.whole), (3, true));
        assert_eq!(
            pixel(&open(&out, 0).unwrap().image, 0, 0),
            Some([255, 0, 0, 255])
        );
        let edited = open(&out, 1).unwrap();
        assert_eq!(pixel(&edited.image, 0, 0), Some([255, 255, 255, 255]));
        assert_eq!(pixel(&edited.image, 1, 0), Some([0, 255, 0, 255]));

        // As PNG, only the frame being edited goes out.
        let png = dir.path().join("frame.png");
        let saved = save(&SaveRequest {
            out: png.clone(),
            source: opened.source.clone(),
            frame: 2,
            edited: Vec::new(),
        })
        .unwrap();
        assert_eq!((saved.frames, saved.whole), (1, false));
        assert_eq!(
            open(&png, 0).unwrap().image.rgba8,
            [0, 0, 255, 255].repeat(12)
        );
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.contains(".partial-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn the_c_plus_plus_edits_set_alpha_premultiply_and_make_opaque() {
        let mut image = DecodedImage::new(
            3,
            1,
            vec![200, 100, 50, 128, 10, 20, 30, 0, 255, 255, 1, 200],
        );
        assert_eq!(set_alpha(&mut image, 1, 0, 77), Some([10, 20, 30, 0]));
        assert_eq!(pixel(&image, 1, 0), Some([10, 20, 30, 77]));
        assert_eq!(set_alpha(&mut image, 3, 0, 1), None);
        premultiply(&mut image);
        assert_eq!(pixel(&image, 0, 0), Some([100, 50, 25, 128]));
        assert_eq!(pixel(&image, 1, 0), Some([3, 6, 9, 77]));
        // 255 at alpha 200 is 200: the divisor is 255, as in the C++.
        assert_eq!(pixel(&image, 2, 0), Some([200, 200, 0, 200]));
        make_opaque(&mut image);
        assert_eq!(pixel(&image, 0, 0), Some([100, 50, 25, 255]));
    }

    #[test]
    fn a_file_that_is_no_picture_is_refused_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"not a picture").unwrap();
        let error = open(&path, 0).unwrap_err().to_string();
        assert!(
            error.contains("not a PNG, JPEG, BMP, TIFF or GIF"),
            "{error}"
        );
        assert!(Format::for_saving(Path::new("a.webp")).is_err());
    }
}
