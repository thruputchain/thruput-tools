//! Off-chain messaging between agents.
//!
//! Agents communicate only through harness-provided channels, never raw I/O,
//! so the harness stays in a position to observe and (later) order their
//! interactions. Messages are opaque bytes; a typed layer can be built on top.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::error::{Result, TestkitError};

/// A message delivered to an agent's mailbox.
#[derive(Debug, Clone)]
pub struct Envelope {
    /// Name of the agent that sent the message.
    pub from: String,
    /// Opaque message payload.
    pub bytes: Vec<u8>,
}

/// An agent's inbound queue plus send handles to its peers.
#[derive(Debug)]
pub struct Mailbox {
    name: Arc<str>,
    inbox: mpsc::UnboundedReceiver<Envelope>,
    peers: HashMap<String, mpsc::UnboundedSender<Envelope>>,
}

impl Mailbox {
    pub(crate) fn new(
        name: Arc<str>,
        inbox: mpsc::UnboundedReceiver<Envelope>,
        peers: HashMap<String, mpsc::UnboundedSender<Envelope>>,
    ) -> Self {
        Self { name, inbox, peers }
    }

    /// Send `bytes` to the agent named `to`.
    ///
    /// Fails with [`TestkitError::UnknownAgent`] when no such agent exists, or
    /// [`TestkitError::MailboxClosed`] when the recipient has already finished.
    pub fn send_to(&self, to: &str, bytes: impl Into<Vec<u8>>) -> Result<()> {
        let sender = self
            .peers
            .get(to)
            .ok_or_else(|| TestkitError::UnknownAgent {
                name: to.to_owned(),
            })?;
        sender
            .send(Envelope {
                from: self.name.to_string(),
                bytes: bytes.into(),
            })
            .map_err(|_| TestkitError::MailboxClosed {
                name: to.to_owned(),
            })
    }

    /// Wait for the next message, or `None` once every peer has finished.
    pub async fn recv(&mut self) -> Option<Envelope> {
        self.inbox.recv().await
    }

    /// Take a pending message without waiting.
    pub fn try_recv(&mut self) -> Option<Envelope> {
        self.inbox.try_recv().ok()
    }
}
