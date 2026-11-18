//! Progress/event plumbing shared by the CLI and the GUI.

use std::sync::mpsc::Sender;

#[derive(Clone, Debug)]
pub enum Event {
    /// Free-form log line (also mirrored to the console by the CLI).
    Log(String),
    /// A new stage of work started.
    Stage(String),
    /// Byte-level progress of the current stage.
    Progress { label: String, done: u64, total: u64 },
    /// Microsoft device-code flow needs the user to open a URL.
    DeviceCode {
        user_code: String,
        verification_uri: String,
        message: String,
    },
    /// Something finished successfully.
    Done(String),
    /// Something failed.
    Failed(String),
}

#[derive(Clone)]
pub struct ProgressSender {
    inner: Option<Sender<Event>>,
}

impl ProgressSender {
    pub fn new(tx: Sender<Event>) -> Self {
        Self { inner: Some(tx) }
    }

    /// A sink that drops every event (useful in tests / quiet installs).
    pub fn silent() -> Self {
        Self { inner: None }
    }

    /// Pair used by the GUI/CLI: the sender to hand out plus the receiver to poll.
    pub fn channel() -> (Self, std::sync::mpsc::Receiver<Event>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Self::new(tx), rx)
    }

    pub fn send(&self, ev: Event) {
        if let Some(tx) = &self.inner {
            let _ = tx.send(ev);
        }
    }

    pub fn log(&self, msg: impl Into<String>) {
        self.send(Event::Log(msg.into()));
    }

    pub fn stage(&self, msg: impl Into<String>) {
        self.send(Event::Stage(msg.into()));
    }

    pub fn progress(&self, label: impl Into<String>, done: u64, total: u64) {
        self.send(Event::Progress {
            label: label.into(),
            done,
            total,
        });
    }

    pub fn done(&self, msg: impl Into<String>) {
        self.send(Event::Done(msg.into()));
    }

    pub fn failed(&self, msg: impl Into<String>) {
        self.send(Event::Failed(msg.into()));
    }
}