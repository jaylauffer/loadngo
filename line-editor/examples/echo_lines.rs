//! Reads lines with editing and prints each back: try the keys by hand with
//! `cargo run -p loadngo-line-editor --example echo_lines`.

fn main() -> std::io::Result<()> {
    let mut editor = loadngo_line_editor::LineEditor::new();
    while let Some(line) = editor.read_line("> ")? {
        println!("[{line}]");
    }
    Ok(())
}
