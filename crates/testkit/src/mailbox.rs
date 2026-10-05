//! Off-chain messaging between agents.
//!
//! Agents communicate only through harness-provided channels, never raw I/O,
//! so the harness stays in a position to observe and (later) order their
//! interactions. The [`Mailbox`] transport carries opaque bytes; the typed
//! layer lives on `AgentContext::send`/`recv`, which CXS-encode and decode the
//! payload and hand back a [`Message`].

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use tokio::sync::mpsc;
use xdrs::ReadXDR;

use crate::error::{Result, TestkitError};

/// A raw message delivered to an agent's mailbox.
#[derive(Debug, Clone)]
pub struct Envelope {
    /// Name of the agent that sent the message.
    pub from: String,
    /// Opaque message payload.
    pub bytes: Vec<u8>,
}

impl Envelope {
    /// Decode this envelope's CXS payload as `T`, keeping the sender's name.
    pub(crate) fn decode<T>(self) -> Result<Message<T>>
    where
        T: for<'r> ReadXDR<'r>,
    {
        let body = xdrs::cxs::decode::<T>(&self.bytes).map_err(TestkitError::decode)?;
        Ok(Message {
            from: self.from,
            body,
        })
    }
}

/// A received message with its CXS payload decoded to `T`.
#[derive(Debug, Clone)]
pub struct Message<T> {
    /// Name of the agent that sent the message.
    pub from: String,
    /// The decoded message payload.
    pub body: T,
}

/// An agent's inbound queue plus send handles to its peers.
///
/// `cache` holds messages already pulled off the channel but not yet withdrawn
/// — set aside by a sender-selective [`recv_from`](Mailbox::recv_from) that
/// skipped past them. They are the oldest messages, so plain
/// [`recv`](Mailbox::recv) drains the cache before the channel.
#[derive(Debug)]
pub struct Mailbox {
    name: Arc<str>,
    inbox: mpsc::UnboundedReceiver<Envelope>,
    peers: HashMap<String, mpsc::UnboundedSender<Envelope>>,
    cache: VecDeque<Envelope>,
}

impl Mailbox {
    pub(crate) fn new(
        name: Arc<str>,
        inbox: mpsc::UnboundedReceiver<Envelope>,
        peers: HashMap<String, mpsc::UnboundedSender<Envelope>>,
    ) -> Self {
        Self {
            name,
            inbox,
            peers,
            cache: VecDeque::new(),
        }
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

    /// Wait for the next message from any sender, or `None` once every peer has
    /// finished. Cached messages (set aside by [`recv_from`](Self::recv_from))
    /// come first, oldest to newest, before anything still on the channel.
    pub async fn recv(&mut self) -> Option<Envelope> {
        if let Some(envelope) = self.cache.pop_front() {
            return Some(envelope);
        }
        self.inbox.recv().await
    }

    /// Take a pending message from any sender without waiting.
    pub fn try_recv(&mut self) -> Option<Envelope> {
        if let Some(envelope) = self.cache.pop_front() {
            return Some(envelope);
        }
        self.inbox.try_recv().ok()
    }

    /// Wait for the next message from any of the agents named in `from`, or
    /// `None` once every peer has finished without one arriving.
    ///
    /// Messages from senders outside the set that arrive first are cached in
    /// order and remain available to later [`recv`](Self::recv) / `recv_from*`
    /// calls, so selecting by sender does not drop anyone else's messages.
    pub async fn recv_from_any(&mut self, from: &[&str]) -> Option<Envelope> {
        if let Some(index) = self.cache.iter().position(|e| sender_in(from, e)) {
            return self.cache.remove(index);
        }
        loop {
            match self.inbox.recv().await {
                Some(envelope) if sender_in(from, &envelope) => return Some(envelope),
                Some(envelope) => self.cache.push_back(envelope),
                None => return None,
            }
        }
    }

    /// Take a pending message from any of the agents named in `from` without
    /// waiting. Non-matching messages already waiting are cached, not dropped.
    pub fn try_recv_from_any(&mut self, from: &[&str]) -> Option<Envelope> {
        if let Some(index) = self.cache.iter().position(|e| sender_in(from, e)) {
            return self.cache.remove(index);
        }
        loop {
            match self.inbox.try_recv() {
                Ok(envelope) if sender_in(from, &envelope) => return Some(envelope),
                Ok(envelope) => self.cache.push_back(envelope),
                Err(_) => return None,
            }
        }
    }

    /// Wait for the next message from the agent named `from`, or `None` once
    /// every peer has finished without one arriving.
    pub async fn recv_from(&mut self, from: &str) -> Option<Envelope> {
        self.recv_from_any(&[from]).await
    }

    /// Take a pending message from the agent named `from` without waiting.
    pub fn try_recv_from(&mut self, from: &str) -> Option<Envelope> {
        self.try_recv_from_any(&[from])
    }
}

/// Whether an envelope's sender is one of `senders`.
fn sender_in(senders: &[&str], envelope: &Envelope) -> bool {
    senders.iter().any(|s| envelope.from == *s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(from: &str, byte: u8) -> Envelope {
        Envelope {
            from: from.to_owned(),
            bytes: vec![byte],
        }
    }

    #[test]
    fn recv_from_selects_a_sender_and_caches_the_rest() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut mailbox = Mailbox::new(Arc::from("bob"), rx, HashMap::new());

        // Alice's message arrives first, then carol's.
        tx.send(envelope("alice", 1)).expect("send alice");
        tx.send(envelope("carol", 2)).expect("send carol");

        // Selecting carol skips past alice's message without dropping it.
        let carol = mailbox.try_recv_from("carol").expect("carol's message");
        assert_eq!(carol.from, "carol");
        assert_eq!(carol.bytes, vec![2]);

        // Alice's cached message is the next one a plain recv sees.
        let alice = mailbox.try_recv().expect("alice's cached message");
        assert_eq!(alice.from, "alice");
        assert_eq!(alice.bytes, vec![1]);

        // Nothing left.
        assert!(mailbox.try_recv().is_none());
    }

    #[test]
    fn recv_from_returns_none_when_no_matching_sender_remains() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut mailbox = Mailbox::new(Arc::from("bob"), rx, HashMap::new());

        tx.send(envelope("alice", 1)).expect("send alice");

        // No message from carol is pending; alice's is cached, not consumed.
        assert!(mailbox.try_recv_from("carol").is_none());
        let alice = mailbox.try_recv().expect("alice's cached message");
        assert_eq!(alice.from, "alice");
    }

    #[test]
    fn recv_from_any_accepts_any_member_and_caches_outsiders() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut mailbox = Mailbox::new(Arc::from("admin"), rx, HashMap::new());

        // A stranger's message arrives before the two providers'.
        tx.send(envelope("stranger", 9)).expect("send stranger");
        tx.send(envelope("bob", 2)).expect("send bob");
        tx.send(envelope("alice", 1)).expect("send alice");

        // Selecting the provider set skips the stranger and yields providers in
        // arrival order.
        let first = mailbox
            .try_recv_from_any(&["alice", "bob"])
            .expect("first provider");
        assert_eq!(first.from, "bob");
        let second = mailbox
            .try_recv_from_any(&["alice", "bob"])
            .expect("second provider");
        assert_eq!(second.from, "alice");

        // The stranger's message was cached, not dropped.
        let stranger = mailbox.try_recv().expect("stranger's cached message");
        assert_eq!(stranger.from, "stranger");
    }
}
