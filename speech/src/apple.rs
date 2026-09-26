//! Apple Speech (recognition) and AVFAudio (microphone, synthesis). The only unsafe code
//! in the crate: Objective-C messages to framework objects.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{class, define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_foundation::{NSArray, NSString};

use crate::Error;

#[link(name = "Speech", kind = "framework")]
extern "C" {}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: *const std::ffi::c_void;
    fn CFRunLoopRunInMode(
        mode: *const std::ffi::c_void,
        seconds: f64,
        return_after_source: u8,
    ) -> i32;
}

/// Waits for `rx` while running this thread's run loop, through which AppKit and
/// AVFoundation deliver callbacks to a program's main thread. The run loop sleeps until
/// an event arrives or the slice ends; nothing spins.
fn wait_with_run_loop<T>(rx: &Receiver<T>, bound: Duration) -> Option<T> {
    let deadline = Instant::now() + bound;
    loop {
        if let Ok(value) = rx.try_recv() {
            return Some(value);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        // SAFETY: runs the current thread's run loop in the default mode for a slice.
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, left.as_secs_f64().min(0.5), 1);
        }
    }
}
#[link(name = "AVFAudio", kind = "framework")]
extern "C" {}

const AUTHORIZED: isize = 3; // SFSpeechRecognizerAuthorizationStatusAuthorized
const TASK_HINT_DICTATION: isize = 1;

/// A framework object handed between threads. The objects kept this way (a recognition
/// request, which the audio tap appends to) are documented as usable from the audio
/// thread; everything else stays on the thread that made it.
struct Shared(Retained<AnyObject>);
// SAFETY: see the type's documentation.
unsafe impl Send for Shared {}

fn ns_error(error: *mut AnyObject) -> String {
    if error.is_null() {
        return "unknown error".into();
    }
    // SAFETY: a live NSError from the framework.
    unsafe {
        let text: Retained<NSString> = msg_send![error, localizedDescription];
        text.to_string()
    }
}

fn status_text(status: isize) -> &'static str {
    match status {
        0 => "not asked yet",
        1 => "denied (System Settings > Privacy & Security > Speech Recognition)",
        2 => "restricted on this Mac",
        _ => "unknown",
    }
}

/// Asks for speech-recognition permission if it has not been decided, and waits for the
/// answer (macOS shows a dialog once). Microphone permission is asked by the system
/// when listening starts.
///
/// # Errors
/// [`Error::NotAuthorized`] when permission is denied or restricted.
pub fn request_authorization() -> Result<(), Error> {
    // SAFETY: class methods of SFSpeechRecognizer; the block is copied by the framework
    // and called once, on an arbitrary queue, with the status.
    let status: isize = unsafe { msg_send![class!(SFSpeechRecognizer), authorizationStatus] };
    if status == AUTHORIZED {
        return Ok(());
    }
    if status != 0 {
        return Err(Error::NotAuthorized(status_text(status).into()));
    }
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |status: isize| {
        let _ = tx.send(status);
    });
    unsafe {
        let _: () = msg_send![class!(SFSpeechRecognizer), requestAuthorization: &*block];
    }
    // The request and its answer travel through the main run loop.
    match wait_with_run_loop(&rx, Duration::from_secs(600)) {
        Some(AUTHORIZED) => Ok(()),
        Some(other) => Err(Error::NotAuthorized(status_text(other).into())),
        None => Err(Error::NotAuthorized(
            "no answer to the permission dialog".into(),
        )),
    }
}

/// An on-device recognizer for `locale`, delivering results on its own serial queue.
fn recognizer(locale: &str) -> Result<Retained<AnyObject>, Error> {
    // SAFETY: plain framework messages; `initWithLocale:` returns nil for an unknown
    // locale, checked below.
    unsafe {
        let status: isize = msg_send![class!(SFSpeechRecognizer), authorizationStatus];
        if status != AUTHORIZED {
            return Err(Error::NotAuthorized(status_text(status).into()));
        }
        let id = NSString::from_str(locale);
        let locale_obj: Retained<AnyObject> =
            msg_send![class!(NSLocale), localeWithLocaleIdentifier: &*id];
        let alloc: Allocated<AnyObject> = msg_send![class!(SFSpeechRecognizer), alloc];
        let recognizer: Option<Retained<AnyObject>> =
            msg_send![alloc, initWithLocale: &*locale_obj];
        let recognizer = recognizer.ok_or_else(|| Error::NoOnDeviceModel(locale.into()))?;
        let on_device: bool = msg_send![&*recognizer, supportsOnDeviceRecognition];
        if !on_device {
            return Err(Error::NoOnDeviceModel(locale.into()));
        }
        // Results default to the main queue; a CLI waiting on its main thread would
        // never see them.
        let queue: Retained<AnyObject> = msg_send![class!(NSOperationQueue), new];
        let _: () = msg_send![&*queue, setMaxConcurrentOperationCount: 1isize];
        let _: () = msg_send![&*recognizer, setQueue: &*queue];
        Ok(recognizer)
    }
}

enum Kind {
    Partial(String),
    Final(String),
    Failed(String),
}

struct Event {
    task: u64,
    kind: Kind,
}

/// The result handler for recognition task number `task`.
fn result_handler(
    task: u64,
    events: Sender<Event>,
) -> RcBlock<dyn Fn(*mut AnyObject, *mut AnyObject)> {
    RcBlock::new(move |result: *mut AnyObject, error: *mut AnyObject| {
        // SAFETY: the framework passes a live result or error, valid for this call.
        let kind = unsafe {
            if result.is_null() {
                Kind::Failed(ns_error(error))
            } else {
                let best: Retained<AnyObject> = msg_send![result, bestTranscription];
                let text: Retained<NSString> = msg_send![&*best, formattedString];
                let is_final: bool = msg_send![result, isFinal];
                if is_final {
                    Kind::Final(text.to_string())
                } else {
                    Kind::Partial(text.to_string())
                }
            }
        };
        let _ = events.send(Event { task, kind });
    })
}

/// Transcribes an audio file on the device (used to test recognition without a
/// microphone).
///
/// # Errors
/// Permission, a missing on-device model, or a recognition failure.
pub fn transcribe_file(path: &std::path::Path, locale: &str) -> Result<String, Error> {
    let recognizer = recognizer(locale)?;
    let (tx, rx) = mpsc::channel();
    // SAFETY: framework messages on objects made here; the handler block is copied.
    let _task: Retained<AnyObject> = unsafe {
        let path = NSString::from_str(&path.to_string_lossy());
        let url: Retained<AnyObject> = msg_send![class!(NSURL), fileURLWithPath: &*path];
        let alloc: Allocated<AnyObject> = msg_send![class!(SFSpeechURLRecognitionRequest), alloc];
        let request: Retained<AnyObject> = msg_send![alloc, initWithURL: &*url];
        let _: () = msg_send![&*request, setRequiresOnDeviceRecognition: true];
        let _: () = msg_send![&*request, setAddsPunctuation: true];
        let handler = result_handler(0, tx);
        msg_send![&*recognizer, recognitionTaskWithRequest: &*request, resultHandler: &*handler]
    };
    loop {
        match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(Event {
                kind: Kind::Final(text),
                ..
            }) => return Ok(text),
            Ok(Event {
                kind: Kind::Failed(e),
                ..
            }) => return Err(Error::Framework(e)),
            Ok(_) => {}
            Err(_) => return Err(Error::Framework("recognition timed out".into())),
        }
    }
}

/// What the audio tap appends to: the current request, if listening.
struct Tap {
    request: Mutex<Option<Shared>>,
    paused: AtomicBool,
}

/// Continuous on-device recognition from the default microphone, one utterance at a
/// time. Use it from one thread.
pub struct Listener {
    recognizer: Retained<AnyObject>,
    engine: Retained<AnyObject>,
    tap: Arc<Tap>,
    task: Option<Retained<AnyObject>>,
    task_id: u64,
    contextual: Retained<NSArray<NSString>>,
    sender: Sender<Event>,
    events: Receiver<Event>,
    /// `LOADNGO_SPEECH_DEBUG=1` traces every recognition event to stderr.
    debug: bool,
}

impl Listener {
    /// Starts the microphone and recognition for `locale` (for example `en-US`).
    /// `contextual` phrases (names such as "Kimi") are favoured by the recognizer.
    ///
    /// # Errors
    /// Permission, no on-device model, or the audio engine failing to start.
    pub fn start(locale: &str, contextual: &[&str]) -> Result<Self, Error> {
        let recognizer = recognizer(locale)?;
        let tap = Arc::new(Tap {
            request: Mutex::new(None),
            paused: AtomicBool::new(false),
        });
        let tap_state = Arc::clone(&tap);
        let block = RcBlock::new(move |buffer: *mut AnyObject, _when: *mut AnyObject| {
            if tap_state.paused.load(Ordering::Relaxed) {
                return;
            }
            let request = tap_state
                .request
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(request) = request.as_ref() {
                // SAFETY: a live AVAudioPCMBuffer for this call, appended to a live
                // request (appending from the audio thread is the documented use).
                unsafe {
                    let _: () = msg_send![&*request.0, appendAudioPCMBuffer: buffer];
                }
            }
        });
        // SAFETY: AVAudioEngine setup on objects made here; the tap block is copied.
        let engine = unsafe {
            let engine: Retained<AnyObject> = msg_send![class!(AVAudioEngine), new];
            let input: Retained<AnyObject> = msg_send![&*engine, inputNode];
            let format: Retained<AnyObject> = msg_send![&*input, outputFormatForBus: 0usize];
            let _: () = msg_send![
                &*input,
                installTapOnBus: 0usize,
                bufferSize: 1024u32,
                format: &*format,
                block: &*block
            ];
            let _: () = msg_send![&*engine, prepare];
            let mut error: *mut AnyObject = std::ptr::null_mut();
            let ok: bool = msg_send![&*engine, startAndReturnError: &mut error];
            if !ok {
                return Err(Error::Framework(format!(
                    "microphone did not start: {}",
                    ns_error(error)
                )));
            }
            engine
        };
        let strings: Vec<Retained<NSString>> =
            contextual.iter().map(|s| NSString::from_str(s)).collect();
        let (sender, events) = mpsc::channel();
        let mut listener = Self {
            recognizer,
            engine,
            tap,
            task: None,
            task_id: 0,
            contextual: NSArray::from_retained_slice(&strings),
            sender,
            events,
            debug: std::env::var_os("LOADNGO_SPEECH_DEBUG").is_some(),
        };
        listener.begin();
        Ok(listener)
    }

    /// A fresh request and recognition task; the tap appends to it from now on.
    fn begin(&mut self) {
        self.task_id += 1;
        // SAFETY: framework messages on objects made here; the handler is copied.
        unsafe {
            let request: Retained<AnyObject> =
                msg_send![class!(SFSpeechAudioBufferRecognitionRequest), new];
            let _: () = msg_send![&*request, setShouldReportPartialResults: true];
            let _: () = msg_send![&*request, setRequiresOnDeviceRecognition: true];
            let _: () = msg_send![&*request, setAddsPunctuation: true];
            let _: () = msg_send![&*request, setTaskHint: TASK_HINT_DICTATION];
            let _: () = msg_send![&*request, setContextualStrings: &*self.contextual];
            let handler = result_handler(self.task_id, self.sender.clone());
            let task: Retained<AnyObject> = msg_send![
                &*self.recognizer,
                recognitionTaskWithRequest: &*request,
                resultHandler: &*handler
            ];
            self.task = Some(task);
            *self
                .tap
                .request
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(Shared(request));
        }
    }

    /// Stops feeding the current request; with `discard`, also cancels its task.
    fn end(&mut self, discard: bool) {
        let request = self
            .tap
            .request
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        // SAFETY: live framework objects owned here.
        unsafe {
            if let Some(request) = request {
                let _: () = msg_send![&*request.0, endAudio];
            }
            if discard {
                if let Some(task) = self.task.take() {
                    let _: () = msg_send![&*task, cancel];
                }
                // Events of the cancelled task are ignored from here on.
                self.task_id += 1;
            }
        }
    }

    /// Waits for the next utterance: partial results until `silence` passes with no
    /// change, then the final transcription. Returns an empty string for a task that
    /// ended without speech.
    ///
    /// # Errors
    /// The event channel closing, which only happens if the framework drops the task.
    pub fn next_utterance(&mut self, silence: Duration) -> Result<String, Error> {
        let mut last = String::new();
        let mut deadline: Option<Instant> = None;
        let mut ending = false;
        loop {
            let event = match deadline {
                None => self
                    .events
                    .recv()
                    .map_err(|e| Error::Framework(e.to_string()))?,
                Some(at) => match self
                    .events
                    .recv_timeout(at.saturating_duration_since(Instant::now()))
                {
                    Ok(event) => event,
                    Err(RecvTimeoutError::Timeout) if !ending => {
                        // Silence: finish this utterance; the final result follows.
                        self.end(false);
                        ending = true;
                        deadline = Some(Instant::now() + Duration::from_secs(3));
                        continue;
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        self.end(true);
                        self.begin();
                        return Ok(last);
                    }
                    Err(e) => return Err(Error::Framework(e.to_string())),
                },
            };
            if self.debug {
                let what = match &event.kind {
                    Kind::Partial(t) => format!("partial {t:?}"),
                    Kind::Final(t) => format!("final {t:?}"),
                    Kind::Failed(e) => format!("failed {e}"),
                };
                eprintln!(
                    "[speech task {} (current {})] {what}",
                    event.task, self.task_id
                );
            }
            if event.task != self.task_id {
                continue;
            }
            match event.kind {
                Kind::Partial(text) => {
                    if text != last {
                        last = text;
                        if !ending {
                            deadline = Some(Instant::now() + silence);
                        }
                    }
                }
                Kind::Final(text) => {
                    self.task = None;
                    self.begin();
                    return Ok(text);
                }
                Kind::Failed(_) => {
                    // No speech, or the task ended: start again, keeping what was heard.
                    self.task = None;
                    self.begin();
                    if !last.is_empty() {
                        return Ok(last);
                    }
                    deadline = None;
                    ending = false;
                }
            }
        }
    }

    /// Stops listening (for example while speaking), discarding anything half-heard.
    pub fn pause(&mut self) {
        self.tap.paused.store(true, Ordering::Relaxed);
        self.end(true);
    }

    /// Listens again after [`Listener::pause`].
    pub fn resume(&mut self) {
        // Drop anything that arrived while paused.
        while self.events.try_recv().is_ok() {}
        self.tap.paused.store(false, Ordering::Relaxed);
        self.begin();
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.end(true);
        // SAFETY: live engine objects owned here.
        unsafe {
            let input: Retained<AnyObject> = msg_send![&*self.engine, inputNode];
            let _: () = msg_send![&*input, removeTapOnBus: 0usize];
            let _: () = msg_send![&*self.engine, stop];
        }
    }
}

struct DelegateIvars {
    done: Sender<()>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and `SpeakerDelegate` has no Drop.
    #[unsafe(super(NSObject))]
    #[name = "LoadngoSpeakerDelegate"]
    #[ivars = DelegateIvars]
    struct SpeakerDelegate;

    impl SpeakerDelegate {
        // SAFETY: AVSpeechSynthesizerDelegate's signatures.
        #[unsafe(method(speechSynthesizer:didFinishSpeechUtterance:))]
        fn did_finish(&self, _synthesizer: *mut AnyObject, _utterance: *mut AnyObject) {
            let _ = self.ivars().done.send(());
        }

        #[unsafe(method(speechSynthesizer:didCancelSpeechUtterance:))]
        fn did_cancel(&self, _synthesizer: *mut AnyObject, _utterance: *mut AnyObject) {
            let _ = self.ivars().done.send(());
        }
    }
);

/// Speaks text with the system's voice for a language, waiting until it has finished.
pub struct Speaker {
    synthesizer: Retained<AnyObject>,
    _delegate: Retained<SpeakerDelegate>,
    voice: Option<Retained<AnyObject>>,
    done: Receiver<()>,
}

impl Speaker {
    /// A speaker for `language` (for example `en-US`), with the system's default voice
    /// for it.
    pub fn new(language: &str) -> Self {
        let (tx, done) = mpsc::channel();
        let delegate = SpeakerDelegate::alloc().set_ivars(DelegateIvars { done: tx });
        // SAFETY: NSObject's init on a freshly allocated instance; then framework
        // messages on objects made here. The synthesizer keeps a weak delegate, which
        // `_delegate` keeps alive.
        unsafe {
            let delegate: Retained<SpeakerDelegate> = msg_send![super(delegate), init];
            let synthesizer: Retained<AnyObject> = msg_send![class!(AVSpeechSynthesizer), new];
            let _: () = msg_send![&*synthesizer, setDelegate: &*delegate];
            let language = NSString::from_str(language);
            let voice: Option<Retained<AnyObject>> =
                msg_send![class!(AVSpeechSynthesisVoice), voiceWithLanguage: &*language];
            Self {
                synthesizer,
                _delegate: delegate,
                voice,
                done,
            }
        }
    }

    /// Speaks `text` and returns when it has been spoken (or after a generous bound
    /// based on its length, if the synthesizer never reports back).
    ///
    /// # Errors
    /// When speech did not finish within that bound.
    pub fn speak(&self, text: &str) -> Result<(), Error> {
        if text.trim().is_empty() {
            return Ok(());
        }
        while self.done.try_recv().is_ok() {}
        // SAFETY: framework messages on objects made here.
        unsafe {
            let string = NSString::from_str(text);
            let utterance: Retained<AnyObject> =
                msg_send![class!(AVSpeechUtterance), speechUtteranceWithString: &*string];
            if let Some(voice) = &self.voice {
                let _: () = msg_send![&*utterance, setVoice: &**voice];
            }
            let _: () = msg_send![&*self.synthesizer, speakUtterance: &*utterance];
        }
        // About 15 characters a second, plus slack.
        let bound = Duration::from_secs(10 + text.len() as u64 / 5);
        wait_with_run_loop(&self.done, bound)
            .ok_or_else(|| Error::Framework("speech did not finish".into()))
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        // SAFETY: a live synthesizer owned here.
        unsafe {
            let _: bool = msg_send![&*self.synthesizer, stopSpeakingAtBoundary: 0isize];
        }
    }
}
