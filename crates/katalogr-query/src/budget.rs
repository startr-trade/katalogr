//! Token budgets. The estimate is `ceil(chars / 4)`: deterministic, and an approximation, not a
//! tokenizer.

use crate::diagnostic::{DIAG_PACK, PackEntry, PackTrace};

/// Estimated tokens of `s`.
pub fn estimate_tokens(s: &str) -> u64 {
    (s.chars().count() as u64).div_ceil(4)
}

/// How many leading lines fit, and how many do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    pub kept: usize,
    pub omitted: usize,
}

/// Keep whole lines, in order, while the estimate of the kept text (each line followed by a
/// newline) stays within `budget`.
pub fn fit_budget<S: AsRef<str>>(lines: &[S], budget: u64) -> Fit {
    let mut chars: u64 = 0;
    let mut kept = 0;
    for l in lines {
        let next = chars + l.as_ref().chars().count() as u64 + 1;
        if next.div_ceil(4) > budget {
            break;
        }
        chars = next;
        kept += 1;
    }
    Fit {
        kept,
        omitted: lines.len() - kept,
    }
}

/// The packing of `lines` into `budget` tokens exactly as [`fit_budget`] does it, with the cost
/// of each line (the difference of the running estimates).
pub fn pack_trace<S: AsRef<str>>(lines: &[S], budget: u64) -> PackTrace {
    let mut chars: u64 = 0;
    let mut kept = 0usize;
    let mut entries = Vec::new();
    let mut first_omitted_tokens = None;
    for (i, l) in lines.iter().enumerate() {
        let next = chars + l.as_ref().chars().count() as u64 + 1;
        let tokens = next.div_ceil(4) - chars.div_ceil(4);
        if next.div_ceil(4) > budget {
            first_omitted_tokens = Some(tokens);
            entries.push(PackEntry {
                index: i as u64,
                tokens,
                kept: false,
                tier: None,
                reason: None,
            });
            break;
        }
        if kept < DIAG_PACK {
            entries.push(PackEntry {
                index: i as u64,
                tokens,
                kept: true,
                tier: None,
                reason: None,
            });
        }
        chars = next;
        kept += 1;
    }
    PackTrace {
        budget,
        estimator: "ceil(chars/4)".into(),
        considered: lines.len() as u64,
        kept: kept as u64,
        omitted: (lines.len() - kept) as u64,
        used_tokens: chars.div_ceil(4),
        first_omitted_tokens,
        entries,
    }
}

/// The line appended to truncated text output.
pub fn omitted_notice(omitted: usize) -> String {
    format!("… {omitted} more omitted (use a larger --budget or narrow the query)")
}
