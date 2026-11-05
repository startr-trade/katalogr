//! Finding and replacing fenced `katalogr-query` blocks in Markdown.

/// The info string that marks a query block.
const INFO: &str = "katalogr-query";

/// One fenced query block: byte range of the whole fence, opening line through the end of the
/// closing line (its line break is not included), and the text between the fences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub start: usize,
    pub end: usize,
    pub source: String,
}

/// A fence line: indent of at most three spaces, then three or more backticks or tildes.
fn fence(line: &str) -> Option<(char, usize, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let c = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = rest.chars().take_while(|x| *x == c).count();
    if len < 3 {
        return None;
    }
    let info = rest[len..].trim();
    // A backtick fence's info string holds no backtick.
    if c == '`' && info.contains('`') {
        return None;
    }
    Some((c, len, info))
}

struct Open {
    ch: char,
    len: usize,
    query: bool,
    start: usize,
    source: Vec<String>,
}

/// Every closed query block, in document order. A fence inside a longer or different fence is
/// content, not a block, and an unclosed fence is left alone.
pub fn find_blocks(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut open: Option<Open> = None;
    let mut pos = 0;
    for raw in markdown.split_inclusive('\n') {
        let line_start = pos;
        pos += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        match open.take() {
            None => {
                if let Some((ch, len, info)) = fence(line) {
                    let query = info.split_whitespace().next() == Some(INFO);
                    open = Some(Open {
                        ch,
                        len,
                        query,
                        start: line_start,
                        source: Vec::new(),
                    });
                }
            }
            Some(o) => {
                let closes = fence(line)
                    .is_some_and(|(ch, len, info)| ch == o.ch && len >= o.len && info.is_empty());
                if closes {
                    if o.query {
                        blocks.push(Block {
                            start: o.start,
                            end: line_start + line.len(),
                            source: o.source.join("\n"),
                        });
                    }
                } else {
                    let mut o = o;
                    o.source.push(line.to_string());
                    open = Some(o);
                }
            }
        }
    }
    blocks
}

/// The document with each query block replaced by what `f` returns; everything else is copied
/// byte for byte.
pub fn replace_blocks(markdown: &str, f: &mut dyn FnMut(&Block) -> String) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut at = 0;
    for b in find_blocks(markdown) {
        out.push_str(&markdown[at..b.start]);
        out.push_str(&f(&b));
        at = b.end;
    }
    out.push_str(&markdown[at..]);
    out
}

#[cfg(test)]
#[test]
fn finds_query_blocks() {
    let md = "# T\n\n```katalogr-query\nstructure --level package\n```\n\ntext\n\n~~~katalogr-query extra\ncycles\n~~~\n";
    let blocks = find_blocks(md);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].source, "structure --level package");
    assert_eq!(
        &md[blocks[0].start..blocks[0].end],
        "```katalogr-query\nstructure --level package\n```"
    );
    assert_eq!(blocks[1].source, "cycles");
    let out = replace_blocks(md, &mut |b| format!("<{}>", b.source.len()));
    assert_eq!(out, "# T\n\n<25>\n\ntext\n\n<6>\n");
    // CRLF input: the source is clean and the line breaks stay.
    let crlf = "a\r\n```katalogr-query\r\ncycles\r\n```\r\nb\r\n";
    let b = find_blocks(crlf);
    assert_eq!(b[0].source, "cycles");
    assert_eq!(replace_blocks(crlf, &mut |_| "X".into()), "a\r\nX\r\nb\r\n");
}

#[cfg(test)]
#[test]
fn other_fences_untouched() {
    let md = "```rust\nlet x = 1;\n```\n\n````markdown\n```katalogr-query\nstructure\n```\n````\n\n```text\n```katalogr-query\n```\n\n```katalogr-query\nunclosed\n";
    assert!(find_blocks(md).is_empty());
    assert_eq!(replace_blocks(md, &mut |_| "X".into()), md);
    // Only a plain info string match counts.
    let md2 = "```katalogr-queryx\ncycles\n```\n    ```katalogr-query\n    cycles\n    ```\n";
    assert!(find_blocks(md2).is_empty());
}
