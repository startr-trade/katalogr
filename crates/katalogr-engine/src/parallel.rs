//! How the processor uses threads (FEAT-072, ADR-0027).

use std::num::NonZeroUsize;

/// How the processor uses threads. `threads == 1` means fully sequential: no pool is built and
/// every stage runs on the calling thread in batch order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parallelism {
    pub threads: NonZeroUsize,
    /// Cap on the encoded shard bytes a round holds in memory before the stitch (R-4).
    pub batch_bytes: u64,
}

impl Parallelism {
    /// 64 MiB: about 7x a measured 256-file round (mean shard 34 KB on the self-index).
    pub const DEFAULT_BATCH_BYTES: u64 = 64 * 1024 * 1024;

    /// One thread, default batch bytes.
    pub fn sequential() -> Parallelism {
        Parallelism {
            threads: NonZeroUsize::MIN,
            batch_bytes: Self::DEFAULT_BATCH_BYTES,
        }
    }

    /// `available_parallelism()` threads (1 when unknown), default batch bytes.
    pub fn from_cores() -> Parallelism {
        Parallelism {
            threads: std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN),
            batch_bytes: Self::DEFAULT_BATCH_BYTES,
        }
    }
}

/// Items per wave: `threads * WAVES_PER_THREAD`, so 1 for a sequential run.
pub const WAVES_PER_THREAD: usize = 4;

pub fn wave_size(threads: NonZeroUsize) -> usize {
    if threads.get() == 1 {
        1
    } else {
        threads.get() * WAVES_PER_THREAD
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_size_per_threads() {
        let n = |v| NonZeroUsize::new(v).unwrap();
        assert_eq!(wave_size(n(1)), 1);
        assert_eq!(wave_size(n(2)), 8);
        assert_eq!(wave_size(n(12)), 48);
        assert_eq!(Parallelism::sequential().threads.get(), 1);
        assert!(Parallelism::from_cores().threads.get() >= 1);
    }
}
