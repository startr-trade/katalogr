//! Token budgets. The estimate is `ceil(chars / 4)`: deterministic, and an approximation, not a
//! tokenizer.

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

/// The line appended to truncated text output.
pub fn omitted_notice(omitted: usize) -> String {
    format!("… {omitted} more omitted (use a larger --budget or narrow the query)")
}
