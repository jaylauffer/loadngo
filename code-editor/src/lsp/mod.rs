//! The language server client: rust-analyzer over LSP. See
//! `docs/CODE_EDITOR.md` (M4).

pub mod session;
pub mod transport;
pub mod uri;

pub use session::{CompletionItem, Location, LspEvent, LspSession, Position, Range, TextEdit};
pub use transport::LspProcess;

/// Against the real rust-analyzer, when it is installed (CI's toolchain has
/// no rust-analyzer component, so there it is skipped).
#[cfg(test)]
mod live_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn rust_analyzer() -> Option<std::path::PathBuf> {
        let program = transport::find_program("rust-analyzer");
        let works = std::process::Command::new(&program)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        works.then_some(program)
    }

    #[test]
    fn rust_analyzer_reports_a_syntax_error_and_answers_hover() {
        let Some(program) = rust_analyzer() else {
            eprintln!("rust-analyzer is not installed; skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"live\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
        )
        .unwrap();
        let file = dir.path().join("src/main.rs");
        // rust-analyzer reports syntax errors itself; a name it cannot
        // resolve is cargo check's to report, which the editor runs.
        let text = concat!(
            "fn main() {\n",
            "    let total = 1 + 2;\n",
            "    println!(\"{total}\");\n",
            "}\n",
            "\n",
            "fn broken() {\n",
            "    let x = ;\n",
            "}\n",
        );
        std::fs::write(&file, text).unwrap();

        let (woke, wakes) = mpsc::channel();
        let process = LspProcess::start(&program, dir.path(), move || {
            let _ = woke.send(());
        })
        .unwrap();
        let mut session = LspSession::new(dir.path());
        session.open_document(&file, text, 1);
        let trace = std::env::var_os("LSP_TRACE").is_some();
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut hover = None;
        let mut hover_text = None;
        loop {
            for message in session.take_outgoing() {
                process.send(&message);
            }
            for message in process.drain() {
                if trace {
                    let text = message.to_string();
                    eprintln!("<- {}", &text[..text.len().min(400)]);
                }
                session.handle(message);
            }
            for event in session.take_events() {
                if let LspEvent::Hover { text, .. } = event {
                    hover_text = Some(text);
                }
            }
            let has_error = !session.diagnostics(&file).is_empty();
            if session.ready && hover.is_none() {
                // `total`, line 2 (zero-based 1).
                hover = session.hover(
                    &file,
                    Position {
                        line: 1,
                        character: 9,
                    },
                );
            }
            if has_error && hover_text.is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no answer in time; status {}, ended {:?}",
                session.status,
                process.ended()
            );
            let _ = wakes.recv_timeout(Duration::from_millis(500));
        }
        assert!(session.utf32(), "rust-analyzer speaks UTF-32 positions");
        let error = session.diagnostics(&file)[0].clone();
        assert_eq!(error.level, crate::cargo_check::Level::Error);
        assert_eq!(error.line, 7, "{error:?}");
        let hover_text = hover_text.unwrap();
        assert!(hover_text.contains("i32"), "{hover_text}");
        session.shutdown();
        for message in session.take_outgoing() {
            process.send(&message);
        }
    }
}
