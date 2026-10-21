---
name: katalogr
description: Deterministic code and artifact graph for the current project. Use it before reading or grepping whenever the question is where something is, who uses it, why it exists, how two things connect, or what a change would touch, in any indexed project (a `.katalogr` marker exists, or `katalogr` answers). It replaces whole-file reads with exact spans and ranked, budgeted answers. Covers search, show, explain, refs, callers, path, why, map, dangling, index and notify, with playbooks for scoping work, finding where to implement, assessing a change, tracing a flow, reviewing a change, and keeping docs and schemas in sync.
---

# katalogr: ask the graph before you read

katalogr has already parsed this project into a graph of artifacts, symbols, contracts and links. Every answer is deterministic and says why it matched. You bring the language understanding; katalogr brings exact locations and relationships. A katalogr query costs a few hundred tokens. Reading whole files to find the same thing costs thousands.

## 1. When to use it

**Use katalogr first when:**
- the question is **where** something is, **who** uses it, **what** it connects to, **why** it exists, or **what a change touches**;
- you are about to Read a whole file to find one function: use `show` instead;
- you are about to Grep for an identifier: use `search` or `refs` instead.

**Read or Grep directly when:**
- the text is not code or docs that katalogr indexes (logs, generated output, files outside the project);
- the file is small and you need all of it anyway;
- katalogr answered exit 5 (stale) and you cannot reindex right now.

## 2. Query craft

1. **Turn the user's words into identifiers.** "where do we bill customers" becomes `search bill charge invoice rate`. Tokens are split on case and underscores, so `rate code` finds `applyRateCode`.
2. **Find, then narrow.**
   - `katalogr search <words…>` gives BM25 results. Each hit names the term and the field it matched.
   - `katalogr find <name>` matches exact or partial names.
   - Exit 3 lists the candidates: pick one and repeat with its full canonical name, for example `'repo:src/x.rs#impl:T/fn:f'`.
3. **Understand one thing.** `katalogr explain <symbol>` gives its signature, doc, and direct incoming and outgoing links in one block.
4. **Read only what you need.**
   - `katalogr show <symbol>` prints just that symbol's lines.
   - `--signature` prints the signature and doc only.
5. **Follow relationships.**
   - `refs <symbol>`: everything that points at it, including through re-exports.
   - `callers <symbol> --depth 2`: who calls it, and who calls those.
   - `refs` and `callers` fold a name with 50 or more same-named targets into one line; add `--expand` to list them.
   - `path <a> <b>`: the shortest chain between two things, with each link's kind and confidence.
   - `why <file>:<line>`: the symbol at that line, the board items and goals it serves, and the commit that last changed it.
6. **Orient in an unfamiliar project.** `katalogr map --budget 1500` gives the most central symbols first (PageRank).
7. **Follow the `next:` line.** Every answer ends with the most useful follow-up commands.
8. **Respect confidence.**
   - `[Extracted]` links are certain.
   - `[Inferred]` links (for example a method called by name) are candidates. Confirm with `show` before relying on one.

**Global flags:**
- `--budget <tokens>` caps output and says what was left out.
- `--json` gives a versioned envelope with `watermark`, `pending`, `stale` and `next`.
- `--consistency wait|wait-all|stale-ok` and `--timeout <s>` choose how fresh the answer must be. The CLI default is `wait`.

## 3. Playbooks

### 3.1 Scope a kanbanr item
1. `katalogr search <domain words from the request>`
2. `katalogr explain <best hits>` (two or three of them)
3. `katalogr refs <the symbol you would change>` and `katalogr callers <it> --depth 2`
4. `katalogr dangling` (docs or board references already broken in the area)
5. Write the item's Design: the components you found, and an **Affects docs** list of links to their component docs.

### 3.2 Find where to implement something
1. `katalogr search <what it must do>`, plus `katalogr map --budget 800` if the area is new to you
2. `katalogr explain <the closest existing implementation>`, then `katalogr show <it>`
3. Follow the established pattern: same module and the same kind of symbol. `katalogr examples` will help here once available.

### 3.3 Assess a change before making it
1. `katalogr refs <symbol>` and `katalogr callers <symbol> --depth 3`
2. `katalogr path <symbol> <entry point>` for the flows it sits on
3. Note every `[Inferred]` hit and confirm it with `show`.
4. Report the blast radius **before** editing. `katalogr impact` will do this in one step once available.

### 3.4 Trace a flow or a bug across files
1. `katalogr why <file>:<line>` where the symptom shows
2. `katalogr callers <that symbol> --depth 3` (upstream) and `katalogr explain <it>` (downstream)
3. `katalogr path <suspected source> <symptom>`

### 3.5 Review a change
1. `katalogr index --range <base>..<head>` so the index reflects the change
2. `katalogr why <file>:<line>` for each changed hunk: does it serve the item and requirement it claims?
3. `katalogr refs` on each changed public symbol: are the callers updated?
4. `katalogr dangling`: nothing new is broken.

### 3.6 Keep docs and schemas in sync
1. `katalogr dangling --kind links-to`: doc links that no longer resolve
2. `katalogr why <doc>:<line>` and `katalogr refs <doc section>`: what the doc claims to describe
3. After a code change, `katalogr refs <changed symbol>` lists the docs that mention it. Update them in the same change.

## 4. Cost routing

- **Bulk lookups go to a cheaper subagent.** Mapping an area, or listing every caller of ten symbols, should run in a low-cost subagent. Give it the exact commands with `--budget` and `--json`, and ask for results only, never file contents.
- **Keep reasoning in the main session.** Bring back compact results: names, locations, link kinds and confidence.
- **Prefer `show` and `explain`** over reading files, and narrow queries over larger budgets.

## 5. Output contract

| Exit | Meaning | What to do |
|---|---|---|
| 0 | answered | use it; follow `next:` |
| 3 | ambiguous: the candidates are on stdout | pick one and repeat with its full canonical name |
| 4 | no match | rephrase: other words, a partial name with `find`, or `search` |
| 5 | stale: the index is behind the files (or a wait timed out) | run `katalogr index`, or retry with `--consistency wait`; never trust a span from a stale answer |
| 1 | error | read stderr |
| 2 | usage or configuration | read stderr; `katalogr --help` |

**Keeping the index current:**
- after editing files outside Claude, run `katalogr notify <paths…>`;
- after switching branches, run `katalogr index --since <previous HEAD>`;
- for a full refresh, run `katalogr index --full`.
