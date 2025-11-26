use anyhow::{anyhow, Result};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

pub trait Work: Send + 'static {
    fn run(self: Box<Self>, key: usize, size: u32);
}

enum Command {
    Job {
        key: usize,
        size: u32,
        work: Box<dyn Work>,
    },
    Stop,
}

pub struct Machine {
    sender: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl Machine {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Command>();
        let thread = thread::spawn(move || {
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    Command::Job { key, size, work } => work.run(key, size),
                    Command::Stop => break,
                }
            }
        });
        Self {
            sender: tx,
            thread: Some(thread),
        }
    }

    pub fn enqueue<W>(&self, work: W, key: usize) -> Result<()>
    where
        W: Work,
    {
        self.enqueue_boxed(Box::new(work), key, 0)
    }

    pub fn enqueue_boxed(&self, work: Box<dyn Work>, key: usize, size: u32) -> Result<()> {
        self.sender
            .send(Command::Job { key, size, work })
            .map_err(|_| anyhow!("machine work queue is closed"))
    }

    pub fn enqueue_fn<F>(&self, key: usize, size: u32, work: F) -> Result<()>
    where
        F: FnOnce(usize, u32) + Send + 'static,
    {
        self.enqueue_boxed(Box::new(FnWork::new(work)), key, size)
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct FnWork<F>
where
    F: FnOnce(usize, u32) + Send + 'static,
{
    inner: Option<F>,
}

impl<F> FnWork<F>
where
    F: FnOnce(usize, u32) + Send + 'static,
{
    fn new(inner: F) -> Self {
        Self { inner: Some(inner) }
    }
}

impl<F> Work for FnWork<F>
where
    F: FnOnce(usize, u32) + Send + 'static,
{
    fn run(mut self: Box<Self>, key: usize, size: u32) {
        if let Some(inner) = self.inner.take() {
            inner(key, size);
        }
    }
}
