//! Unit tests of `mcp::transport`; compiled as the crate-level module `transport` so they print as
//! `transport::<test>`.

use crate::mcp::transport::*;
use std::collections::VecDeque;

fn run(cap: usize, input: &[u8], chunk: usize) -> Vec<u8> {
    let mut state = CapState {
        cap,
        run: 0,
        discarding: false,
    };
    let mut out = VecDeque::new();
    for c in input.chunks(chunk) {
        filter(&mut state, c, &mut out);
    }
    out.into_iter().collect()
}

/// R-11. A line over the cap is cut and replaced, wherever the chunk boundaries fall.
#[test]
fn caps_oversized_line() {
    for chunk in [1, 3, 64] {
        assert_eq!(
            run(8, b"0123456789abc\nok\n", chunk),
            b"01234567\n{}\nok\n",
            "chunk {chunk}"
        );
    }
    // A line of exactly the cap passes.
    assert_eq!(run(8, b"01234567\nx\n", 4), b"01234567\nx\n");
    // An oversized last line without a newline is still cut once.
    assert_eq!(run(2, b"abcdef", 2), b"ab\n{}\n");
}

/// R-11. Lines within the cap pass byte for byte.
#[test]
fn passes_lines_unchanged() {
    let input: &[u8] = b"{\"a\":1}\r\n\n\nlast without newline";
    for chunk in [1, 5, 64] {
        assert_eq!(run(64, input, chunk), input, "chunk {chunk}");
    }
}

#[test]
fn reader_filters_through_poll_read() {
    use tokio::io::AsyncReadExt;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let got = rt.block_on(async {
        let input: &[u8] = b"0123456789abc\nok\n";
        let mut r = CappedLines::new(input, 8);
        let mut v = Vec::new();
        r.read_to_end(&mut v).await.unwrap();
        v
    });
    assert_eq!(got, b"01234567\n{}\nok\n");
}
