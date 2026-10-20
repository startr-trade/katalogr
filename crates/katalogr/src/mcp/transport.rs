//! The input side of the stdio transport: a reader that caps the length of a line.
//!
//! rmcp frames newline-delimited JSON-RPC and has no line limit, so a hostile or broken
//! client could make it buffer without bound. [`CappedLines`] sits between stdin and rmcp and
//! replaces an oversized line with a well-formed non-message, which rmcp answers with
//! `-32600` while memory stays bounded (R-11).

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

/// The longest line passed on, in bytes, excluding its `\n`. The largest legitimate request
/// is a few hundred bytes.
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// The bytes substituted for an oversized line: ends the cut line, then a well-formed
/// non-message that rmcp answers with -32600.
pub const OVERSIZE_REPLACEMENT: &[u8] = b"\n{}\n";

/// The state of the byte filter behind [`CappedLines`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CapState {
    /// The longest line passed on.
    pub cap: usize,
    /// Bytes of the current line passed on so far.
    pub run: usize,
    /// The rest of an oversized line is being dropped, up to its `\n`.
    pub discarding: bool,
}

/// The pure byte filter: append to `out` what `chunk` becomes. A line longer than the cap is
/// cut at the cap, followed by [`OVERSIZE_REPLACEMENT`]; the rest of that line is dropped up
/// to and including its `\n`.
pub fn filter(state: &mut CapState, chunk: &[u8], out: &mut VecDeque<u8>) {
    for &b in chunk {
        if state.discarding {
            if b == b'\n' {
                state.discarding = false;
                state.run = 0;
            }
        } else if b == b'\n' {
            out.push_back(b);
            state.run = 0;
        } else if state.run == state.cap {
            out.extend(OVERSIZE_REPLACEMENT);
            state.discarding = true;
        } else {
            out.push_back(b);
            state.run += 1;
        }
    }
}

/// An `AsyncRead` that forwards its input unchanged except that a line longer than the cap
/// is cut (see [`filter`]). Memory: one 8 KiB chunk plus the queue of one chunk's output.
pub struct CappedLines<R> {
    inner: R,
    state: CapState,
    out: VecDeque<u8>,
}

impl<R> CappedLines<R> {
    pub fn new(inner: R, cap: usize) -> Self {
        CappedLines {
            inner,
            state: CapState {
                cap,
                run: 0,
                discarding: false,
            },
            out: VecDeque::new(),
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for CappedLines<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        loop {
            if buf.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
            if !this.out.is_empty() {
                let n = this.out.len().min(buf.remaining());
                let bytes: Vec<u8> = this.out.drain(..n).collect();
                buf.put_slice(&bytes);
                return Poll::Ready(Ok(()));
            }
            let mut chunk = [0u8; 8192];
            let mut read = ReadBuf::new(&mut chunk);
            match Pin::new(&mut this.inner).poll_read(cx, &mut read) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => {}
            }
            let filled = read.filled();
            if filled.is_empty() {
                // End of input.
                return Poll::Ready(Ok(()));
            }
            filter(&mut this.state, filled, &mut this.out);
            // Everything may have been discarded: read again, never return 0 bytes before EOF.
        }
    }
}
