//! Small, bounded SPA format POD codec. Wire constants/layout are from
//! PipeWire 1.4.2 spa/{utils/type,param/format,param/audio/raw,pod/pod}.h.
//! Encoding in Rust avoids a C shim/build-time target-header dependency.

pub const RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
#[cfg(target_endian = "little")]
const F32: u32 = 0x11b;
#[cfg(target_endian = "big")]
const F32: u32 = 0x11c;

#[repr(C, align(8))]
pub struct FormatPod(pub [u32; 42]);

pub fn format() -> FormatPod {
    let mut words = [0; 42];
    words[..4].copy_from_slice(&[160, 15, 0x40003, 3]);
    for (index, (key, kind, value)) in [
        (1, 3, 1),
        (2, 3, 1),
        (0x10001, 3, F32),
        (0x10003, 4, RATE),
        (0x10004, 4, CHANNELS),
    ]
    .into_iter()
    .enumerate()
    {
        words[4 + index * 6..10 + index * 6].copy_from_slice(&[key, 0, 4, kind, value, 0]);
    }
    // Position array: two Ids, FL and FR. Every property is padded to 8 bytes.
    words[34..].copy_from_slice(&[0x10005, 0, 16, 13, 4, 3, 3, 4]);
    FormatPod(words)
}

fn word(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// Accept only the fixed format offered by this adapter. Unknown properties
/// are skipped with checked lengths; malformed/truncated PODs are rejected.
pub fn is_expected_format(bytes: &[u8]) -> bool {
    let Some(end) = word(bytes, 0).and_then(|n| usize::try_from(n).ok()?.checked_add(8)) else {
        return false;
    };
    if end != bytes.len() || word(bytes, 4) != Some(15) || word(bytes, 8) != Some(0x40003) {
        return false;
    }
    let mut found = [false; 5];
    let mut offset = 16usize;
    while offset < end {
        let Some(size) = word(bytes, offset + 8).map(|n| n as usize) else {
            return false;
        };
        let Some(next) = offset
            .checked_add(16)
            .and_then(|n| n.checked_add(size))
            .and_then(|n| n.checked_add(7))
            .map(|n| n & !7)
        else {
            return false;
        };
        if next > end {
            return false;
        }
        for (i, (key, kind, value)) in [
            (1, 3, 1),
            (2, 3, 1),
            (0x10001, 3, F32),
            (0x10003, 4, RATE),
            (0x10004, 4, CHANNELS),
        ]
        .into_iter()
        .enumerate()
        {
            if word(bytes, offset) == Some(key) {
                if found[i]
                    || size != 4
                    || word(bytes, offset + 12) != Some(kind)
                    || word(bytes, offset + 16) != Some(value)
                {
                    return false;
                }
                found[i] = true;
            }
        }
        offset = next;
    }
    found.into_iter().all(|v| v)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bytes() -> Vec<u8> {
        format().0.iter().flat_map(|n| n.to_ne_bytes()).collect()
    }

    #[test]
    fn format_is_fixed_stereo_float_with_aligned_properties() {
        assert_eq!(std::mem::align_of::<FormatPod>(), 8);
        assert_eq!(std::mem::size_of::<FormatPod>(), 168);
        assert!(is_expected_format(&bytes()));
    }
    #[test]
    fn rejects_every_truncation_and_wrong_rate() {
        let mut data = bytes();
        for n in 0..data.len() {
            assert!(!is_expected_format(&data[..n]));
        }
        data[104..108].copy_from_slice(&44_100u32.to_ne_bytes());
        assert!(!is_expected_format(&data));
    }
    #[test]
    fn rejects_oversized_property() {
        let mut data = bytes();
        data[24..28].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(!is_expected_format(&data));
    }
}
