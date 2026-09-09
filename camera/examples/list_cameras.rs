//! Prints the cameras this platform can see, as `loadngo-camera` resolves
//! them. Handy for checking enumeration on a new machine without building a
//! preview window.

fn main() {
    match loadngo_camera::list_devices() {
        Ok(devices) if devices.is_empty() => println!("(no cameras found)"),
        Ok(devices) => {
            println!(
                "ffmpeg input format: {}",
                loadngo_camera::device::input_format()
            );
            for device in devices {
                println!(
                    "  id={:<24} name={}  (-i {})",
                    device.id,
                    device.name,
                    loadngo_camera::device::input_argument(&device.id)
                );
            }
        }
        Err(err) => eprintln!("enumeration failed: {err}"),
    }
}
