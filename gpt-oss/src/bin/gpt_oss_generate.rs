//! Text continuation, one chat answer, or an interactive chat with a gpt-oss GGUF, on the
//! GPU or the CPU reference path.

use std::{
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    process::exit,
    time::Instant,
};

use loadngo_gpt_oss::{
    chat::{read_reply, Conversation, Message, Reasoning},
    model::Model,
    tokenizer::Tokenizer,
};
use loadngo_weights::gguf;

const HELP: &str = "\
gpt_oss_generate -- runs a gpt-oss GGUF greedily. With TEXT it continues the text; with
--chat and TEXT it answers TEXT as a question in the harmony chat format (the reasoning
goes to stderr, the answer to stdout); with --chat and no TEXT it chats interactively,
one message per line, `/reset` to start over and `/quit` (or end of input) to stop.
Timings go to stderr.

Usage:
  cargo run --release -p loadngo-gpt-oss --bin gpt_oss_generate -- [OPTIONS] [TEXT]

Options:
  --gguf PATH     (required)  the model file, e.g. the verified copy in ~/.loadngo/models
  --blake3 HEX    (optional)  refuse the file unless its BLAKE3 is HEX (hashed while the
                              model loads)
  --tokens N      (optional)  tokens to generate (default 8; with --chat 2048)
  --chat          (optional)  chat format: answer TEXT, or chat interactively without it
  --reasoning R   (optional)  with --chat: low, medium or high (default low)
  --show-reasoning (optional) in an interactive chat, print the reasoning too
  --gpu           (optional)  run on the GPU (macOS): weights in GPU memory, prompts in
                              passes of 512, up to 8192 positions
  --profile       (optional)  with --gpu: where the prompt's and the reply's time went
  -h, --help                  this text

Example:
  cargo run --release -p loadngo-gpt-oss --bin gpt_oss_generate -- --gpu --chat \\
      --gguf ~/.loadngo/models/56fcc05caeabd1f4f352f7b9d6762cad2035b860973c07a5c330a7fa3944e8e1.gguf \\
      'What is the capital of France?'
";

fn fail(message: &str) -> ! {
    eprintln!("gpt_oss_generate: {message}; run with --help for the options");
    exit(2);
}

struct Options {
    path: PathBuf,
    blake3: Option<String>,
    tokens: Option<usize>,
    text: Option<String>,
    chat: bool,
    reasoning: Reasoning,
    show_reasoning: bool,
    on_gpu: bool,
    profile: bool,
}

fn options() -> Options {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        exit(0);
    }
    let (mut path, mut blake3, mut tokens, mut text) = (None, None, None, None);
    let (mut chat, mut reasoning, mut show_reasoning, mut on_gpu, mut profile) =
        (false, Reasoning::Low, false, false, false);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |what: &str| {
            args.next()
                .unwrap_or_else(|| fail(&format!("{arg} needs {what}")))
        };
        match arg.as_str() {
            "--gguf" => path = Some(PathBuf::from(value("a path"))),
            "--blake3" => blake3 = Some(value("a hash").to_ascii_lowercase()),
            "--tokens" => {
                tokens = Some(
                    value("a number")
                        .parse()
                        .unwrap_or_else(|_| fail("--tokens needs a number")),
                );
            }
            "--reasoning" => {
                reasoning = Reasoning::parse(&value("low, medium or high"))
                    .unwrap_or_else(|| fail("--reasoning is low, medium or high"));
            }
            "--chat" => chat = true,
            "--show-reasoning" => show_reasoning = true,
            "--gpu" => on_gpu = true,
            "--profile" => profile = true,
            other if other.starts_with("--") => fail(&format!("unknown option {other}")),
            other => text = Some(other.to_owned()),
        }
    }
    if text.is_none() && !chat {
        fail("no text to continue (or pass --chat for a conversation)");
    }
    Options {
        path: path.unwrap_or_else(|| fail("--gguf is required")),
        blake3,
        tokens,
        text,
        chat,
        reasoning,
        show_reasoning,
        on_gpu,
        profile,
    }
}

/// The file's BLAKE3, as lowercase hex.
fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 4 << 20];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buffer[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn main() {
    let o = options();
    let start = Instant::now();
    // The hash runs beside the load; nothing is generated until it matches.
    let (tokenizer, model) = std::thread::scope(|scope| {
        let check = o
            .blake3
            .as_ref()
            .map(|_| scope.spawn(|| hash_file(&o.path)));
        let (file, _) = gguf::open(&o.path).unwrap_or_else(|e| fail(&e.to_string()));
        let tokenizer = Tokenizer::from_gguf(&file).unwrap_or_else(|e| fail(&e.to_string()));
        let model = Model::load(&o.path).unwrap_or_else(|e| fail(&e.to_string()));
        if let (Some(check), Some(want)) = (check, &o.blake3) {
            let got = check
                .join()
                .expect("hash thread")
                .unwrap_or_else(|e| fail(&format!("cannot hash {}: {e}", o.path.display())));
            if &got != want {
                fail(&format!(
                    "{} hashes to {got}, not {want}; refusing to run it",
                    o.path.display()
                ));
            }
        }
        (tokenizer, model)
    });
    let mut engine = Engine::new(model, o.on_gpu);
    eprintln!(
        "loaded in {:.1}s ({}{})",
        start.elapsed().as_secs_f64(),
        engine.name(),
        if o.blake3.is_some() {
            ", BLAKE3 verified"
        } else {
            ""
        }
    );
    let stops: Vec<u32> = ["<|return|>", "<|call|>"]
        .iter()
        .filter_map(|name| tokenizer.control(name))
        .collect();
    let limit = o.tokens.unwrap_or(if o.chat { 2048 } else { 8 });

    match (&o.text, o.chat) {
        (Some(text), false) => {
            let prompt = tokenizer.encode(text);
            let out = generate(&mut engine, &prompt, limit, &o, |t| tokenizer.is_control(t));
            println!("{text}{}", tokenizer.decode(&out));
        }
        (Some(text), true) => {
            let mut conversation = Conversation::new(today());
            conversation.reasoning = o.reasoning;
            conversation.messages.push(Message::User(text.clone()));
            let prompt = conversation
                .prompt(&tokenizer)
                .unwrap_or_else(|e| fail(&e.to_string()));
            let out = generate(&mut engine, &prompt, limit, &o, |t| stops.contains(&t));
            let reply = read_reply(&tokenizer, &out);
            eprintln!("[reasoning] {}", reply.analysis.trim());
            println!("{}", reply.answer.trim());
            if !reply.complete {
                eprintln!("[the reply was cut off at {limit} tokens]");
            }
        }
        (None, _) => interactive(&mut engine, &tokenizer, &stops, limit, &o),
    }
}

/// Feeds `prompt` and generates greedily until `stop` or `limit` tokens.
fn generate(
    engine: &mut Engine,
    prompt: &[u32],
    limit: usize,
    o: &Options,
    stop: impl Fn(u32) -> bool,
) -> Vec<u32> {
    let start = Instant::now();
    let mut logits = engine.feed(prompt);
    eprintln!(
        "{} prompt tokens in {:.2}s ({:.0} tokens/s)",
        prompt.len(),
        start.elapsed().as_secs_f64(),
        prompt.len() as f64 / start.elapsed().as_secs_f64()
    );
    if o.profile {
        eprintln!("[prompt] {}", engine.profile());
    }
    let start = Instant::now();
    let mut out = Vec::new();
    for _ in 0..limit {
        let next = logits
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i as u32)
            .expect("logits");
        out.push(next);
        if stop(next) {
            break;
        }
        logits = engine.feed(&[next]);
    }
    eprintln!(
        "{} tokens in {:.2}s ({:.1} tokens/s)",
        out.len(),
        start.elapsed().as_secs_f64(),
        out.len() as f64 / start.elapsed().as_secs_f64()
    );
    if o.profile {
        eprintln!("[reply] {}", engine.profile());
    }
    out
}

/// A conversation on stdin. Each turn re-reads the whole conversation: harmony leaves
/// earlier replies' reasoning out of the history, so the cached positions would not
/// match it.
fn interactive(
    engine: &mut Engine,
    tokenizer: &Tokenizer,
    stops: &[u32],
    limit: usize,
    o: &Options,
) {
    let mut conversation = Conversation::new(today());
    conversation.reasoning = o.reasoning;
    eprintln!("chat: one message per line; /reset starts over, /quit stops");
    let stdin = std::io::stdin();
    loop {
        eprint!("> ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = line.trim();
        match line {
            "" => continue,
            "/quit" => break,
            "/reset" => {
                conversation.messages.clear();
                eprintln!("[conversation cleared]");
                continue;
            }
            _ => {}
        }
        conversation.messages.push(Message::User(line.to_owned()));
        let prompt = conversation
            .prompt(tokenizer)
            .unwrap_or_else(|e| fail(&e.to_string()));
        if prompt.len() + limit > engine.context() {
            eprintln!("[the conversation is too long for the context; /reset to start over]");
            conversation.messages.pop();
            continue;
        }
        engine.reset();
        let out = generate(engine, &prompt, limit, o, |t| stops.contains(&t));
        let reply = read_reply(tokenizer, &out);
        if o.show_reasoning {
            eprintln!("[reasoning] {}", reply.analysis.trim());
        }
        println!("{}", reply.answer.trim());
        if !reply.complete {
            eprintln!("[the reply was cut off at {limit} tokens]");
        }
        conversation
            .messages
            .push(Message::Assistant(reply.answer.trim().to_owned()));
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

    /// The positions a conversation can reach.
    fn context(&self) -> usize {
        match self {
            Self::Cpu(model, _) => model.config.context_length,
            #[cfg(target_os = "macos")]
            Self::Gpu(gpu, _) => gpu.max_context,
        }
    }

    /// Starts the conversation over.
    fn reset(&mut self) {
        match self {
            Self::Cpu(model, session) => *session = model.session(),
            #[cfg(target_os = "macos")]
            Self::Gpu(_, session) => session.reset(),
        }
    }

    /// The GPU profile since the last call (nothing on the CPU path).
    fn profile(&self) -> String {
        match self {
            Self::Cpu(..) => "no profile on the CPU path".into(),
            #[cfg(target_os = "macos")]
            Self::Gpu(gpu, _) => gpu.take_profile().to_string(),
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
