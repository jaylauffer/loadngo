//! Greedy text continuation, or one chat answer, with a gpt-oss GGUF on the CPU reference
//! path.

use std::{path::PathBuf, process::exit, time::Instant};

use loadngo_gpt_oss::{
    chat::{read_reply, Conversation, Message, Reasoning},
    model::Model,
    tokenizer::Tokenizer,
};
use loadngo_weights::gguf;

const HELP: &str = "\
gpt_oss_generate -- continues a text with a gpt-oss GGUF, greedily, on the CPU
reference forward pass, or with --chat answers it as a question in the harmony chat
format (the reasoning goes to stderr, the answer to stdout). Prints timings to stderr.

Usage:
  cargo run --release -p loadngo-gpt-oss --bin gpt_oss_generate -- [OPTIONS] TEXT

Options:
  --gguf PATH    (required)  the model file, e.g. the verified copy in ~/.loadngo/models
  --tokens N     (optional)  tokens to generate (default 8; with --chat 512)
  --chat         (optional)  treat TEXT as a user message and print the final answer
  --reasoning R  (optional)  with --chat: low, medium or high (default low)
  --gpu          (optional)  run on the GPU (macOS): weights in GPU memory, prompts in
                             passes of 512, up to 8192 positions
  -h, --help                 this text

Example:
  cargo run --release -p loadngo-gpt-oss --bin gpt_oss_generate -- \\
      --gguf ~/.loadngo/models/56fcc05caeabd1f4f352f7b9d6762cad2035b860973c07a5c330a7fa3944e8e1.gguf \\
      'The capital of France is'
  cargo run --release -p loadngo-gpt-oss --bin gpt_oss_generate -- \\
      --gguf ~/.loadngo/models/56fcc05c....gguf --chat 'What is the capital of France?'
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
    let (mut path, mut tokens, mut text) = (None, None, None);
    let (mut chat, mut reasoning, mut on_gpu) = (false, Reasoning::Low, false);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--gguf" => {
                path = Some(PathBuf::from(
                    args.next().unwrap_or_else(|| fail("--gguf needs a path")),
                ))
            }
            "--tokens" => {
                tokens = Some(
                    args.next()
                        .and_then(|n| n.parse().ok())
                        .unwrap_or_else(|| fail("--tokens needs a number")),
                );
            }
            "--chat" => chat = true,
            "--gpu" => on_gpu = true,
            "--reasoning" => {
                reasoning = args
                    .next()
                    .and_then(|r| Reasoning::parse(&r))
                    .unwrap_or_else(|| fail("--reasoning is low, medium or high"));
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
    let mut engine = Engine::new(model, on_gpu);
    eprintln!(
        "loaded in {:.1}s ({})",
        start.elapsed().as_secs_f64(),
        engine.name()
    );

    let tokens = tokens.unwrap_or(if chat { 512 } else { 8 });
    let prompt = if chat {
        let mut conversation = Conversation::new(today());
        conversation.reasoning = reasoning;
        conversation.messages.push(Message::User(text.clone()));
        conversation
            .prompt(&tokenizer)
            .unwrap_or_else(|e| fail(&e.to_string()))
    } else {
        tokenizer.encode(&text)
    };
    let stops: Vec<u32> = ["<|return|>", "<|call|>"]
        .iter()
        .filter_map(|name| tokenizer.control(name))
        .collect();
    let start = Instant::now();
    let mut logits = engine.feed(&prompt);
    eprintln!(
        "{} prompt tokens in {:.2}s ({:.0} tokens/s)",
        prompt.len(),
        start.elapsed().as_secs_f64(),
        prompt.len() as f64 / start.elapsed().as_secs_f64()
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
        if if chat {
            stops.contains(&next)
        } else {
            tokenizer.is_control(next)
        } {
            break;
        }
        logits = engine.feed(&[next]);
    }
    if chat {
        let reply = read_reply(&tokenizer, &out);
        eprintln!("[reasoning] {}", reply.analysis.trim());
        println!("{}", reply.answer.trim());
        if !reply.complete {
            eprintln!("[the reply was cut off at {tokens} tokens]");
        }
    } else {
        println!("{text}{}", tokenizer.decode(&out));
    }
    eprintln!(
        "{} tokens in {:.2}s ({:.1} tokens/s)",
        out.len(),
        start.elapsed().as_secs_f64(),
        out.len() as f64 / start.elapsed().as_secs_f64()
    );
}

/// The CPU reference or the GPU path, behind one `feed`.
enum Engine {
    Cpu(Box<Model>, loadngo_gpt_oss::model::Session),
    #[cfg(target_os = "macos")]
    Gpu(
        Box<loadngo_gpt_oss::gpu::GpuModel>,
        loadngo_gpt_oss::gpu::GpuSession,
    ),
}

impl Engine {
    fn new(model: Model, on_gpu: bool) -> Self {
        if on_gpu {
            #[cfg(target_os = "macos")]
            {
                let gpu = loadngo_gpt_oss::gpu::GpuModel::new(model, 512, 8192)
                    .unwrap_or_else(|e| fail(&e.to_string()));
                let session = gpu.session().unwrap_or_else(|e| fail(&e.to_string()));
                return Self::Gpu(Box::new(gpu), session);
            }
            #[cfg(not(target_os = "macos"))]
            fail("--gpu needs macOS");
        }
        let session = model.session();
        Self::Cpu(Box::new(model), session)
    }

    fn name(&self) -> String {
        match self {
            Self::Cpu(..) => "CPU".into(),
            #[cfg(target_os = "macos")]
            Self::Gpu(gpu, _) => format!("GPU: {}", gpu.device()),
        }
    }

    /// Feeds tokens and returns the logits after the last.
    fn feed(&mut self, tokens: &[u32]) -> Vec<f32> {
        match self {
            Self::Cpu(model, session) => {
                let mut logits = Vec::new();
                for &t in tokens {
                    logits = model.step(session, t);
                }
                logits
            }
            #[cfg(target_os = "macos")]
            Self::Gpu(gpu, session) => gpu
                .feed(session, tokens, loadngo_gpt_oss::gpu::Logits::Last)
                .unwrap_or_else(|e| fail(&e.to_string()))
                .pop()
                .expect("the last position's logits"),
        }
    }
}

/// Today's date in UTC as `YYYY-MM-DD` (days to civil date, Howard Hinnant's method).
fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}
