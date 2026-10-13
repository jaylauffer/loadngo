//! The `device` tool: build a game from the workspace and put it where Jay can see it,
//! on the attached Android phone, the attached iPhone, or this Mac's screen.
//!
//! Before it, a local model asked to "build it for the attached android and ios devices"
//! had no tool for that, and answered that it ran in a sandbox without a compiler
//! (gpt-oss, 2026-10-13, in the chat where its own `cargo test` had passed twice). Builds
//! go through the repository's own scripts (`scripts/android_device_build.sh`,
//! `scripts/ios_device_build.sh`), the ones run by hand, so packaging and linkage
//! workarounds stay in one place. They are always release builds, the ones worth playing.
//!
//! iOS signing is looked up, not configured. Profiles on Jay's team last seven days
//! (`~/pudding/ios-signing-notes.md`), so a fixed path would go stale every week. The tool
//! takes the newest unexpired provisioning profile for the script's bundle id that lists
//! a connected iPhone, and the codesigning identity whose certificate is in that profile,
//! matched by the certificate itself (two identities here share one name). When there is
//! no such profile it says so: Jay refreshes profiles, nothing here can.
//!
//! A phone another agent holds in `AGENT-BOARD.md`'s shared devices table is refused.
//! Commands run through [`work_tools::run`] under the proactor, with time limits. A game
//! started on this Mac keeps running after the call, its output in
//! `target/loadngo-agent-run.log` of its repository; the tool watches it for an early exit
//! with one proactor timer.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use loadngo_proactor::{new_platform_proactor, CompletionKind};
use serde_json::{json, Value};

use crate::board;
use crate::tools::Tool;
use crate::work_tools::{self, Ran, WorkTools};

/// How long a device build may run (a first Android build also rebuilds std).
const BUILD_TIMEOUT: Duration = Duration::from_secs(40 * 60);
/// How long listing devices or keychain entries may take.
const QUERY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a game started on this Mac is watched for an early exit.
const SETTLE: Duration = Duration::from_secs(4);
/// Output kept of a command read as data (device lists, certificates).
const DATA_BYTES: usize = 512 * 1024;
/// Lines of a successful build's output shown.
const SUCCESS_LINES: usize = 12;
const ANDROID_SCRIPT: &str = "scripts/android_device_build.sh";
const IOS_SCRIPT: &str = "scripts/ios_device_build.sh";
const RUN_LOG: &str = "target/loadngo-agent-run.log";
const ACTIONS: [&str; 4] = ["list", "android", "ios", "mac"];
/// Where Xcode keeps provisioning profiles, under the home directory (newer, older).
const PROFILE_DIRS: [&str; 2] = [
    "Library/Developer/Xcode/UserData/Provisioning Profiles",
    "Library/MobileDevice/Provisioning Profiles",
];

/// The device tool for one workspace.
pub struct DeviceTools {
    work: WorkTools,
    agent: String,
    home: Option<PathBuf>,
    /// Games started on this Mac, by directory: one each.
    launched: RefCell<Vec<(PathBuf, Child)>>,
}

impl DeviceTools {
    /// # Errors
    /// When the workspace does not exist.
    pub fn new(workspace: &Path, agent: &str) -> Result<Self, String> {
        Ok(Self {
            work: WorkTools::new(workspace)?,
            agent: agent.to_owned(),
            home: std::env::var_os("HOME").map(PathBuf::from),
            launched: RefCell::default(),
        })
    }

    pub fn into_tools(self) -> Vec<Box<dyn Tool>> {
        vec![Box::new(DeviceTool(self))]
    }

    /// The repository directories with device build scripts, and which.
    fn games(&self) -> Vec<String> {
        let mut games: Vec<String> = fs::read_dir(self.work.workspace())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let dir = entry.path();
                let kinds: Vec<&str> = [("android", ANDROID_SCRIPT), ("ios", IOS_SCRIPT)]
                    .into_iter()
                    .filter(|(_, script)| dir.join(script).is_file())
                    .map(|(kind, _)| kind)
                    .collect();
                (!kinds.is_empty()).then(|| {
                    format!(
                        "{} ({})",
                        entry.file_name().to_string_lossy(),
                        kinds.join(", ")
                    )
                })
            })
            .collect();
        games.sort();
        games
    }

    /// The `dir` argument, which must hold `script`.
    fn repository(&self, args: &Value, script: &str) -> Result<(PathBuf, String), String> {
        let given = args
            .get("dir")
            .and_then(Value::as_str)
            .ok_or("missing string argument `dir` (the game's repository, e.g. sng-mahjong)")?;
        let (dir, shown) = self.work.dir(given)?;
        if !dir.join(script).is_file() {
            return Err(format!(
                "{shown} has no {script}. Repositories with device builds: {}",
                self.games().join(", ")
            ));
        }
        Ok((dir, shown))
    }

    /// Refuses a shared device another agent holds on the board.
    fn free(&self, device: &str) -> Result<(), String> {
        let board =
            fs::read_to_string(self.work.workspace().join("AGENT-BOARD.md")).unwrap_or_default();
        match board::device_holder(&board, &self.agent, device) {
            Some((held, since)) => Err(format!(
                "The {device} is held by {held} (since {since}) in AGENT-BOARD.md's shared \
                 devices; not used. Tell Jay."
            )),
            None => Ok(()),
        }
    }

    fn list(&self) -> String {
        let mut out = String::from("Android (adb devices):\n");
        match android_devices() {
            Ok(list) if list.is_empty() => out.push_str("  none attached\n"),
            Ok(list) => {
                for d in list {
                    out.push_str(&format!("  {}  {}  {}\n", d.serial, d.state, d.model));
                }
            }
            Err(e) => out.push_str(&format!("  cannot list: {e}\n")),
        }
        out.push_str("iPhone (devicectl):\n");
        match ios_devices() {
            Ok(list) if list.is_empty() => out.push_str("  none paired\n"),
            Ok(list) => {
                for d in list {
                    out.push_str(&format!("  {}  {}  {}\n", d.name, d.udid, d.state));
                }
            }
            Err(e) => out.push_str(&format!("  cannot list: {e}\n")),
        }
        for device in ["Android phone", "iPhone"] {
            if let Err(e) = self.free(device) {
                out.push_str(&format!("{e}\n"));
            }
        }
        out.push_str(&format!(
            "Repositories with device builds: {}. Any crate with a binary runs on this Mac \
             (action mac).",
            self.games().join(", ")
        ));
        out
    }

    fn android(&self, args: &Value) -> Result<String, String> {
        let (repo, shown) = self.repository(args, ANDROID_SCRIPT)?;
        self.free("Android phone")?;
        let devices = android_devices()?;
        let serial = pick_android(&devices, args.get("device").and_then(Value::as_str))?;
        let script = fs::read_to_string(repo.join(ANDROID_SCRIPT)).map_err(|e| e.to_string())?;
        let mut command = Command::new("bash");
        command
            .arg(ANDROID_SCRIPT)
            .current_dir(&repo)
            .env("CARGO_TERM_COLOR", "never")
            .env("ANDROID_BUILD_PROFILE", "release")
            .env("ANDROID_INSTALL_ON_DEVICE", "1")
            .env("ANDROID_DEVICE_ID", &serial);
        if std::env::var_os("ANDROID_HOME").is_none()
            && std::env::var_os("ANDROID_SDK_ROOT").is_none()
        {
            if let Some(sdk) = android_sdk() {
                command.env("ANDROID_HOME", sdk);
            }
        }
        let ran = work_tools::run(command, BUILD_TIMEOUT)?;
        let mut out = outcome(
            &format!("{ANDROID_SCRIPT} (release, install) in {shown} for {serial}"),
            &ran,
        );
        if succeeded(&ran) {
            match script_default(&script, "ANDROID_PACKAGE_NAME") {
                Some(package) => {
                    let mut start = Command::new(adb());
                    start.args([
                        "-s",
                        &serial,
                        "shell",
                        "am",
                        "start",
                        "-n",
                        &format!("{package}/android.app.NativeActivity"),
                    ]);
                    let line = match work_tools::run(start, QUERY_TIMEOUT) {
                        Ok(r) if succeeded(&r) && !r.stdout.contains("Error") => {
                            format!("launched {package} on {serial}")
                        }
                        Ok(r) => format!(
                            "installed, but launching {package} failed: {}",
                            first_lines(&format!("{}{}", r.stdout, r.stderr), 3)
                        ),
                        Err(e) => format!("installed, but launching {package} failed: {e}"),
                    };
                    out.push_str(&format!("\n{line}"));
                }
                None => out.push_str("\ninstalled; the script names no package, so not launched"),
            }
        }
        Ok(out)
    }

    fn ios(&self, args: &Value) -> Result<String, String> {
        let (repo, shown) = self.repository(args, IOS_SCRIPT)?;
        self.free("iPhone")?;
        let script = fs::read_to_string(repo.join(IOS_SCRIPT)).map_err(|e| e.to_string())?;
        let bundle = script_default(&script, "IOS_BUNDLE_ID")
            .ok_or_else(|| format!("{shown}/{IOS_SCRIPT} names no default IOS_BUNDLE_ID"))?;
        let wanted = args.get("device").and_then(Value::as_str);
        let devices = ios_devices()?;
        let usable: Vec<&IosDevice> = devices
            .iter()
            .filter(|d| d.usable() && wanted.is_none_or(|w| d.udid == w || d.name == w))
            .collect();
        if usable.is_empty() {
            return Err(format!(
                "No iPhone is connected{} (devicectl lists: {}). Ask Jay to connect or unlock it.",
                wanted.map_or_else(String::new, |w| format!(" as {w}")),
                describe_ios(&devices)
            ));
        }
        // Connected devices first, so a wired phone wins over one seen over Wi-Fi.
        let mut udids: Vec<&str> = usable.iter().map(|d| d.udid.as_str()).collect();
        udids.sort_by_key(|u| {
            !usable
                .iter()
                .any(|d| d.udid == *u && d.state == "connected")
        });
        let profiles = self.profiles();
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let (profile, udid) = choose_profile(&profiles, &bundle, &udids, &now)?;
        let identity = identity_for(profile)?;
        let mut command = Command::new("bash");
        command
            .arg(IOS_SCRIPT)
            .current_dir(&repo)
            .env("CARGO_TERM_COLOR", "never")
            .env("IOS_BUILD_PROFILE", "release")
            .env("IOS_SIGN_IDENTITY", &identity)
            .env("IOS_PROVISIONING_PROFILE", &profile.path)
            .env("IOS_INSTALL_ON_DEVICE", "1")
            .env("IOS_INSTALL_METHOD", "devicectl")
            .env("IOS_DEVICE_ID", udid);
        let ran = work_tools::run(command, BUILD_TIMEOUT)?;
        let name = devices
            .iter()
            .find(|d| d.udid == udid)
            .map_or(udid, |d| d.name.as_str());
        Ok(format!(
            "{}{}\nsigned with profile {} (expires {}) and identity {identity}",
            outcome(
                &format!("{IOS_SCRIPT} (release, install, launch) in {shown} for {name} ({udid})"),
                &ran
            ),
            locked_hint(&ran),
            profile.uuid,
            profile.expires
        ))
    }

    /// Every provisioning profile Xcode keeps.
    fn profiles(&self) -> Vec<Profile> {
        let Some(home) = &self.home else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for dir in PROFILE_DIRS {
            for entry in fs::read_dir(home.join(dir)).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "mobileprovision") {
                    if let Some(profile) = fs::read(&path)
                        .ok()
                        .and_then(|bytes| read_profile(&path, &bytes))
                    {
                        out.push(profile);
                    }
                }
            }
        }
        out
    }

    fn mac(&self, args: &Value) -> Result<String, String> {
        let given = args
            .get("dir")
            .and_then(Value::as_str)
            .ok_or("missing string argument `dir` (a crate with a binary, e.g. sng-mahjong)")?;
        let (dir, shown) = self.work.dir(given)?;
        if !dir.join("Cargo.toml").is_file() {
            return Err(format!("{shown} has no Cargo.toml"));
        }
        let extra = work_tools::arguments(args, &work_tools::REFUSED_CARGO_ARGS)?;
        let cargo_args: Vec<&String> = extra.iter().take_while(|a| *a != "--").collect();
        // Build first, under the time limit, so a compile error is this call's result.
        let mut build = Command::new("cargo");
        build
            .current_dir(&dir)
            .env("CARGO_TERM_COLOR", "never")
            .args(["build", "--release"])
            .args(&cargo_args);
        let ran = work_tools::run(build, work_tools::CARGO_TIMEOUT)?;
        let built = format!("cargo build --release {} in {shown}", join(&cargo_args));
        if !succeeded(&ran) {
            return Ok(outcome(&built, &ran));
        }
        self.stop(&dir);
        let repo = shown.split('/').next().unwrap_or(".");
        let log_path = self.work.workspace().join(repo).join(RUN_LOG);
        let log_shown = format!("{repo}/{RUN_LOG}");
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let log = fs::File::create(&log_path).map_err(|e| format!("{log_shown}: {e}"))?;
        let mut run = Command::new("cargo");
        run.current_dir(&dir)
            .env("CARGO_TERM_COLOR", "never")
            .args(["run", "--release"])
            .args(&extra)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log);
        let mut child = run
            .spawn()
            .map_err(|e| format!("cannot start cargo run: {e}"))?;
        wait(SETTLE)?;
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = fs::read_to_string(&log_path).unwrap_or_default();
                Ok(format!(
                    "{built}: succeeded. cargo run --release {} then exited ({status}) within \
                     {}s of starting. The end of its output ({log_shown}):\n{}",
                    join(&extra.iter().collect::<Vec<_>>()),
                    SETTLE.as_secs(),
                    last_lines(&text, 40)
                ))
            }
            Ok(None) => {
                let pid = child.id();
                self.launched.borrow_mut().push((dir, child));
                Ok(format!(
                    "{built}: succeeded. The game is running on this Mac's screen (pid {pid}); \
                     its output goes to {log_shown} (fs_read it for logs). Calling mac again \
                     for {shown} restarts it."
                ))
            }
            Err(e) => Err(format!("cannot check the game process: {e}")),
        }
    }

    /// Stops the game this tool started from `dir`, and forgets games that ended.
    fn stop(&self, dir: &Path) {
        self.launched.borrow_mut().retain_mut(|(d, child)| {
            if d == dir {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            !matches!(child.try_wait(), Ok(Some(_)))
        });
    }
}

struct DeviceTool(DeviceTools);

impl Tool for DeviceTool {
    fn name(&self) -> &'static str {
        "device"
    }
    fn description(&self) -> &'static str {
        "Build a game and run it where Jay can see it. action list: the attached Android phone \
         and iPhone, and which repositories have device builds. android or ios: run the \
         repository's own scripts/*_device_build.sh (release build, install and launch on the \
         attached phone; iOS signing is found automatically); takes minutes. mac: cargo build \
         --release, then start the game on this Mac's screen and leave it running."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "action": {"type": "string", "enum": ACTIONS},
            "dir": {"type": "string", "description": "the game's repository (android, ios) or crate (mac), relative to the workspace, e.g. sng-mahjong"},
            "device": {"type": "string", "description": "an adb serial or iPhone UDID from list, when several are attached"},
            "args": {"type": "array", "items": {"type": "string"}, "description": "mac only: further cargo run arguments, e.g. [\"-p\", \"sng-roguelite-game\"]"}},
            "required": ["action"]})
    }
    fn call(&self, args: &Value) -> Result<String, String> {
        match args.get("action").and_then(Value::as_str) {
            Some("list") => Ok(self.0.list()),
            Some("android") => self.0.android(args),
            Some("ios") => self.0.ios(args),
            Some("mac") => self.0.mac(args),
            _ => Err(format!("action must be one of {ACTIONS:?}")),
        }
    }
}

fn succeeded(ran: &Ran) -> bool {
    !ran.timed_out && ran.status == Some(0)
}

fn join(args: &[&String]) -> String {
    args.iter()
        .map(|a| a.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn first_lines(text: &str, n: usize) -> String {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .collect::<Vec<_>>()
        .join(" / ")
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// A build's result: on success its status and last lines; on failure the whole report
/// (the end of each stream, where the errors are).
fn outcome(what: &str, ran: &Ran) -> String {
    if !succeeded(ran) {
        return work_tools::report(what, ran);
    }
    let shown = if ran.stdout.trim().is_empty() {
        &ran.stderr
    } else {
        &ran.stdout
    };
    format!(
        "{what}: succeeded (exit 0) after {:.1}s\n--- end of output:\n{}",
        ran.elapsed.as_secs_f64(),
        last_lines(shown.trim_end(), SUCCESS_LINES)
    )
}

/// When the app installed but iOS would not launch it on a locked phone: a line saying
/// so, since the error itself runs to twenty lines and does not say the install worked.
fn locked_hint(ran: &Ran) -> &'static str {
    let all = format!("{}{}", ran.stdout, ran.stderr);
    if !succeeded(ran)
        && all.contains("App installed:")
        && all.contains("BSErrorCodeDescription = Locked")
    {
        "\n[The app is installed on the iPhone; it did not launch only because the phone is \
         locked. Tell Jay to unlock it and open the app, or call ios again once it is \
         unlocked.]"
    } else {
        ""
    }
}

/// Waits `duration` in a proactor (one timer; nothing polls).
fn wait(duration: Duration) -> Result<(), String> {
    let proactor = new_platform_proactor().map_err(|e| format!("proactor: {e}"))?;
    let fired = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&fired);
    proactor
        .handle()
        .defer_for(duration, CompletionKind::Timer, 0, move |_| {
            flag.store(true, Ordering::Release);
        })
        .map_err(|e| format!("proactor: {e}"))?;
    while !fired.load(Ordering::Acquire) {
        proactor.run_once().map_err(|e| format!("proactor: {e}"))?;
    }
    Ok(())
}

/// A command whose output is read as data; a failure is an error.
fn query(mut command: Command) -> Result<Ran, String> {
    command.env("CARGO_TERM_COLOR", "never");
    let program = command.get_program().to_string_lossy().into_owned();
    let ran = work_tools::run_keeping(command, QUERY_TIMEOUT, DATA_BYTES)?;
    if succeeded(&ran) {
        Ok(ran)
    } else {
        Err(format!(
            "{program} failed: {}",
            first_lines(&format!("{}\n{}", ran.stderr, ran.stdout), 3)
        ))
    }
}

/// The default a script gives a variable, from its `NAME:-default}` expansion.
fn script_default(script: &str, name: &str) -> Option<String> {
    let marker = format!("{name}:-");
    let start = script.find(&marker)? + marker.len();
    let end = script[start..].find('}')? + start;
    let value = script[start..end].trim();
    (!value.is_empty() && !value.contains('$')).then(|| value.to_owned())
}

/// The Android SDK: `ANDROID_SDK_ROOT`, `ANDROID_HOME`, or Android Studio's default place.
fn android_sdk() -> Option<PathBuf> {
    ["ANDROID_SDK_ROOT", "ANDROID_HOME"]
        .iter()
        .find_map(|v| std::env::var_os(v).map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join("Library/Android/sdk"))
                .filter(|p| p.is_dir())
        })
}

fn adb() -> PathBuf {
    android_sdk()
        .map(|sdk| sdk.join("platform-tools/adb"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("adb"))
}

#[derive(Debug, PartialEq, Eq)]
struct AndroidDevice {
    serial: String,
    /// `device` when attached and authorized; `unauthorized`, `offline` otherwise.
    state: String,
    model: String,
}

fn android_devices() -> Result<Vec<AndroidDevice>, String> {
    let mut command = Command::new(adb());
    command.args(["devices", "-l"]);
    Ok(parse_adb(&query(command)?.stdout))
}

/// `adb devices -l` output.
fn parse_adb(text: &str) -> Vec<AndroidDevice> {
    text.lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let serial = words.next()?.to_owned();
            let state = words.next()?.to_owned();
            let model = words
                .find_map(|w| w.strip_prefix("model:"))
                .unwrap_or("")
                .to_owned();
            Some(AndroidDevice {
                serial,
                state,
                model,
            })
        })
        .collect()
}

/// The serial to install on: the one asked for, or the only attached one.
fn pick_android(devices: &[AndroidDevice], wanted: Option<&str>) -> Result<String, String> {
    let listed = || {
        if devices.is_empty() {
            "none".to_owned()
        } else {
            devices
                .iter()
                .map(|d| format!("{} {}", d.serial, d.state))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let ready: Vec<&AndroidDevice> = devices.iter().filter(|d| d.state == "device").collect();
    if let Some(w) = wanted {
        return ready
            .iter()
            .find(|d| d.serial == w)
            .map(|d| d.serial.clone())
            .ok_or_else(|| format!("{w} is not attached and ready (adb devices: {})", listed()));
    }
    match ready.as_slice() {
        [one] => Ok(one.serial.clone()),
        [] if devices.iter().any(|d| d.state == "unauthorized") => Err(format!(
            "The Android phone is attached but not authorized (adb devices: {}). Ask Jay to \
             accept the USB debugging prompt on it.",
            listed()
        )),
        [] => Err(format!(
            "No Android device is attached (adb devices: {}). Ask Jay to connect it.",
            listed()
        )),
        _ => Err(format!(
            "Several Android devices are attached ({}); pass device with one serial.",
            listed()
        )),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct IosDevice {
    name: String,
    udid: String,
    /// devicectl's tunnel state: `connected`, `disconnected`, `unavailable`.
    state: String,
}

impl IosDevice {
    /// Reachable now, or when devicectl connects to it.
    fn usable(&self) -> bool {
        self.state != "unavailable"
    }
}

fn ios_devices() -> Result<Vec<IosDevice>, String> {
    let mut command = Command::new("xcrun");
    command.args(["devicectl", "list", "devices", "--json-output", "-", "-q"]);
    let ran = query(command)?;
    let json: Value = serde_json::from_str(&ran.stdout)
        .map_err(|e| format!("devicectl's device list is not JSON: {e}"))?;
    Ok(parse_devicectl(&json))
}

/// The iOS devices in `devicectl list devices --json-output` output.
fn parse_devicectl(json: &Value) -> Vec<IosDevice> {
    json["result"]["devices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| d["hardwareProperties"]["platform"] == "iOS")
        .filter_map(|d| {
            Some(IosDevice {
                name: d["deviceProperties"]["name"]
                    .as_str()
                    .unwrap_or("?")
                    .to_owned(),
                udid: d["hardwareProperties"]["udid"].as_str()?.to_owned(),
                state: d["connectionProperties"]["tunnelState"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_owned(),
            })
        })
        .collect()
}

fn describe_ios(devices: &[IosDevice]) -> String {
    if devices.is_empty() {
        return "none paired".into();
    }
    devices
        .iter()
        .map(|d| format!("{} {}", d.name, d.state))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a provisioning profile says, from the property list inside its signature.
#[derive(Debug)]
struct Profile {
    path: PathBuf,
    uuid: String,
    /// `TEAMID.bundle.id`, or with a `*` wildcard.
    app_id: String,
    /// `YYYY-MM-DDTHH:MM:SSZ`, UTC.
    expires: String,
    devices: Vec<String>,
    /// The developer certificates, base64 without whitespace.
    certificates: Vec<String>,
}

impl Profile {
    /// 2 for this bundle exactly, 1 through a wildcard, 0 not at all.
    fn covers(&self, bundle: &str) -> u8 {
        let Some((_, id)) = self.app_id.split_once('.') else {
            return 0;
        };
        if id == bundle {
            2
        } else if id == "*"
            || id
                .strip_suffix('*')
                .is_some_and(|prefix| bundle.starts_with(prefix))
        {
            1
        } else {
            0
        }
    }
}

/// The plist a `.mobileprovision` carries in plain text inside its CMS signature. Only
/// used to choose a file: `codesign` and the install check the signature.
fn read_profile(path: &Path, bytes: &[u8]) -> Option<Profile> {
    let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|w| w == needle);
    let start = find(bytes, b"<?xml")?;
    let end = start + find(&bytes[start..], b"</plist>")?;
    let xml = std::str::from_utf8(&bytes[start..end]).ok()?;
    Some(Profile {
        path: path.to_owned(),
        uuid: value_after(xml, "UUID", "string").unwrap_or_default(),
        app_id: value_after(xml, "application-identifier", "string")?,
        expires: value_after(xml, "ExpirationDate", "date")?,
        devices: array_after(xml, "ProvisionedDevices", "string"),
        certificates: array_after(xml, "DeveloperCertificates", "data")
            .into_iter()
            .map(|c| c.split_whitespace().collect())
            .collect(),
    })
}

/// The text after `<key>key</key>`, from just past the next opening tag, when only
/// whitespace comes before `open`.
fn after_key<'x>(xml: &'x str, key: &str, open: &str) -> Option<&'x str> {
    let at = xml.find(&format!("<key>{key}</key>"))? + key.len() + 11;
    let rest = xml[at..].trim_start();
    rest.strip_prefix(open)
}

fn value_after(xml: &str, key: &str, tag: &str) -> Option<String> {
    let rest = after_key(xml, key, &format!("<{tag}>"))?;
    let end = rest.find(&format!("</{tag}>"))?;
    Some(rest[..end].trim().to_owned())
}

fn array_after(xml: &str, key: &str, tag: &str) -> Vec<String> {
    let Some(rest) = after_key(xml, key, "<array>") else {
        return Vec::new();
    };
    let body = &rest[..rest.find("</array>").unwrap_or(rest.len())];
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    body.split(open.as_str())
        .skip(1)
        .filter_map(|item| item.find(close.as_str()).map(|end| item[..end].to_owned()))
        .collect()
}

/// The profile to sign with and the device to install on: the newest unexpired profile
/// for `bundle` (an exact one before a wildcard) that lists one of `udids` (in order of
/// preference).
fn choose_profile<'p, 'u>(
    profiles: &'p [Profile],
    bundle: &str,
    udids: &[&'u str],
    now: &str,
) -> Result<(&'p Profile, &'u str), String> {
    let refresh = "Profiles on Jay's team last 7 days and only Jay can refresh them \
                   (~/pudding/ios-signing-notes.md, \"The refresh procedure\"): ask him.";
    let mine: Vec<&Profile> = profiles.iter().filter(|p| p.covers(bundle) > 0).collect();
    if mine.is_empty() {
        return Err(format!(
            "No provisioning profile for {bundle} in ~/{} or ~/{}. {refresh}",
            PROFILE_DIRS[0], PROFILE_DIRS[1]
        ));
    }
    mine.iter()
        .filter(|p| p.expires.as_str() > now)
        .filter_map(|p| {
            udids
                .iter()
                .find(|u| p.devices.iter().any(|d| d == *u))
                .map(|u| (*p, *u))
        })
        .max_by(|a, b| (a.0.covers(bundle), &a.0.expires).cmp(&(b.0.covers(bundle), &b.0.expires)))
        .ok_or_else(|| {
            let seen: Vec<String> = mine
                .iter()
                .map(|p| {
                    format!(
                        "{} expires {}, devices {}",
                        p.uuid,
                        p.expires,
                        p.devices.join(" ")
                    )
                })
                .collect();
            format!(
                "No unexpired profile for {bundle} lists a connected iPhone ({}); found: {}. \
                 {refresh}",
                udids.join(", "),
                seen.join("; ")
            )
        })
}

/// The valid codesigning identity whose certificate `profile` carries.
fn identity_for(profile: &Profile) -> Result<String, String> {
    let mut command = Command::new("security");
    command.args(["find-identity", "-v", "-p", "codesigning"]);
    let identities = parse_identities(&query(command)?.stdout);
    let mut names: Vec<&str> = identities.iter().map(|(_, n)| n.as_str()).collect();
    names.dedup();
    let mut certificates = Vec::new();
    for name in names {
        let mut command = Command::new("security");
        command.args(["find-certificate", "-a", "-Z", "-p", "-c", name]);
        certificates.extend(parse_certificates(&query(command)?.stdout));
    }
    matching_identity(&identities, &certificates, profile).ok_or_else(|| {
        format!(
            "No valid codesigning identity in the keychain matches a certificate in profile \
             {}. Tell Jay.",
            profile.uuid
        )
    })
}

/// The first valid identity (by SHA-1) whose certificate body is one `profile` carries.
fn matching_identity(
    identities: &[(String, String)],
    certificates: &[(String, String)],
    profile: &Profile,
) -> Option<String> {
    certificates
        .iter()
        .find(|(hash, body)| {
            identities.iter().any(|(h, _)| h == hash) && profile.certificates.contains(body)
        })
        .map(|(hash, _)| hash.clone())
}

/// `(SHA-1, name)` of each identity `security find-identity -v` lists.
fn parse_identities(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (_, rest) = line.split_once(") ")?;
            let (hash, name) = rest.split_once(' ')?;
            let name = name.trim().strip_prefix('"')?.strip_suffix('"')?;
            (hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| (hash.to_owned(), name.to_owned()))
        })
        .collect()
}

/// `(SHA-1, base64 body)` of each certificate `security find-certificate -Z -p` prints.
fn parse_certificates(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut hash = String::new();
    let mut body: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(h) = line.strip_prefix("SHA-1 hash: ") {
            hash = h.to_owned();
        } else if line == "-----BEGIN CERTIFICATE-----" {
            body = Some(String::new());
        } else if line == "-----END CERTIFICATE-----" {
            if let Some(b) = body.take() {
                out.push((hash.clone(), b));
            }
        } else if let Some(b) = &mut body {
            b.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(app_id: &str, expires: &str, devices: &[&str]) -> Profile {
        Profile {
            path: PathBuf::from(format!("{app_id}-{expires}.mobileprovision")),
            uuid: format!("{app_id}-{expires}"),
            app_id: app_id.into(),
            expires: expires.into(),
            devices: devices.iter().map(|d| (*d).to_owned()).collect(),
            certificates: Vec::new(),
        }
    }

    #[test]
    fn a_profiles_plist_is_read_from_inside_its_signature() {
        let mut bytes = b"\x30\x82\x01binary CMS header ".to_vec();
        bytes.extend_from_slice(
            b"<?xml version=\"1.0\"?>\n<plist><dict>\n\
              <key>DeveloperCertificates</key>\n\t<array>\n\t\t<data>MIIF\n\t\tzjCC</data>\n\
              \t\t<data>QUJD</data>\n\t</array>\n\
              <key>Entitlements</key>\n<dict>\n\t<key>application-identifier</key>\n\
              \t\t<string>5PJMVLCEVH.com.jarraya.games.sngmahjong</string>\n</dict>\n\
              <key>ExpirationDate</key>\n\t<date>2026-10-19T17:57:16Z</date>\n\
              <key>ProvisionedDevices</key>\n\t<array>\n\t\t<string>00008110-001E</string>\n\
              \t</array>\n<key>UUID</key>\n\t<string>cf60c22d</string>\n</dict></plist>",
        );
        bytes.extend_from_slice(b"\x00\xa0 trailing signature bytes");
        let p = read_profile(Path::new("x.mobileprovision"), &bytes).unwrap();
        assert_eq!(p.app_id, "5PJMVLCEVH.com.jarraya.games.sngmahjong");
        assert_eq!(p.expires, "2026-10-19T17:57:16Z");
        assert_eq!(p.devices, ["00008110-001E"]);
        assert_eq!(p.certificates, ["MIIFzjCC", "QUJD"]);
        assert_eq!(p.uuid, "cf60c22d");
        assert!(read_profile(Path::new("y"), b"no plist here").is_none());
    }

    #[test]
    fn the_newest_unexpired_profile_listing_a_connected_phone_is_chosen() {
        let now = "2026-10-13T08:00:00Z";
        let bundle = "com.jarraya.games.sngmahjong";
        let profiles = [
            profile(
                "T.com.jarraya.games.sngmahjong",
                "2026-10-12T17:00:00Z",
                &["A"],
            ),
            profile(
                "T.com.jarraya.games.sngmahjong",
                "2026-10-19T17:00:00Z",
                &["A"],
            ),
            profile(
                "T.com.jarraya.games.sngmahjong",
                "2026-10-20T17:00:00Z",
                &["B"],
            ),
            profile("T.*", "2026-10-25T17:00:00Z", &["A"]),
            profile(
                "T.com.jarraya.games.sngzhoenus",
                "2026-10-30T17:00:00Z",
                &["A"],
            ),
        ];
        // An exact profile wins over a later wildcard; the expired one is skipped.
        let (p, udid) = choose_profile(&profiles, bundle, &["A"], now).unwrap();
        assert_eq!((p.expires.as_str(), udid), ("2026-10-19T17:00:00Z", "A"));
        // Only the profile for device B lists B.
        let (p, udid) = choose_profile(&profiles, bundle, &["B", "A"], now).unwrap();
        assert_eq!((p.expires.as_str(), udid), ("2026-10-20T17:00:00Z", "B"));
        // The wildcard serves a bundle with no profile of its own.
        let (p, _) = choose_profile(&profiles, "com.other", &["A"], now).unwrap();
        assert_eq!(p.app_id, "T.*");
        // All expired: the refusal names what was found and who refreshes.
        let later = "2026-11-01T00:00:00Z";
        let error = choose_profile(&profiles, bundle, &["A"], later).unwrap_err();
        assert!(
            error.contains("expires 2026-10-19") && error.contains("ask him"),
            "{error}"
        );
        let error = choose_profile(&profiles[..1], "com.none", &["A"], now).unwrap_err();
        assert!(
            error.contains("No provisioning profile for com.none"),
            "{error}"
        );
    }

    #[test]
    fn the_identity_is_matched_by_the_certificate_not_the_shared_name() {
        let identities = parse_identities(
            "  1) ED54F607ECB08A8BB902FC63E3E972EEC59F0EAC \"Apple Development: j (D)\"\n\
             \x20 2) 001A93501FDB5CA2231A689667E2174E38E71D6A \"Apple Development: j (D)\"\n\
             \x20    2 valid identities found\n",
        );
        assert_eq!(identities.len(), 2);
        assert_eq!(identities[1].1, "Apple Development: j (D)");
        let certificates = parse_certificates(
            "SHA-256 hash: 1F0F\nSHA-1 hash: ED54F607ECB08A8BB902FC63E3E972EEC59F0EAC\n\
             -----BEGIN CERTIFICATE-----\nMIIF\nzjCC\n-----END CERTIFICATE-----\n\
             SHA-1 hash: 001A93501FDB5CA2231A689667E2174E38E71D6A\n\
             -----BEGIN CERTIFICATE-----\nQUJD\n-----END CERTIFICATE-----\n",
        );
        // Both share a name; the profile carries only the second one's certificate.
        let mut p = profile("T.x", "2026-10-19T00:00:00Z", &[]);
        p.certificates = vec!["QUJD".into()];
        assert_eq!(
            matching_identity(&identities, &certificates, &p).as_deref(),
            Some("001A93501FDB5CA2231A689667E2174E38E71D6A")
        );
        // A certificate whose identity is not valid (no key, expired) does not sign.
        assert_eq!(matching_identity(&identities[..1], &certificates, &p), None);
        p.certificates = vec!["OTHER".into()];
        assert_eq!(matching_identity(&identities, &certificates, &p), None);
        assert_eq!(
            certificates,
            [
                (
                    "ED54F607ECB08A8BB902FC63E3E972EEC59F0EAC".to_owned(),
                    "MIIFzjCC".to_owned()
                ),
                (
                    "001A93501FDB5CA2231A689667E2174E38E71D6A".to_owned(),
                    "QUJD".to_owned()
                )
            ]
        );
    }

    #[test]
    fn devices_are_read_from_adb_and_devicectl() {
        let adb = parse_adb(
            "* daemon started successfully\nList of devices attached\n\
             e8a6ab881222           device usb:51445760X product:sunstone_in model:22111317I \
             device:sunstone transport_id:2\nZX1          unauthorized usb:1-1 transport_id:3\n\n",
        );
        assert_eq!(adb.len(), 2);
        assert_eq!(adb[0].model, "22111317I");
        assert_eq!(pick_android(&adb, None).unwrap(), "e8a6ab881222");
        assert!(pick_android(&adb, Some("ZX1"))
            .unwrap_err()
            .contains("not attached and ready"));
        assert!(pick_android(&adb[1..], None)
            .unwrap_err()
            .contains("not authorized"));
        assert!(pick_android(&[], None)
            .unwrap_err()
            .contains("No Android device"));
        let two = [
            adb.into_iter().next().unwrap(),
            AndroidDevice {
                serial: "S2".into(),
                state: "device".into(),
                model: String::new(),
            },
        ];
        assert!(pick_android(&two, None).unwrap_err().contains("Several"));
        assert_eq!(pick_android(&two, Some("S2")).unwrap(), "S2");

        let json: Value = serde_json::from_str(
            r#"{"result":{"devices":[
              {"deviceProperties":{"name":"Arraya"},"hardwareProperties":{"udid":"0812","platform":"iOS"},
               "connectionProperties":{"tunnelState":"unavailable"}},
              {"deviceProperties":{"name":"Zheonus VI"},"hardwareProperties":{"udid":"0811","platform":"iOS"},
               "connectionProperties":{"tunnelState":"connected"}},
              {"deviceProperties":{"name":"Watch"},"hardwareProperties":{"udid":"W","platform":"watchOS"},
               "connectionProperties":{"tunnelState":"connected"}}]}}"#,
        )
        .unwrap();
        let ios = parse_devicectl(&json);
        assert_eq!(ios.len(), 2);
        assert!(!ios[0].usable() && ios[1].usable());
        assert_eq!(ios[1].name, "Zheonus VI");
    }

    #[test]
    fn an_install_on_a_locked_iphone_says_it_installed() {
        let ran = |status, stdout: &str, stderr: &str| Ran {
            status: Some(status),
            timed_out: false,
            elapsed: Duration::from_secs(22),
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        // devicectl's output on 2026-10-13, cut down.
        let locked = ran(
            1,
            "Built and signed app bundle: x.app\nApp installed:\n• bundleID: com.x\n",
            "ERROR: The application failed to launch.\n       BSErrorCodeDescription = Locked\n",
        );
        assert!(locked_hint(&locked).contains("installed on the iPhone"));
        let failed_build = ran(101, "", "error[E0308]: mismatched types\n");
        assert_eq!(locked_hint(&failed_build), "");
        let fine = ran(0, "App installed:\nLaunched application", "");
        assert_eq!(locked_hint(&fine), "");
    }

    #[test]
    fn a_scripts_default_is_read_and_a_computed_one_is_not() {
        let script = "PACKAGE_NAME=\"${ANDROID_PACKAGE_NAME:-com.jarraya.games.sngmahjong}\"\n\
                      OUT=\"${ANDROID_OUT:-$ROOT/build}\"\n";
        assert_eq!(
            script_default(script, "ANDROID_PACKAGE_NAME").as_deref(),
            Some("com.jarraya.games.sngmahjong")
        );
        assert_eq!(script_default(script, "ANDROID_OUT"), None);
        assert_eq!(script_default(script, "IOS_BUNDLE_ID"), None);
    }

    #[test]
    fn builds_need_a_repository_with_the_script_and_a_free_phone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("game/scripts")).unwrap();
        fs::write(root.join("game/scripts/android_device_build.sh"), "").unwrap();
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::write(
            root.join("AGENT-BOARD.md"),
            "## Shared devices\n\n| Resource | Held by | Since | Notes |\n|---|---|---|---|\n\
             | Android phone — Xiaomi | Codex | 2026-10-13 | testing |\n\
             | iPhone — Zheonus VI | free | 2026-10-13 | |\n",
        )
        .unwrap();
        let tools = DeviceTools::new(root, "gpt-oss").unwrap();
        assert_eq!(tools.games(), ["game (android)"]);
        let error = tools
            .android(&json!({"action": "android", "dir": "lib"}))
            .unwrap_err();
        assert!(
            error.contains("lib has no scripts/android_device_build.sh")
                && error.contains("game (android)"),
            "{error}"
        );
        let error = tools
            .android(&json!({"action": "android", "dir": "game"}))
            .unwrap_err();
        assert!(error.contains("held by Codex"), "{error}");
        let error = tools
            .android(&json!({"action": "android", "dir": ".."}))
            .unwrap_err();
        assert!(error.contains("outside the workspace"), "{error}");
        let tool = DeviceTool(DeviceTools::new(root, "Codex").unwrap());
        let error = tool.call(&json!({"action": "flash"})).unwrap_err();
        assert!(error.contains("action must be one of"), "{error}");
    }

    #[test]
    fn a_game_on_this_mac_is_left_running_and_an_early_exit_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let krate = |name: &str, main: &str| {
            fs::create_dir_all(root.join(name).join("src")).unwrap();
            fs::write(
                root.join(name).join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                     [workspace]\n"
                ),
            )
            .unwrap();
            fs::write(root.join(name).join("src/main.rs"), main).unwrap();
        };
        krate(
            "stays",
            "fn main() { println!(\"up\"); std::thread::sleep(std::time::Duration::from_secs(120)); }",
        );
        krate(
            "quits",
            "fn main() { eprintln!(\"no window\"); std::process::exit(3); }",
        );
        krate("broken", "fn main() { let x: u8 = \"s\"; }");
        let tools = DeviceTools::new(root, "gpt-oss").unwrap();

        let running = tools.mac(&json!({"dir": "stays"})).unwrap();
        assert!(
            running.contains("running on this Mac's screen"),
            "{running}"
        );
        assert_eq!(tools.launched.borrow().len(), 1);
        // A second start in the same directory replaces the first.
        tools.mac(&json!({"dir": "stays"})).unwrap();
        assert_eq!(tools.launched.borrow().len(), 1);
        let log = fs::read_to_string(root.join("stays").join(RUN_LOG)).unwrap();
        assert!(log.contains("up"), "{log}");

        let quit = tools.mac(&json!({"dir": "quits"})).unwrap();
        assert!(
            quit.contains("exited") && quit.contains("no window"),
            "{quit}"
        );
        let broken = tools.mac(&json!({"dir": "broken"})).unwrap();
        assert!(
            broken.contains("FAILED") && broken.contains("mismatched types"),
            "{broken}"
        );
        let refused = tools
            .mac(&json!({"dir": "stays", "args": ["--target-dir", "/tmp/x"]}))
            .unwrap_err();
        assert!(refused.contains("not allowed"), "{refused}");

        tools.stop(&root.join("stays").canonicalize().unwrap());
        assert!(tools.launched.borrow().is_empty());
    }
}
