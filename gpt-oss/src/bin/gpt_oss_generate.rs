//! Text continuation, one chat answer, or an interactive chat with a gpt-oss GGUF, on the
//! GPU or the CPU reference path. In chat the model has loadngo's read-only tools: the
//! local drive, the Archive CAS archives on attached drives, its own notes, and the web.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::exit,
    time::Instant,
};

use loadngo_gpt_oss::{
    chat::{
        read_call, read_reply, tool_namespace, tool_result, Conversation, Message, Reasoning, Reply,
    },
    model::Model,
    tokenizer::Tokenizer,
};
use loadngo_inference::{
    cas_tools::{cas_tools, Archives},
    memory_tools::{format_notes, MemoryStore},
    tools::{FsTools, Toolbox},
    web_tools::WebTools,
};
use loadngo_weights::gguf;

const HELP: &str = "\
gpt_oss_generate -- runs a gpt-oss GGUF greedily. With TEXT it continues the text; with
--chat and TEXT it answers TEXT as a question in the harmony chat format; with --chat
and no TEXT it chats interactively, one message per line, `/reset` to start over and
`/quit` (or Ctrl-D) to stop; on a terminal the arrow keys edit the line and Up/Down
recall earlier ones. Answers go to stdout; reasoning, tool calls and
timings to stderr.

In chat the model has loadngo's tools, as Kimi does: fs_list, fs_read, fs_find and
fs_grep on the local drive (read-only, relative paths from --base); cas_archives,
cas_list, cas_find, cas_read and cas_grep over the Archive CAS archives on attached
drives (signatures checked against --cas-key); memory_save, memory_search, memory_list
and memory_forget over its notes (--memory); web_search and web_fetch (queries leave
this machine). Nothing it can call edits files or runs commands.

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
                               16384)
  --profile        (optional)  with --gpu: where the prompt's and the reply's time went
  --base PATH      (optional)  where relative fs_* paths start (default: the current
                               directory)
  --cas-root PATH  (optional, repeatable)  an Archive CAS root beyond those found on
                               attached drives
  --cas-key PATH   (optional)  the public key archive signatures are checked against
                               (default: ~/.loadngo/keys/*.pub)
  --memory PATH    (optional)  the notes file (default ~/.loadngo/gpt-oss/memory.jsonl)
  --no-tools       (optional)  chat without tools
  --no-web         (optional)  without web_search and web_fetch
  --no-memory      (optional)  without the memory tools
  -h, --help                   this text

Example:
  cargo run --release -p loadngo-gpt-oss --bin gpt_oss_generate -- --gpu --chat \\
      --gguf ~/.loadngo/models/56fcc05caeabd1f4f352f7b9d6762cad2035b860973c07a5c330a7fa3944e8e1.gguf \\
      --base ~/pudding 'Which archives are attached, and what is in the newest one?'
";

fn fail(message: &str) -> ! {
    eprintln!("gpt_oss_generate: {message}; run with --help for the options");
    exit(2);
}

#[allow(clippy::struct_excessive_bools)]
struct Options {
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
    web: bool,
    notes: bool,
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
        context: 16384,
        profile: false,
        base: None,
        cas_roots: Vec::new(),
        cas_key: None,
        memory: None,
        tools: true,
        web: true,
        notes: true,
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
            "--no-web" => o.web = false,
            "--no-memory" => o.notes = false,
            other if other.starts_with("--") => fail(&format!("unknown option {other}")),
            other => o.text = Some(other.to_owned()),
        }
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

/// The tools and what the conversation tells the model about them and its notes.
fn toolbox(o: &Options) -> (Toolbox, String) {
    let mut tools = Toolbox::default();
    let base = o
        .base
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| ".".into());
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut instructions = format!(
        "You are running locally on Jay's Mac mini. Relative file paths start at {}. Use \
         your tools to look things up instead of guessing, and name the files and archives \
         you read. Your tools only read (and keep your notes): nothing here edits files or \
         runs commands.",
        base.display()
    );
    eprintln!(
        "file tools: local drive, read-only, relative to {}",
        base.display()
    );
    for tool in FsTools::new(&base, home.as_deref()).into_tools() {
        tools.push(tool);
    }
    // Every Archive CAS root on the attached drives, plus --cas-root; signatures are
    // checked against --cas-key, or the key kept on this Mac.
    let key = match &o.cas_key {
        Some(path) => Some(
            data::archive_cas_sign::read_public_key(path)
                .unwrap_or_else(|e| fail(&format!("CAS key {}: {e:#}", path.display()))),
        ),
        None => data::archive_cas_sign::default_trusted_key().ok().flatten(),
    };
    let archives = Archives::new(o.cas_roots.clone(), key);
    let roots = archives.roots();
    eprintln!(
        "archive tools: {} Archive CAS root(s) attached ({})",
        roots.len(),
        roots
            .iter()
            .map(|r| r.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for tool in cas_tools(archives) {
        tools.push(tool);
    }
    if o.notes {
        let path = o.memory.clone().unwrap_or_else(|| {
            home.clone()
                .unwrap_or_default()
                .join(".loadngo/gpt-oss/memory.jsonl")
        });
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let memory = MemoryStore::new(&path);
        eprintln!(
            "memory: {} (memory_save, memory_search, memory_list, memory_forget)",
            path.display()
        );
        instructions.push_str(
            "\n\nYou have notes that last across conversations: save facts, decisions and the \
             state of ongoing work with memory_save when they will matter later, look them up \
             with memory_search, and drop wrong or outdated ones with memory_forget.",
        );
        match memory.recall(4096) {
            Ok(notes) if !notes.is_empty() => {
                instructions.push_str(&format!(
                    " Your most recent notes:\n{}",
                    format_notes(&notes)
                ));
            }
            Ok(_) => instructions.push_str(" You have no notes yet."),
            Err(e) => eprintln!("memory: cannot read the notes: {e}"),
        }
        for tool in memory.into_tools() {
            tools.push(tool);
        }
    }
    if o.web {
        eprintln!("web tools: web_search (DuckDuckGo) and web_fetch; queries leave this machine");
        for tool in WebTools::new().into_tools() {
            tools.push(tool);
        }
    } else {
        eprintln!("web tools: off (--no-web)");
    }
    (tools, instructions)
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
    let mut engine = Engine::new(model, o.on_gpu, o.context);
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
    let mut chat = Chat::new(&tokenizer, &o);
    match &o.text {
        Some(text) => {
            let reply = chat.turn(&mut engine, text, limit, &o);
            eprintln!("[reasoning] {}", reply.analysis.trim());
            println!("{}", reply.answer.trim());
        }
        None => chat.interactive(&mut engine, limit, &o),
    }
}

/// Feeds `tokens` and reports the time; returns the logits after the last.
fn feed_timed(engine: &mut Engine, tokens: &[u32], o: &Options) -> Vec<f32> {
    let start = Instant::now();
    let logits = engine.feed(tokens);
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
        if stop(next) || engine.position() + 1 >= engine.context() {
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

/// A conversation with the tools.
struct Chat<'t> {
    tokenizer: &'t Tokenizer,
    conversation: Conversation,
    /// The developer instructions; each turn puts the local date and time first.
    instructions: Option<String>,
    tools: Option<Toolbox>,
    call: u32,
    stops: [u32; 2],
}

/// Longest tool result passed back, in characters; the tools bound their own output
/// well below this, and the rest of the context stays for the reply.
const MAX_RESULT_CHARS: usize = 24_000;

impl<'t> Chat<'t> {
    fn new(tokenizer: &'t Tokenizer, o: &Options) -> Self {
        let mut conversation = Conversation::new(now().date);
        conversation.reasoning = o.reasoning;
        let (tools, instructions) = if o.tools {
            let (tools, instructions) = toolbox(o);
            conversation.tools =
                Some(tool_namespace(&tools.declaration()).unwrap_or_else(|e| fail(&e)));
            (Some(tools), Some(instructions))
        } else {
            eprintln!("tools: off (--no-tools)");
            (None, None)
        };
        let control = |name| {
            tokenizer
                .control(name)
                .unwrap_or_else(|| fail(&format!("no {name} token")))
        };
        Self {
            tokenizer,
            conversation,
            instructions,
            tools,
            call: control("<|call|>"),
            stops: [control("<|return|>"), control("<|call|>")],
        }
    }

    /// One user message: the whole conversation is read again (harmony leaves earlier
    /// replies' reasoning out of the history), then the model replies, calling tools as
    /// often as it needs; each call's `<|call|>` and result are fed straight in.
    fn turn(&mut self, engine: &mut Engine, text: &str, limit: usize, o: &Options) -> Reply {
        // The local date and time, read again each turn: the system message's date, and
        // the first line of the instructions with the weekday and offset, so the model
        // has no reason to look them up.
        let now = now();
        self.conversation.date = now.date;
        self.conversation.instructions = Some(match &self.instructions {
            Some(instructions) => format!("{}\n\n{instructions}", now.said),
            None => now.said,
        });
        self.conversation
            .messages
            .push(Message::User(text.to_owned()));
        let prompt = self
            .conversation
            .prompt(self.tokenizer)
            .unwrap_or_else(|e| fail(&e.to_string()));
        engine.reset();
        let mut logits = feed_timed(engine, &prompt, o);
        let mut written = Vec::new();
        loop {
            let round = decode(engine, logits, limit, o, |t| self.stops.contains(&t));
            written.extend_from_slice(&round);
            if round.last() != Some(&self.call) {
                break;
            }
            let Some(call) = read_call(self.tokenizer, &round) else {
                eprintln!("[a tool call that could not be read; the turn stops]");
                break;
            };
            let result = match &self.tools {
                Some(tools) => tools
                    .call(&call.name, &call.arguments)
                    .unwrap_or_else(|e| format!("error: {e}")),
                None => "error: tools are off".to_owned(),
            };
            let mut result: String = result.chars().take(MAX_RESULT_CHARS).collect();
            eprintln!(
                "[tool] {} {} -> {} characters",
                call.name,
                call.arguments.trim(),
                result.chars().count()
            );
            let mut more = vec![self.call];
            more.extend(
                tool_result(self.tokenizer, &call.name, &result)
                    .unwrap_or_else(|e| fail(&e.to_string())),
            );
            if engine.position() + more.len() + 512 > engine.context() {
                result = "error: the result does not fit in what is left of the context".into();
                more.truncate(1);
                more.extend(
                    tool_result(self.tokenizer, &call.name, &result)
                        .unwrap_or_else(|e| fail(&e.to_string())),
                );
                if engine.position() + more.len() + 64 > engine.context() {
                    eprintln!("[the context is full; the turn stops]");
                    break;
                }
            }
            logits = engine.feed(&more);
        }
        let reply = read_reply(self.tokenizer, &written);
        if !reply.complete {
            eprintln!("[the reply did not finish]");
        }
        self.conversation
            .messages
            .push(Message::Assistant(reply.answer.trim().to_owned()));
        reply
    }

    fn interactive(&mut self, engine: &mut Engine, limit: usize, o: &Options) {
        eprintln!(
            "chat: one message per line (arrow keys edit, Up/Down recall); /reset starts over, \
             /quit or Ctrl-D stops"
        );
        let mut editor = loadngo_line_editor::LineEditor::new();
        loop {
            let line = match editor.read_line("> ") {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(e) => fail(&format!("reading the terminal: {e}")),
            };
            match line.trim() {
                "" => {}
                "/quit" => break,
                "/reset" => {
                    self.conversation.messages.clear();
                    eprintln!("[conversation cleared]");
                }
                text => {
                    let reply = self.turn(engine, text, limit, o);
                    if o.show_reasoning {
                        eprintln!("[reasoning] {}", reply.analysis.trim());
                    }
                    println!("{}", reply.answer.trim());
                }
            }
        }
    }
}

/// The date and time the chat tells the model.
struct Now {
    /// `YYYY-MM-DD`, local: the system message's "Current date".
    date: String,
    /// A sentence with the weekday, date, time and offset from UTC.
    said: String,
}

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// `(year, month 1-12, day)` of a count of days since 1970-01-01 (Howard Hinnant's
/// days-to-civil).
fn civil(days: i64) -> (i64, usize, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month as usize, day)
}

/// Now, in the machine's time zone. Until 2026-10-04 this was UTC: at 06:07 on Sunday 4
/// October at +07 the model was told 2026-10-03, and answered "Wednesday, October 3".
fn now() -> Now {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    // Seconds east of UTC for the local zone (0, UTC, where it cannot be read).
    let offset = local_offset(seconds);
    let local = seconds + offset;
    let days = local.div_euclid(86_400);
    let (year, month, day) = civil(days);
    let weekday = WEEKDAYS[(days + 4).rem_euclid(7) as usize];
    let minute = local.rem_euclid(86_400) / 60;
    let zone = if offset == 0 {
        "UTC".to_owned()
    } else {
        let sign = if offset < 0 { '-' } else { '+' };
        format!(
            "UTC{sign}{:02}:{:02}",
            offset.abs() / 3600,
            offset.abs() % 3600 / 60
        )
    };
    Now {
        date: format!("{year:04}-{month:02}-{day:02}"),
        said: format!(
            "It is now {weekday}, {day} {} {year}, {:02}:{:02} local time ({zone}); the \
             current date above is local too. Answer questions about the date or time from \
             this; do not look them up.",
            MONTHS[month - 1],
            minute / 60,
            minute % 60
        ),
    }
}

#[cfg(unix)]
// tm_gmtoff is a C long: 64 bits here, 32 on 32-bit targets.
#[allow(clippy::useless_conversion)]
fn local_offset(seconds: i64) -> i64 {
    let time = seconds as libc::time_t;
    // SAFETY: localtime_r writes only into the tm it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let filled = unsafe { !libc::localtime_r(&time, &mut tm).is_null() };
    if filled {
        i64::from(tm.tm_gmtoff)
    } else {
        0
    }
}

#[cfg(not(unix))]
fn local_offset(_seconds: i64) -> i64 {
    0
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
    fn new(model: Model, on_gpu: bool, context: usize) -> Self {
        if on_gpu {
            #[cfg(target_os = "macos")]
            {
                let gpu = loadngo_gpt_oss::gpu::GpuModel::new(model, 512, context)
                    .unwrap_or_else(|e| fail(&e.to_string()));
                let session = gpu.session().unwrap_or_else(|e| fail(&e.to_string()));
                return Self::Gpu(Box::new(gpu), session);
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = context;
                fail("--gpu needs macOS");
            }
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

    /// Positions fed so far.
    fn position(&self) -> usize {
        match self {
            Self::Cpu(_, session) => session.len(),
            #[cfg(target_os = "macos")]
            Self::Gpu(_, session) => session.len(),
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
