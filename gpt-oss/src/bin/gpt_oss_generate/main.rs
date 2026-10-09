//! Text continuation, one chat answer, or an interactive chat with a gpt-oss GGUF, on the
//! GPU or the CPU reference path. In chat the model works in Jay's workspace with
//! loadngo's tools: reading the drive, the Archive CAS archives and its notes; editing
//! text files and checking them with cargo and git; the web when nothing local will do.
//! System One (Jev) checks the work as it goes.

mod agent;
mod engine;
mod jev;

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::exit,
    time::Instant,
};

use loadngo_gpt_oss::{chat::Reasoning, model::Model, tokenizer::Tokenizer};
use loadngo_weights::gguf;

use crate::engine::{Engine, Which};

const HELP: &str = "\
gpt_oss_generate -- runs a gpt-oss GGUF greedily. With TEXT it continues the text; with
--chat and TEXT it answers TEXT as a request; with --chat and no TEXT it chats
interactively, one message per line, `/reset` to start over and `/quit` (or Ctrl-D) to
stop; on a terminal the arrow keys edit the line and Up/Down recall earlier ones.
Answers go to stdout; reasoning, tool calls, Jev's checks and timings to stderr.

In chat the model works in the workspace (--base) with loadngo's tools:
  reading:  fs_list, fs_read, fs_find, fs_grep (the local drive); cas_archives, cas_list,
            cas_find, cas_read, cas_grep (Archive CAS archives on attached drives,
            signatures checked against --cas-key)
  notes:    memory_save, memory_search, memory_list, memory_forget (--memory)
  editing:  text_read, text_edit, text_write (workspace text files, from a read
            revision; not other agents' uncommitted or claimed files, the root's rules
            and board, ignored files or build output); cargo (check, test, clippy,
            build, fmt --check) and git (status, diff, log, show). Its first write in a
            repository claims it on AGENT-BOARD.md as gpt-oss; at the end the claim
            becomes a handoff listing the uncommitted files. Nothing is committed.
  web:      web_search, web_fetch (queries leave this machine)
System One (Jev) answers typed questions with the same model in a side session: every
6 tool calls it judges whether the turn is in progress, waiting for Jay, complete or
stuck (nudging, then closing the tools when it stays stuck), and before a web call
until one is approved this turn it judges whether useful local lookup is still missing,
using the recorded tool evidence. Search receipts are kept across turns in a bounded log.
The turn loop is loadngo's shared one (inference::agent, docs/AGENT_LOOP.md): repeated
calls are not run again, and a second round of them closes the tools; a write failing
the same way twice ends the turn; an answer after unchecked changes waits for a check.
In an interactive chat /undo drops the last exchange (file changes stay), and /continue
goes on with a paused turn: one paused by its budget (--turn-minutes, --turn-tokens) or a
reply that reached --tokens. Past three quarters of --context the model writes a handoff
and the context is rebuilt from it. Chats are saved in ~/.loadngo/gpt-oss/transcripts,
with a snapshot to resume from (--resume).

Usage:
  cargo run --release -p loadngo-gpt-oss --bin gpt_oss_generate -- [OPTIONS] [TEXT]

Options:
  --gguf PATH      (required)  the model file, e.g. the verified copy in ~/.loadngo/models
  --blake3 HEX     (optional)  refuse the file unless its BLAKE3 is HEX (hashed while the
                               model loads)
  --tokens N       (optional)  tokens per reply round (default 8; with --chat 2048)
  --chat           (optional)  chat format: answer TEXT, or chat interactively without it
  --reasoning R    (optional)  with --chat: low, medium or high (default low)
  --show-reasoning (optional)  in an interactive chat, print the reasoning too
  --gpu            (optional)  run on the GPU (macOS): weights in GPU memory, prompts in
                               passes of 512
  --context N      (optional)  with --gpu: positions a conversation can reach (default
                               32768)
  --profile        (optional)  with --gpu: where the prompt's and the reply's time went
  --base PATH      (optional)  the workspace: where relative paths start and edits stay
                               (default: the current directory)
  --cas-root PATH  (optional, repeatable)  an Archive CAS root beyond those found on
                               attached drives
  --cas-key PATH   (optional)  the public key archive signatures are checked against
                               (default: ~/.loadngo/keys/*.pub)
  --memory PATH    (optional)  the notes file (default ~/.loadngo/gpt-oss/memory.jsonl)
  --no-tools       (optional)  chat without tools
  --no-edit        (optional)  without the editing, cargo and git tools
  --no-web         (optional)  without web_search and web_fetch
  --no-memory      (optional)  without the memory tools
  --no-jev         (optional)  without Jev's checkpoints and web gate
  --turn-minutes N (optional)  pause a turn after N minutes (default: no limit)
  --turn-tokens N  (optional)  pause a turn after N generated tokens (default: no limit)
  --resume WHICH   (optional)  with --chat: carry on a saved chat, `latest` or the path of
                               its .jsonl or .state.json
  -h, --help                   this text

Example:
  cargo run --release -p loadngo-gpt-oss --bin gpt_oss_generate -- --gpu --chat \\
      --gguf ~/.loadngo/models/56fcc05caeabd1f4f352f7b9d6762cad2035b860973c07a5c330a7fa3944e8e1.gguf \\
      --base ~/pudding 'The line-editor test for word moves is missing a case; add it and run the tests.'
";

fn fail(message: &str) -> ! {
    eprintln!("gpt_oss_generate: {message}; run with --help for the options");
    exit(2);
}

#[allow(clippy::struct_excessive_bools)]
pub struct Options {
    path: PathBuf,
    blake3: Option<String>,
    tokens: Option<usize>,
    text: Option<String>,
    chat: bool,
    reasoning: Reasoning,
    show_reasoning: bool,
    on_gpu: bool,
    context: usize,
    profile: bool,
    base: Option<PathBuf>,
    cas_roots: Vec<PathBuf>,
    cas_key: Option<PathBuf>,
    memory: Option<PathBuf>,
    tools: bool,
    edit: bool,
    web: bool,
    notes: bool,
    jev: bool,
    budget: loadngo_inference::agent::Budget,
    resume: Option<String>,
}

fn options() -> Options {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        exit(0);
    }
    let mut o = Options {
        path: PathBuf::new(),
        blake3: None,
        tokens: None,
        text: None,
        chat: false,
        reasoning: Reasoning::Low,
        show_reasoning: false,
        on_gpu: false,
        context: 32768,
        profile: false,
        base: None,
        cas_roots: Vec::new(),
        cas_key: None,
        memory: None,
        tools: true,
        edit: true,
        web: true,
        notes: true,
        jev: true,
        budget: loadngo_inference::agent::Budget::default(),
        resume: None,
    };
    let mut path = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |what: &str| {
            args.next()
                .unwrap_or_else(|| fail(&format!("{arg} needs {what}")))
        };
        let number = |text: String, name: &str| -> usize {
            text.parse()
                .unwrap_or_else(|_| fail(&format!("{name} needs a number")))
        };
        match arg.as_str() {
            "--gguf" => path = Some(PathBuf::from(value("a path"))),
            "--blake3" => o.blake3 = Some(value("a hash").to_ascii_lowercase()),
            "--tokens" => o.tokens = Some(number(value("a number"), "--tokens")),
            "--context" => o.context = number(value("a number"), "--context"),
            "--reasoning" => {
                o.reasoning = Reasoning::parse(&value("low, medium or high"))
                    .unwrap_or_else(|| fail("--reasoning is low, medium or high"));
            }
            "--base" => o.base = Some(PathBuf::from(value("a path"))),
            "--cas-root" => o.cas_roots.push(PathBuf::from(value("a path"))),
            "--cas-key" => o.cas_key = Some(PathBuf::from(value("a path"))),
            "--memory" => o.memory = Some(PathBuf::from(value("a path"))),
            "--chat" => o.chat = true,
            "--show-reasoning" => o.show_reasoning = true,
            "--gpu" => o.on_gpu = true,
            "--profile" => o.profile = true,
            "--no-tools" => o.tools = false,
            "--no-edit" => o.edit = false,
            "--no-web" => o.web = false,
            "--no-memory" => o.notes = false,
            "--no-jev" => o.jev = false,
            "--turn-minutes" => {
                let minutes = number(value("a number"), "--turn-minutes");
                o.budget.time =
                    (minutes > 0).then(|| std::time::Duration::from_secs(60 * minutes as u64));
            }
            "--turn-tokens" => {
                let tokens = number(value("a number"), "--turn-tokens");
                o.budget.tokens = (tokens > 0).then_some(tokens);
            }
            "--resume" => o.resume = Some(value("latest or a path")),
            other if other.starts_with("--") => fail(&format!("unknown option {other}")),
            other => o.text = Some(other.to_owned()),
        }
    }
    if o.resume.is_some() && (!o.chat || o.text.is_some()) {
        fail("--resume carries on an interactive chat: --chat without TEXT");
    }
    if o.text.is_none() && !o.chat {
        fail("no text to continue (or pass --chat for a conversation)");
    }
    o.path = path.unwrap_or_else(|| fail("--gguf is required"));
    o
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
    let mut engine = Engine::new(model, o.on_gpu, o.context).unwrap_or_else(|e| fail(&e));
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
    let limit = o.tokens.unwrap_or(if o.chat { 2048 } else { 8 });

    if !o.chat {
        let text = o.text.as_deref().expect("checked in options()");
        let prompt = tokenizer.encode(text);
        let logits = feed_timed(&mut engine, &prompt, &o);
        let out = decode(&mut engine, logits, limit, &o, |t| tokenizer.is_control(t));
        println!("{text}{}", tokenizer.decode(&out));
        return;
    }
    let (mut chat, saved) = agent::chat(&tokenizer, &o);
    let mut backend = agent::GptOss::new(engine, &tokenizer, o.profile);
    if let Some(saved) = saved {
        chat.restore(&saved, &mut backend)
            .unwrap_or_else(|e| fail(&e));
        eprintln!(
            "resumed: {} exchanges, {} context tokens{}",
            chat.history().len(),
            loadngo_inference::agent::Backend::position(&backend),
            if chat.pending().is_some() {
                "; a paused turn waits for /continue"
            } else {
                ""
            }
        );
    }
    match &o.text {
        Some(text) => agent::turn(&mut chat, &mut backend, text, limit, true),
        None => agent::interactive(&mut chat, &mut backend, limit, &o),
    }
    agent::finish(&mut chat);
}

/// Feeds `tokens` to the conversation and reports the time; returns the logits after
/// the last.
fn feed_timed(engine: &mut Engine, tokens: &[u32], o: &Options) -> Vec<f32> {
    let start = Instant::now();
    let logits = engine
        .feed(Which::Main, tokens)
        .unwrap_or_else(|e| fail(&e));
    eprintln!(
        "{} prompt tokens in {:.2}s ({:.0} tokens/s)",
        tokens.len(),
        start.elapsed().as_secs_f64(),
        tokens.len() as f64 / start.elapsed().as_secs_f64()
    );
    if o.profile {
        eprintln!("[prompt] {}", engine.profile());
    }
    logits
}

/// Generates greedily from `logits` until `stop` (included, not fed) or `limit` tokens.
fn decode(
    engine: &mut Engine,
    mut logits: Vec<f32>,
    limit: usize,
    o: &Options,
    stop: impl Fn(u32) -> bool,
) -> Vec<u32> {
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
        if stop(next) || engine.position(Which::Main) + 1 >= engine.context() {
            break;
        }
        logits = engine
            .feed(Which::Main, &[next])
            .unwrap_or_else(|e| fail(&e));
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
