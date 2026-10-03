//! Greedy text continuation with a gpt-oss GGUF on the CPU reference path.

use std::{path::PathBuf, process::exit, time::Instant};

use loadngo_gpt_oss::{model::Model, tokenizer::Tokenizer};
use loadngo_weights::gguf;

const HELP: &str = "\
gpt_oss_generate -- continues a text with a gpt-oss GGUF, greedily, on the CPU
reference forward pass (no chat format). Prints the continuation and timings.

Usage:
  cargo run --release -p loadngo-gpt-oss --bin gpt_oss_generate -- [OPTIONS] TEXT

Options:
  --gguf PATH    (required)  the model file, e.g. the verified copy in ~/.loadngo/models
  --tokens N     (optional)  tokens to generate (default 8)
  -h, --help                 this text

Example:
  cargo run --release -p loadngo-gpt-oss --bin gpt_oss_generate -- \\
      --gguf ~/.loadngo/models/56fcc05caeabd1f4f352f7b9d6762cad2035b860973c07a5c330a7fa3944e8e1.gguf \\
      'The capital of France is'
";

fn fail(message: &str) -> ! {
    eprintln!("gpt_oss_generate: {message}; run with --help for the options");
    exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return;
    }
    let (mut path, mut tokens, mut text) = (None, 8_usize, None);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--gguf" => {
                path = Some(PathBuf::from(
                    args.next().unwrap_or_else(|| fail("--gguf needs a path")),
                ))
            }
            "--tokens" => {
                tokens = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or_else(|| fail("--tokens needs a number"));
            }
            other if other.starts_with("--") => fail(&format!("unknown option {other}")),
            other => text = Some(other.to_owned()),
        }
    }
    let path = path.unwrap_or_else(|| fail("--gguf is required"));
    let text = text.unwrap_or_else(|| fail("no text to continue"));

    let start = Instant::now();
    let (file, _) = gguf::open(&path).unwrap_or_else(|e| fail(&e.to_string()));
    let tokenizer = Tokenizer::from_gguf(&file).unwrap_or_else(|e| fail(&e.to_string()));
    let model = Model::load(&path).unwrap_or_else(|e| fail(&e.to_string()));
    eprintln!("loaded in {:.1}s", start.elapsed().as_secs_f64());

    let prompt = tokenizer.encode(&text);
    let mut session = model.session();
    let start = Instant::now();
    let mut logits = Vec::new();
    for &id in &prompt {
        logits = model.step(&mut session, id);
    }
    eprintln!(
        "{} prompt tokens in {:.1}s",
        prompt.len(),
        start.elapsed().as_secs_f64()
    );
    let start = Instant::now();
    let mut out = Vec::new();
    for _ in 0..tokens {
        let next = logits
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i as u32)
            .expect("logits");
        out.push(next);
        if tokenizer.is_control(next) {
            break;
        }
        logits = model.step(&mut session, next);
    }
    println!("{text}{}", tokenizer.decode(&out));
    eprintln!(
        "{} tokens in {:.1}s ({:.2} s/token): {out:?}",
        out.len(),
        start.elapsed().as_secs_f64(),
        start.elapsed().as_secs_f64() / out.len().max(1) as f64
    );
}
