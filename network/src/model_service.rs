use anyhow::{anyhow, bail, Context, Result};
use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const DEFAULT_LOG_TAIL_LINES: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendMode {
    Auto,
    Metal,
    Cpu,
}

impl BackendMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "metal" => Ok(Self::Metal),
            "cpu" => Ok(Self::Cpu),
            other => bail!("unsupported backend mode: {other}; expected auto, metal, or cpu"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Metal => "metal",
            Self::Cpu => "cpu",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelServerConfig {
    pub llama_server: PathBuf,
    pub model_path: PathBuf,
    pub host: String,
    pub port: u16,
    pub backend: BackendMode,
    pub ctx_size: u32,
    pub threads: Option<u32>,
    pub startup_timeout: Duration,
    pub health_path: String,
    pub extra_args: Vec<String>,
}

impl Default for ModelServerConfig {
    fn default() -> Self {
        Self {
            llama_server: PathBuf::from("llama-server"),
            model_path: PathBuf::from("/Users/jay/Downloads/gpt-oss-20b-mxfp4.gguf"),
            host: "127.0.0.1".to_string(),
            port: 8787,
            backend: BackendMode::Auto,
            ctx_size: 4096,
            threads: None,
            startup_timeout: Duration::from_secs(90),
            health_path: "/health".to_string(),
            extra_args: Vec::new(),
        }
    }
}

impl ModelServerConfig {
    pub fn validate(&self) -> Result<()> {
        if !self.model_path.exists() {
            bail!("model path does not exist: {}", self.model_path.display());
        }
        if !self.model_path.is_file() {
            bail!("model path is not a file: {}", self.model_path.display());
        }
        if self.health_path.is_empty() || !self.health_path.starts_with('/') {
            bail!("health path must start with '/': {}", self.health_path);
        }
        if self.startup_timeout.is_zero() {
            bail!("startup timeout must be greater than zero");
        }
        Ok(())
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub fn attempted_backends(&self) -> Vec<BackendMode> {
        match self.backend {
            BackendMode::Auto => vec![BackendMode::Metal, BackendMode::Cpu],
            BackendMode::Metal => vec![BackendMode::Metal],
            BackendMode::Cpu => vec![BackendMode::Cpu],
        }
    }

    pub fn llama_args_for_backend(&self, backend: BackendMode) -> Vec<String> {
        let mut args = vec![
            "-m".to_string(),
            self.model_path.display().to_string(),
            "--host".to_string(),
            self.host.clone(),
            "--port".to_string(),
            self.port.to_string(),
            "--ctx-size".to_string(),
            self.ctx_size.to_string(),
            "--parallel".to_string(),
            "1".to_string(),
            "--timeout".to_string(),
            "120".to_string(),
        ];

        if let Some(threads) = self.threads {
            args.push("--threads".to_string());
            args.push(threads.to_string());
        }

        match backend {
            BackendMode::Auto => {}
            BackendMode::Metal => {
                args.push("--n-gpu-layers".to_string());
                args.push("all".to_string());
            }
            BackendMode::Cpu => {
                args.push("--device".to_string());
                args.push("none".to_string());
                args.push("--n-gpu-layers".to_string());
                args.push("0".to_string());
            }
        }

        args.extend(self.extra_args.iter().cloned());
        args
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupFailureKind {
    MetalBackendUnavailable,
    UnsupportedModel,
    ResourceExhausted,
    ProcessExited,
    StartupTimeout,
}

impl StartupFailureKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::MetalBackendUnavailable => "metal-backend-unavailable",
            Self::UnsupportedModel => "unsupported-model",
            Self::ResourceExhausted => "resource-exhausted",
            Self::ProcessExited => "process-exited",
            Self::StartupTimeout => "startup-timeout",
        }
    }
}

#[derive(Debug)]
pub struct StartupFailure {
    pub backend: BackendMode,
    pub kind: StartupFailureKind,
    pub message: String,
    pub log_tail: String,
}

impl std::fmt::Display for StartupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "model service startup failed for backend={} kind={}: {}",
            self.backend.label(),
            self.kind.label(),
            self.message
        )?;
        if !self.log_tail.trim().is_empty() {
            write!(f, "\nlog tail:\n{}", self.log_tail)?;
        }
        Ok(())
    }
}

impl std::error::Error for StartupFailure {}

pub struct RunningModelServer {
    child: Child,
    backend: BackendMode,
    endpoint: String,
    logs: CapturedLogs,
}

impl RunningModelServer {
    pub fn backend(&self) -> BackendMode {
        self.backend
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn log_tail(&self) -> String {
        self.logs.joined()
    }

    pub fn wait(mut self) -> Result<ExitStatus> {
        self.child
            .wait()
            .context("failed to wait for llama-server process")
    }
}

impl Drop for RunningModelServer {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[derive(Clone)]
struct CapturedLogs {
    lines: Arc<Mutex<VecDeque<String>>>,
    max_lines: usize,
}

impl CapturedLogs {
    fn new(max_lines: usize) -> Self {
        Self {
            lines: Arc::new(Mutex::new(VecDeque::new())),
            max_lines,
        }
    }

    fn push(&self, line: impl Into<String>) {
        let mut lines = self.lines.lock().expect("log capture lock poisoned");
        lines.push_back(line.into());
        while lines.len() > self.max_lines {
            lines.pop_front();
        }
    }

    fn joined(&self) -> String {
        let lines = self.lines.lock().expect("log capture lock poisoned");
        lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

pub fn start_model_server(config: &ModelServerConfig) -> Result<RunningModelServer> {
    config.validate()?;
    if health_probe(&config.host, config.port, &config.health_path).is_ok() {
        bail!(
            "model service endpoint already reports healthy at {}; stop the existing service or choose another port",
            config.endpoint()
        );
    }

    let mut last_failure = None;
    for backend in config.attempted_backends() {
        match start_backend(config, backend) {
            Ok(server) => return Ok(server),
            Err(failure) => {
                let should_retry = config.backend == BackendMode::Auto
                    && backend == BackendMode::Metal
                    && should_retry_cpu(failure.kind);
                if should_retry {
                    eprintln!(
                        "zhoenus_head_model_retry backend=metal fallback=cpu kind={}",
                        failure.kind.label()
                    );
                    last_failure = Some(failure);
                    continue;
                }
                return Err(failure.into());
            }
        }
    }

    Err(last_failure
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow!("no model backend attempted")))
}

fn start_backend(
    config: &ModelServerConfig,
    backend: BackendMode,
) -> std::result::Result<RunningModelServer, StartupFailure> {
    let args = config.llama_args_for_backend(backend);
    let mut child = Command::new(&config.llama_server)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| StartupFailure {
            backend,
            kind: StartupFailureKind::ProcessExited,
            message: format!("failed to spawn {}: {err}", config.llama_server.display()),
            log_tail: String::new(),
        })?;

    let logs = CapturedLogs::new(DEFAULT_LOG_TAIL_LINES);
    if let Some(stdout) = child.stdout.take() {
        spawn_log_reader(stdout, "stdout", logs.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_reader(stderr, "stderr", logs.clone());
    }

    let deadline = Instant::now() + config.startup_timeout;
    loop {
        if health_probe(&config.host, config.port, &config.health_path).is_ok() {
            return Ok(RunningModelServer {
                child,
                backend,
                endpoint: config.endpoint(),
                logs,
            });
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                let log_tail = logs.joined();
                let kind = classify_startup_failure(&log_tail)
                    .unwrap_or(StartupFailureKind::ProcessExited);
                return Err(StartupFailure {
                    backend,
                    kind,
                    message: format!("llama-server exited before readiness: {status}"),
                    log_tail,
                });
            }
            Ok(None) => {}
            Err(err) => {
                let log_tail = logs.joined();
                return Err(StartupFailure {
                    backend,
                    kind: StartupFailureKind::ProcessExited,
                    message: format!("failed to inspect llama-server status: {err}"),
                    log_tail,
                });
            }
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let log_tail = logs.joined();
            let kind =
                classify_startup_failure(&log_tail).unwrap_or(StartupFailureKind::StartupTimeout);
            return Err(StartupFailure {
                backend,
                kind,
                message: format!(
                    "llama-server did not become ready within {}s",
                    config.startup_timeout.as_secs()
                ),
                log_tail,
            });
        }

        thread::sleep(Duration::from_millis(250));
    }
}

fn spawn_log_reader<R>(reader: R, stream_name: &'static str, logs: CapturedLogs)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim_end().to_string();
                    eprintln!("llama-server[{stream_name}] {trimmed}");
                    logs.push(format!("{stream_name}: {trimmed}"));
                }
                Err(err) => {
                    logs.push(format!("{stream_name}: failed to read process log: {err}"));
                    break;
                }
            }
        }
    });
}

pub fn classify_startup_failure(log_tail: &str) -> Option<StartupFailureKind> {
    let lower = log_tail.to_ascii_lowercase();
    if lower.contains("failed to create command queue")
        || lower.contains("ggml_metal")
        || lower.contains("ggml_backend_metal_device_init_backend")
        || (lower.contains("failed to allocate context") && lower.contains("metal"))
    {
        return Some(StartupFailureKind::MetalBackendUnavailable);
    }
    if lower.contains("unknown tokenizer")
        || lower.contains("unsupported model")
        || lower.contains("unknown model")
    {
        return Some(StartupFailureKind::UnsupportedModel);
    }
    if lower.contains("out of memory")
        || lower.contains("not enough memory")
        || lower.contains("cannot allocate memory")
    {
        return Some(StartupFailureKind::ResourceExhausted);
    }
    None
}

fn should_retry_cpu(kind: StartupFailureKind) -> bool {
    matches!(kind, StartupFailureKind::MetalBackendUnavailable)
}

fn health_probe(host: &str, port: u16, path: &str) -> Result<()> {
    let connect_host = match host {
        "0.0.0.0" => "127.0.0.1",
        "::" => "::1",
        other => other,
    };
    let mut addrs = (connect_host, port)
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve model service host: {connect_host}"))?;
    let addr = addrs
        .next()
        .ok_or_else(|| anyhow!("no socket address for model service host: {connect_host}"))?;
    http_get_status(addr, host, path).and_then(|status| {
        if (200..400).contains(&status) {
            Ok(())
        } else {
            bail!("model service health returned HTTP {status}")
        }
    })
}

fn http_get_status(addr: SocketAddr, host_header: &str, path: &str) -> Result<u16> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(200))
        .with_context(|| format!("model service is not accepting connections at {addr}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .context("failed to set model service read timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_millis(500)))
        .context("failed to set model service write timeout")?;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .context("failed to send model service health request")?;

    let mut response = [0u8; 128];
    let n = stream
        .read(&mut response)
        .context("failed to read model service health response")?;
    let head = std::str::from_utf8(&response[..n]).context("health response was not UTF-8")?;
    parse_http_status(head).ok_or_else(|| anyhow!("invalid health response: {head:?}"))
}

fn parse_http_status(head: &str) -> Option<u16> {
    let mut parts = head.lines().next()?.split_whitespace();
    let _http = parts.next()?;
    parts.next()?.parse().ok()
}

pub fn describe_command(config: &ModelServerConfig, backend: BackendMode) -> String {
    let mut parts = vec![shell_quote(&config.llama_server)];
    parts.extend(
        config
            .llama_args_for_backend(backend)
            .iter()
            .map(|arg| shell_quote(Path::new(arg))),
    );
    parts.join(" ")
}

fn shell_quote(path: &Path) -> String {
    let text = path.display().to_string();
    if text
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
    {
        return text;
    }
    format!("'{}'", text.replace('\'', "'\\''"))
}

pub fn model_file_size(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path)
        .with_context(|| format!("failed to stat model file: {}", path.display()))?
        .len())
}

#[cfg(test)]
mod tests {
    use super::{
        classify_startup_failure, describe_command, parse_http_status, BackendMode,
        ModelServerConfig, StartupFailureKind,
    };
    use std::{path::PathBuf, time::Duration};

    #[test]
    fn backend_mode_parse_accepts_expected_values() {
        assert_eq!(BackendMode::parse("auto").unwrap(), BackendMode::Auto);
        assert_eq!(BackendMode::parse("metal").unwrap(), BackendMode::Metal);
        assert_eq!(BackendMode::parse("cpu").unwrap(), BackendMode::Cpu);
        assert!(BackendMode::parse("cuda").is_err());
    }

    #[test]
    fn cpu_backend_disables_device_offload() {
        let config = ModelServerConfig {
            model_path: PathBuf::from("/tmp/model.gguf"),
            ..ModelServerConfig::default()
        };
        let args = config.llama_args_for_backend(BackendMode::Cpu);
        assert!(args.windows(2).any(|pair| pair == ["--device", "none"]));
        assert!(args.windows(2).any(|pair| pair == ["--n-gpu-layers", "0"]));
    }

    #[test]
    fn metal_backend_requests_gpu_layers() {
        let config = ModelServerConfig {
            model_path: PathBuf::from("/tmp/model.gguf"),
            ..ModelServerConfig::default()
        };
        let args = config.llama_args_for_backend(BackendMode::Metal);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--n-gpu-layers", "all"]));
        assert!(!args.windows(2).any(|pair| pair == ["--device", "none"]));
    }

    #[test]
    fn classifier_recognizes_observed_metal_failure() {
        let logs = "ggml_metal_init: error: failed to create command queue\n\
                    ggml_backend_metal_device_init_backend: error: failed to allocate context";
        assert_eq!(
            classify_startup_failure(logs),
            Some(StartupFailureKind::MetalBackendUnavailable)
        );
    }

    #[test]
    fn classifier_recognizes_unsupported_tokenizer() {
        assert_eq!(
            classify_startup_failure("llama_model_load: unknown tokenizer: deepseek_coder"),
            Some(StartupFailureKind::UnsupportedModel)
        );
    }

    #[test]
    fn http_status_parser_reads_status_code() {
        assert_eq!(parse_http_status("HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(
            parse_http_status("HTTP/1.1 503 Service Unavailable\r\n"),
            Some(503)
        );
        assert_eq!(parse_http_status("garbage"), None);
    }

    #[test]
    fn describe_command_quotes_spaces() {
        let config = ModelServerConfig {
            llama_server: PathBuf::from("/Applications/llama server"),
            model_path: PathBuf::from("/tmp/model file.gguf"),
            startup_timeout: Duration::from_secs(1),
            ..ModelServerConfig::default()
        };
        let command = describe_command(&config, BackendMode::Cpu);
        assert!(command.contains("'/Applications/llama server'"));
        assert!(command.contains("'/tmp/model file.gguf'"));
    }
}
