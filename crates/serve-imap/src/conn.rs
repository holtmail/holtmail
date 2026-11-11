//! Drives `imap-next`'s server state machine over a connection.

use std::collections::VecDeque;
use std::io;
use std::net::IpAddr;

use async_trait::async_trait;
use holtmail_core::UserId;
use imap_next::server::{Error as ServerError, Event, Options, Server};
use imap_next::{Interrupt, Io, State};
use imap_types::response::{CommandContinuationRequest, Data, Greeting, Status};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// A client connection as the session sees it.
#[async_trait]
pub(crate) trait Wire: AsyncRead + AsyncWrite + Unpin + Send {
    /// Whether `STARTTLS` can succeed now.
    fn can_start_tls(&self) -> bool;
    /// Turns the connection into a TLS one; false when the handshake failed and the connection
    /// is unusable.
    async fn start_tls(&mut self) -> bool;
    /// Whether a password may be sent: encrypted, or from this machine.
    fn sign_in_allowed(&self) -> bool;
    /// The client's address, for the sign-in limits.
    fn client(&self) -> IpAddr;
    /// Records the sign-in in the connection limits; false over the per-user limit.
    fn authenticated(&mut self, user: &UserId) -> bool;
}

/// Something the client sent.
#[derive(Debug)]
pub(crate) enum Incoming {
    /// A command or a step of an exchange.
    Event(Box<Event>),
    /// Bytes that could not be understood, already discarded.
    Error(ServerError),
}

/// The connection with its protocol state.
pub(crate) struct Conn<W> {
    server: Server,
    pub(crate) wire: W,
    input: Vec<u8>,
    pending: VecDeque<Incoming>,
    by_line: bool,
    eof: bool,
    unfinished: usize,
}

const READ_CHUNK: usize = 16 * 1024;

impl<W: Wire> Conn<W> {
    pub(crate) fn new(wire: W, options: Options, greeting: Greeting<'static>) -> Self {
        let by_line = wire.can_start_tls();
        Self {
            server: Server::new(options, greeting),
            wire,
            input: Vec::new(),
            pending: VecDeque::new(),
            by_line,
            eof: false,
            unfinished: 0,
        }
    }

    pub(crate) fn data(&mut self, data: Data<'static>) {
        self.server.enqueue_data(data);
    }

    pub(crate) fn status(&mut self, status: Status<'static>) {
        self.server.enqueue_status(status);
    }

    pub(crate) fn authenticate_continue(&mut self, request: CommandContinuationRequest<'static>) {
        let _ = self.server.authenticate_continue(request);
    }

    pub(crate) fn authenticate_finish(&mut self, status: Status<'static>) {
        if let Err(status) = self.server.authenticate_finish(status) {
            self.server.enqueue_status(status);
        }
    }

    pub(crate) fn idle_accept(&mut self, request: CommandContinuationRequest<'static>) {
        let _ = self.server.idle_accept(request);
    }

    pub(crate) fn idle_reject(&mut self, status: Status<'static>) {
        if let Err(status) = self.server.idle_reject(status) {
            self.server.enqueue_status(status);
        }
    }

    /// Writes everything queued. Commands the client sent meanwhile wait for
    /// [`Conn::step`].
    pub(crate) async fn flush(&mut self) -> io::Result<()> {
        loop {
            match self.server.next() {
                Ok(Event::GreetingSent { .. } | Event::ResponseSent { .. }) => {}
                Ok(event) => {
                    self.unfinished = 0;
                    self.pending.push_back(Incoming::Event(Box::new(event)));
                }
                Err(Interrupt::Io(Io::Output(bytes))) => self.wire.write_all(&bytes).await?,
                Err(Interrupt::Io(Io::NeedMoreInput)) => break,
                Err(Interrupt::Error(error)) => self.pending.push_back(Incoming::Error(error)),
            }
        }
        self.wire.flush().await
    }

    /// The next thing the client sent from what was read so far, writing queued responses on
    /// the way; `None` when more must be read.
    pub(crate) async fn step(&mut self) -> io::Result<Option<Incoming>> {
        if let Some(incoming) = self.pending.pop_front() {
            return Ok(Some(incoming));
        }
        loop {
            match self.server.next() {
                Ok(Event::GreetingSent { .. } | Event::ResponseSent { .. }) => {}
                Ok(event) => {
                    self.unfinished = 0;
                    return Ok(Some(Incoming::Event(Box::new(event))));
                }
                Err(Interrupt::Io(Io::Output(bytes))) => self.wire.write_all(&bytes).await?,
                Err(Interrupt::Io(Io::NeedMoreInput)) => {
                    if !self.feed() {
                        self.wire.flush().await?;
                        return Ok(None);
                    }
                }
                Err(Interrupt::Error(error)) => {
                    self.unfinished = 0;
                    return Ok(Some(Incoming::Error(error)));
                }
            }
        }
    }

    /// Bytes handed to the state machine since the last complete command, exchange step or
    /// discarded input: what it holds for a command still arriving.
    pub(crate) fn unfinished(&self) -> usize {
        self.unfinished
    }

    /// Hands buffered input to the state machine. Before TLS on a connection that may still
    /// start it, input goes one line at a time, so nothing sent after `STARTTLS` in the clear
    /// is ever inside the state machine when the connection is upgraded.
    fn feed(&mut self) -> bool {
        if self.input.is_empty() {
            return false;
        }
        let take = if self.by_line {
            self.input
                .iter()
                .position(|b| *b == b'\n')
                .map_or(self.input.len(), |at| at + 1)
        } else {
            self.input.len()
        };
        let chunk: Vec<u8> = self.input.drain(..take).collect();
        self.unfinished = self.unfinished.saturating_add(chunk.len());
        self.server.enqueue_input(&chunk);
        true
    }

    /// Reads once from the client. Safe to cancel. False at the end of the stream.
    pub(crate) async fn read_more(&mut self) -> io::Result<bool> {
        if self.eof {
            return Ok(false);
        }
        let mut buf = [0u8; READ_CHUNK];
        let n = self.wire.read(&mut buf).await?;
        if n == 0 {
            self.eof = true;
            return Ok(false);
        }
        self.input.extend_from_slice(&buf[..n]);
        Ok(true)
    }

    /// Upgrades to TLS after the `OK` to `STARTTLS` was written. Anything the client sent in
    /// the clear after the command is dropped.
    pub(crate) async fn start_tls(&mut self) -> bool {
        self.input.clear();
        self.by_line = false;
        self.wire.start_tls().await
    }
}
