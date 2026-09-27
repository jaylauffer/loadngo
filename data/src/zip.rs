//! Reads the members of a zip file (PKWARE APPNOTE 6.3): the central directory,
//! including Zip64, and each member's bytes, stored or deflated, checked against the
//! CRC-32 the zip records. Used to unpack zips into an Archive CAS so their contents are
//! stored, and deduplicated, as ordinary files.
//!
//! Encrypted members and compression methods other than stored and deflate are reported
//! as unsupported; callers leave such a zip as it is.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

use anyhow::{bail, Context, Result};

const END_OF_CENTRAL_DIRECTORY: u32 = 0x0605_4b50;
const ZIP64_END_LOCATOR: u32 = 0x0706_4b50;
const ZIP64_END_OF_CENTRAL_DIRECTORY: u32 = 0x0606_4b50;
const CENTRAL_FILE_HEADER: u32 = 0x0201_4b50;
const LOCAL_FILE_HEADER: u32 = 0x0403_4b50;

/// Stored (no compression).
pub const METHOD_STORED: u16 = 0;
/// Deflate.
pub const METHOD_DEFLATE: u16 = 8;

/// One member of a zip, from its central directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    /// The name as recorded (UTF-8, or CP437 decoded), `/`-separated.
    pub name: String,
    pub is_dir: bool,
    pub method: u16,
    pub encrypted: bool,
    pub compressed_size: u64,
    pub size: u64,
    pub crc32: u32,
    /// Modification time, from the extended-timestamp field or the DOS date and time.
    pub modified_unix_secs: Option<u64>,
    local_header_offset: u64,
}

impl ZipEntry {
    /// Whether [`member_reader`] can read it.
    pub fn is_supported(&self) -> bool {
        !self.encrypted && matches!(self.method, METHOD_STORED | METHOD_DEFLATE)
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

fn read_at(file: &mut File, offset: u64, length: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)
        .with_context(|| format!("zip is truncated at offset {offset}"))?;
    Ok(bytes)
}

/// CP437 for bytes 0x80-0xFF, the encoding of zip names without the UTF-8 flag.
const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

fn decode_name(bytes: &[u8], utf8_flag: bool) -> String {
    if utf8_flag || bytes.is_ascii() {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    // Many tools write UTF-8 without setting the flag; prefer it when it is valid.
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    let high: Vec<char> = CP437_HIGH.chars().collect();
    bytes
        .iter()
        .map(|&b| {
            if b < 0x80 {
                char::from(b)
            } else {
                high[usize::from(b - 0x80)]
            }
        })
        .collect()
}

/// Days from 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A DOS date and time (local time, taken as UTC) as Unix seconds.
fn dos_time(date: u16, time: u16) -> Option<u64> {
    let (year, month, day) = (
        1980 + i64::from(date >> 9),
        i64::from((date >> 5) & 0x0f),
        i64::from(date & 0x1f),
    );
    if !(1..=12).contains(&month) || day == 0 {
        return None;
    }
    let seconds = days_from_civil(year, month, day) * 86_400
        + i64::from(time >> 11) * 3600
        + i64::from((time >> 5) & 0x3f) * 60
        + i64::from(time & 0x1f) * 2;
    u64::try_from(seconds).ok()
}

/// Every member of the zip in `file`, in central-directory order.
///
/// # Errors
/// When the file is not a zip or its central directory is damaged.
pub fn read_entries(file: &mut File) -> Result<Vec<ZipEntry>> {
    let length = file.seek(SeekFrom::End(0))?;
    if length < 22 {
        bail!("too short to be a zip");
    }
    // The end record is the last 22 bytes plus a comment of up to 65,535 bytes.
    let tail_length = length.min(22 + 65_535);
    let tail = read_at(file, length - tail_length, usize::try_from(tail_length)?)?;
    let at = (0..=tail.len() - 22)
        .rev()
        .find(|&i| u32_at(&tail, i) == END_OF_CENTRAL_DIRECTORY)
        .context("no end of central directory: not a zip")?;
    let end_offset = length - tail_length + at as u64;
    let mut count = u64::from(u16_at(&tail, at + 10));
    let mut directory_size = u64::from(u32_at(&tail, at + 12));
    let mut directory_offset = u64::from(u32_at(&tail, at + 16));

    if count == 0xffff || directory_size == 0xffff_ffff || directory_offset == 0xffff_ffff {
        if end_offset < 20 {
            bail!("zip64 end locator missing");
        }
        let locator = read_at(file, end_offset - 20, 20)?;
        if u32_at(&locator, 0) != ZIP64_END_LOCATOR {
            bail!("zip64 end locator missing");
        }
        let record = read_at(file, u64_at(&locator, 8), 56)?;
        if u32_at(&record, 0) != ZIP64_END_OF_CENTRAL_DIRECTORY {
            bail!("zip64 end of central directory missing");
        }
        count = u64_at(&record, 32);
        directory_size = u64_at(&record, 40);
        directory_offset = u64_at(&record, 48);
    }
    if directory_offset.saturating_add(directory_size) > length {
        bail!("central directory lies outside the file");
    }
    let directory = read_at(file, directory_offset, usize::try_from(directory_size)?)?;

    let mut entries = Vec::with_capacity(usize::try_from(count).unwrap_or(0).min(1 << 20));
    let mut at = 0usize;
    for _ in 0..count {
        if at + 46 > directory.len() || u32_at(&directory, at) != CENTRAL_FILE_HEADER {
            bail!("central directory entry {} is damaged", entries.len());
        }
        let flags = u16_at(&directory, at + 8);
        let method = u16_at(&directory, at + 10);
        let (time, date) = (u16_at(&directory, at + 12), u16_at(&directory, at + 14));
        let crc32 = u32_at(&directory, at + 16);
        let mut compressed_size = u64::from(u32_at(&directory, at + 20));
        let mut size = u64::from(u32_at(&directory, at + 24));
        let name_length = usize::from(u16_at(&directory, at + 28));
        let extra_length = usize::from(u16_at(&directory, at + 30));
        let comment_length = usize::from(u16_at(&directory, at + 32));
        let mut local_header_offset = u64::from(u32_at(&directory, at + 42));
        let name_start = at + 46;
        let extra_start = name_start + name_length;
        let next = extra_start + extra_length + comment_length;
        if next > directory.len() {
            bail!("central directory entry {} is truncated", entries.len());
        }
        let name = decode_name(&directory[name_start..extra_start], flags & 0x0800 != 0);
        let mut modified_unix_secs = dos_time(date, time);

        // Extra fields: Zip64 sizes and offset, and the extended timestamp.
        let extra = &directory[extra_start..extra_start + extra_length];
        let mut e = 0usize;
        while e + 4 <= extra.len() {
            let (id, len) = (u16_at(extra, e), usize::from(u16_at(extra, e + 2)));
            let data = &extra[e + 4..(e + 4 + len).min(extra.len())];
            match id {
                0x0001 => {
                    let mut d = 0usize;
                    let mut take = |value: &mut u64, limit: u64| {
                        if *value == limit && d + 8 <= data.len() {
                            *value = u64_at(data, d);
                            d += 8;
                        }
                    };
                    take(&mut size, 0xffff_ffff);
                    take(&mut compressed_size, 0xffff_ffff);
                    take(&mut local_header_offset, 0xffff_ffff);
                }
                0x5455 if data.len() >= 5 && data[0] & 1 != 0 => {
                    modified_unix_secs = u64::try_from(i32::from_le_bytes(
                        data[1..5].try_into().expect("four bytes"),
                    ))
                    .ok();
                }
                _ => {}
            }
            e += 4 + len;
        }

        entries.push(ZipEntry {
            is_dir: name.ends_with('/'),
            name,
            method,
            encrypted: flags & 1 != 0,
            compressed_size,
            size,
            crc32,
            modified_unix_secs,
            local_header_offset,
        });
        at = next;
    }
    Ok(entries)
}

/// Reads a member's bytes and checks size and CRC-32 at the end.
struct Checked<R> {
    inner: R,
    crc: crc32fast::Hasher,
    read: u64,
    expected_size: u64,
    expected_crc: u32,
    name: String,
}

impl<R: Read> Read for Checked<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(out)?;
        if n == 0 {
            let crc = std::mem::take(&mut self.crc).finalize();
            if self.read != self.expected_size || crc != self.expected_crc {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{}: read {} bytes with CRC {crc:08x}; the zip records {} bytes, CRC {:08x}",
                        self.name, self.read, self.expected_size, self.expected_crc
                    ),
                ));
            }
            return Ok(0);
        }
        self.crc.update(&out[..n]);
        self.read += n as u64;
        Ok(n)
    }
}

/// A reader of `entry`'s uncompressed bytes; it fails at the end if the size or CRC-32
/// differs from what the zip records.
///
/// # Errors
/// An unsupported member, or a damaged local header.
pub fn member_reader<'a>(file: &'a mut File, entry: &ZipEntry) -> Result<Box<dyn Read + 'a>> {
    if !entry.is_supported() {
        bail!(
            "{}: {} member not supported",
            entry.name,
            if entry.encrypted {
                "encrypted".to_string()
            } else {
                format!("compression method {}", entry.method)
            }
        );
    }
    let header = read_at(file, entry.local_header_offset, 30)?;
    if u32_at(&header, 0) != LOCAL_FILE_HEADER {
        bail!("{}: local header is damaged", entry.name);
    }
    let skip = u64::from(u16_at(&header, 26)) + u64::from(u16_at(&header, 28));
    file.seek(SeekFrom::Start(entry.local_header_offset + 30 + skip))?;
    let raw = Read::take(file, entry.compressed_size);
    let inner: Box<dyn Read + 'a> = match entry.method {
        METHOD_STORED => Box::new(raw),
        _ => Box::new(flate2::read::DeflateDecoder::new(raw)),
    };
    Ok(Box::new(Checked {
        inner,
        crc: crc32fast::Hasher::new(),
        read: 0,
        expected_size: entry.size,
        expected_crc: entry.crc32,
        name: entry.name.clone(),
    }))
}

/// Whether the file starts like a zip (a local header, or the end record of an empty
/// zip).
pub fn looks_like_zip(file: &mut File) -> bool {
    let mut magic = [0u8; 4];
    file.seek(SeekFrom::Start(0)).is_ok()
        && file.read_exact(&mut magic).is_ok()
        && matches!(
            u32::from_le_bytes(magic),
            LOCAL_FILE_HEADER | END_OF_CENTRAL_DIRECTORY
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// A zip written by hand: `hello.txt` stored, `dir/` a directory, and
    /// `dir/big.txt` deflated.
    fn sample_zip(path: &std::path::Path, big: &[u8]) {
        let mut out = Vec::new();
        let mut central = Vec::new();
        let mut members: Vec<(&str, u16, Vec<u8>, &[u8])> = Vec::new();
        members.push((
            "hello.txt",
            METHOD_STORED,
            b"hello zip\n".to_vec(),
            b"hello zip\n",
        ));
        members.push(("dir/", METHOD_STORED, Vec::new(), b""));
        let mut deflated =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        deflated.write_all(big).unwrap();
        members.push((
            "dir/big.txt",
            METHOD_DEFLATE,
            deflated.finish().unwrap(),
            big,
        ));
        for (name, method, data, plain) in &members {
            let offset = out.len() as u32;
            let crc = crc32fast::hash(plain);
            let (date, time) = ((46 << 9) | (9 << 5) | 28, (3 << 11) | (4 << 5));
            let header = |sig: u32, out: &mut Vec<u8>, central: bool| {
                out.extend(sig.to_le_bytes());
                if central {
                    out.extend(20u16.to_le_bytes());
                }
                out.extend(20u16.to_le_bytes());
                out.extend(0x0800u16.to_le_bytes());
                out.extend(method.to_le_bytes());
                out.extend((time as u16).to_le_bytes());
                out.extend((date as u16).to_le_bytes());
                out.extend(crc.to_le_bytes());
                out.extend((data.len() as u32).to_le_bytes());
                out.extend((plain.len() as u32).to_le_bytes());
                out.extend((name.len() as u16).to_le_bytes());
                out.extend(0u16.to_le_bytes());
            };
            header(LOCAL_FILE_HEADER, &mut out, false);
            out.extend(name.as_bytes());
            out.extend(data);
            header(CENTRAL_FILE_HEADER, &mut central, true);
            central.extend([0u8; 6]); // comment length, disk, internal attributes
            central.extend(0u32.to_le_bytes()); // external attributes
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
        }
        let directory_offset = out.len() as u32;
        out.extend(&central);
        out.extend(END_OF_CENTRAL_DIRECTORY.to_le_bytes());
        out.extend([0u8; 4]);
        out.extend((members.len() as u16).to_le_bytes());
        out.extend((members.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(directory_offset.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn members_read_back_exactly_and_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.zip");
        let big = "loadngo ".repeat(10_000).into_bytes();
        sample_zip(&path, &big);
        let mut file = File::open(&path).unwrap();
        assert!(looks_like_zip(&mut file));
        let entries = read_entries(&mut file).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["hello.txt", "dir/", "dir/big.txt"]);
        assert!(entries[1].is_dir);
        assert_eq!(entries[2].size, big.len() as u64);
        assert!(entries[2].compressed_size < 1000, "deflated");
        assert_eq!(
            entries[0].modified_unix_secs,
            dos_time((46 << 9) | (9 << 5) | 28, (3 << 11) | (4 << 5))
        );
        let mut text = Vec::new();
        member_reader(&mut file, &entries[0])
            .unwrap()
            .read_to_end(&mut text)
            .unwrap();
        assert_eq!(text, b"hello zip\n");
        let mut text = Vec::new();
        member_reader(&mut file, &entries[2])
            .unwrap()
            .read_to_end(&mut text)
            .unwrap();
        assert_eq!(text, big);

        // A member whose recorded CRC is wrong fails at the end of the read.
        let mut wrong = entries[0].clone();
        wrong.crc32 ^= 1;
        let mut sink = Vec::new();
        let error = member_reader(&mut file, &wrong)
            .unwrap()
            .read_to_end(&mut sink)
            .unwrap_err();
        assert!(error.to_string().contains("the zip records"), "{error}");
    }

    #[test]
    fn unsupported_members_and_non_zips_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.zip");
        sample_zip(&path, b"x");
        let mut file = File::open(&path).unwrap();
        let mut entries = read_entries(&mut file).unwrap();
        entries[0].encrypted = true;
        assert!(!entries[0].is_supported());
        assert!(member_reader(&mut file, &entries[0]).is_err());
        entries[2].method = 14; // LZMA
        assert!(member_reader(&mut file, &entries[2]).is_err());
        let text = dir.path().join("text.txt");
        std::fs::write(&text, b"not a zip at all, just some text").unwrap();
        let mut file = File::open(&text).unwrap();
        assert!(!looks_like_zip(&mut file));
        assert!(read_entries(&mut file).is_err());
    }

    #[test]
    fn dos_times_and_cp437_names_decode() {
        assert_eq!(dos_time((46 << 9) | (9 << 5) | 28, 0), Some(1_790_553_600));
        assert_eq!(decode_name(&[b'c', b'a', b'f', 0x82], false), "café");
        assert_eq!(decode_name("日本".as_bytes(), false), "日本");
    }
}
