fn main() {
    // Embed legacy toolbar bitmaps so Win32 can load them by resource ID.
    embed_resource::compile("resources/task.rc", std::iter::empty::<&str>());
}
