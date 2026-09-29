//! Residents made after `set_wire_residents(true)` are locked in memory and still
//! readable; without it, nothing is locked.
#![cfg(target_os = "macos")]

use loadngo_metal_compute::Gpu;

#[test]
fn wired_residents_are_locked_and_readable() {
    let plain_gpu = Gpu::new().unwrap();
    let plain = plain_gpu.resident(&[1, 2, 3, 4]).unwrap();
    assert_eq!(plain_gpu.wired_bytes(), 0);
    assert_eq!(plain.as_bytes(), &[1, 2, 3, 4]);

    let mut gpu = Gpu::new().unwrap();
    gpu.set_wire_residents(true);
    let bytes: Vec<u8> = (0..4096_u32).map(|i| (i * 7) as u8).collect();
    let wired = gpu.resident(&bytes).unwrap();
    assert_eq!(gpu.wire_failures(), 0, "mlock refused (wire limit?)");
    // The whole arena the resident was carved from is locked.
    assert!(gpu.wired_bytes() >= bytes.len());
    assert_eq!(wired.as_bytes(), &bytes[..]);
}
