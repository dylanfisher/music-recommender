//! The HTTP API the player talks to: a small HTTP/1.1 server on `std::net` (ADR 0008), the
//! bearer-token check, size caps on what it reads, and the routes in `docs/api.md`; and the
//! owner's status page, `GET /status`, signed in with HTTP Basic (ADR 0021). The downloads'
//! job shape and snapshot reader are public for the commands, which print and read the same
//! JSON.
//!
//! Each connection carries one request and one reply, then closes. A thread serves each
//! connection, up to [`Limits::connections`] at once.

use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use config::Config;
use features::clock::Clock;

mod body;
pub mod downloads;
mod profile;
mod reply;
mod request;
mod route;
mod status;
mod token;

use reply::{Refusal, Reply};
use request::{Deadline, ReadError};

/// How much of a refused request is read and thrown away before closing, so the client
/// reads the refusal instead of a reset over unread bytes.
const MAX_DRAIN: u64 = 1024 * 1024;

/// How long a refusal for a full server waits on the client. Short, because the accept loop
/// waits with it.
const BUSY_WAIT: Duration = Duration::from_millis(50);

/// How long to wait after the listener fails to accept, for example with no file
/// descriptors left, before trying again.
const ACCEPT_RETRY: Duration = Duration::from_secs(1);

/// What the server allows each client.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Connections served at once; one more is refused with a 503.
    pub connections: usize,
    /// The time from accepting a connection to having read its whole request. A client
    /// still sending after it is refused with a 408, however it spaces its bytes.
    pub read_deadline: Duration,
    /// The largest request body read, in bytes; a larger one is refused with a 400.
    pub max_body: usize,
    /// The largest body `POST /v1/profile` reads, in bytes, sent with the player token.
    pub max_profile_body: usize,
}

/// The limits `serve` runs with.
pub const LIMITS: Limits = Limits {
    connections: 16,
    read_deadline: Duration::from_secs(10),
    // A release snapshot is a few KB.
    max_body: 256 * 1024,
    max_profile_body: features::profile::MAX_BYTES,
};

/// Serves the API on `listener` forever. The status page reads the time from `clock`.
pub fn serve(
    listener: &TcpListener,
    config: &Arc<Config>,
    clock: &Arc<dyn Clock>,
    limits: &Limits,
) -> ! {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) => {
                eprintln!("error: the API couldn't accept a connection: {error}");
                thread::sleep(ACCEPT_RETRY);
                continue;
            }
        };
        // Only this loop adds to the count, so reading and then adding can't race past the
        // cap; the threads only take away.
        if active.load(Ordering::Acquire) >= limits.connections {
            refuse_busy(&stream);
            continue;
        }
        active.fetch_add(1, Ordering::AcqRel);
        let slot = Slot(Arc::clone(&active));
        let config = Arc::clone(config);
        let clock = Arc::clone(clock);
        let limits = *limits;
        thread::spawn(move || {
            let _slot = slot;
            answer(&stream, &config, clock.as_ref(), &limits);
        });
    }
}

/// A connection's place in the count, given back when its thread ends, panics included.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Reads one request from `stream`, answers it and closes.
fn answer(stream: &TcpStream, config: &Config, clock: &dyn Clock, limits: &Limits) {
    let until = Instant::now() + limits.read_deadline;
    let mut reader = BufReader::new(Deadline::new(stream, until));
    // The profile is the one body larger than `max_body`, and only a client with the token
    // gets its cap, so no one else can make a thread hold 32 MB.
    let max_body = |path: &str, authorization: Option<&str>| {
        if path != route::PROFILE {
            Ok(limits.max_body)
        } else if token::authorizes(authorization, config.player_token.secret()) {
            Ok(limits.max_profile_body)
        } else {
            Err(Refusal::Unauthorized)
        }
    };
    let (reply, unread) = match request::read(&mut reader, max_body) {
        Ok(request) => (route::answer(&request, config, clock), false),
        // The client went away before sending a whole request: nobody to answer.
        Err(ReadError::Closed) => return,
        Err(ReadError::Refused { refusal, unread }) => (Reply::from(refusal), unread),
    };
    // A client that stopped reading would otherwise hold the thread forever.
    if stream
        .set_write_timeout(Some(limits.read_deadline))
        .is_err()
    {
        return;
    }
    // A write fails only when the client has gone, which is the client's business, not an
    // error of the service's.
    let _ = finish(stream, &reply, unread.then_some(&mut reader));
}

/// Writes `reply` and, for a request not read to its end, reads what's left of it until
/// the client closes, so its last bytes don't turn the close into a reset.
fn finish(mut stream: &TcpStream, reply: &Reply, unread: Option<&mut impl Read>) -> io::Result<()> {
    reply.write(&mut stream)?;
    stream.flush()?;
    if let Some(reader) = unread {
        stream.shutdown(Shutdown::Write)?;
        io::copy(&mut reader.take(MAX_DRAIN), &mut io::sink())?;
    }
    Ok(())
}

/// Answers a connection over the cap with a 503 from the accept loop, waiting on the
/// client at most `BUSY_WAIT` for each step.
fn refuse_busy(stream: &TcpStream) {
    let until = Instant::now() + BUSY_WAIT;
    if stream.set_write_timeout(Some(BUSY_WAIT)).is_err() {
        return;
    }
    // As in `answer`, a failed write means the client has gone.
    let _ = finish(
        stream,
        &Reply::from(Refusal::Busy),
        Some(&mut Deadline::new(stream, until)),
    );
}
