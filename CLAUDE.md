<!-- kanbanr:begin — generated, edits here are overwritten -->

## This project is tracked with kanbanr

The board is the system of record for scope, reasoning, progress and documentation. It is a separate git repository beside this one. **Recover from it at the start of every session** and record work there as you go — this block is a pointer, not a copy.

**Why this project exists.** katalogr is an indexing and cross-referencing solution for code, documents and kanbanr items, giving full traceability between artifacts of any technology and any connecting protocol, within and across projects.

**What it commits to** (work items link these by id):

- `G-0` The system stays operable and maintainable.
- `G-1` Full code traceability of every atomic code block (file/function/method) in a polyglot repository.
- `G-2` Cross-repository scanning with bidirectionally traceable inter-repository dependencies.
- `G-3` All dependencies traced for vulnerability assessment.
- `G-4` Cross-language dependencies traced through intermediate connectivity: JNI, WASM, gRPC, Thrift, REST, queue/topic messaging, event bus/brokers, Dapr, Knative, CloudEvents, Avro, Kafka, GraphQL, Dubbo, QUIC, HTTP/1.1 and HTTP/2, WebSockets, RSocket, ZeroMQ, Cap'n Proto, FlatBuffers, NATS, Tremor, SQL stored procedures/triggers in native or intermediary languages.
- `G-5` Initial language support: Rust, Java, Kotlin, JavaScript, TypeScript, HTML, CSS/SCSS/PostCSS/LESS/Tailwind, Python, Shell, Markdown, Dart, Flutter, Ruby, C, C++ (latest release), C#, Visual Basic, PHP.
- `G-6` SCIP may be used for indexing but must not limit the tool.
- `G-7` Teams can choose the language appropriate to the task.
- `G-8` Management risk on a polyglot codebase is minimised.
- `G-9` Flexible to add additional languages.
- `G-10` Quantitative and qualitative quality metrics are tracked over time and enforceable as gates.

**Constraints:**

- Token use is optimized across multiple projects on one Claude account (right model for the right task, cost-optimized).
- One installable binary (curl + install script, like kanbanr) needing no toolchain beyond cargo. Must never be violated.
- The core model names no specific language, format or protocol; each is an extension.
- Scanned repositories' names, paths and code never appear in katalogr's repository, board, docs or fixtures; their index data and project-specific profiles live in separate kindex repositories.
- Every traceability and analytics capability is fully usable standalone (CLI, web view, editor, CI) without an agent; agent integration accelerates, never gates.

**How work moves here** — each stage asks only for what it needs; `kanbanr check <CODE>` names what the next one still lacks:

- **Vision** — Say what this is and why it matters — a statement and the goal it serves.
- **Business Arch** — Who it is for, what it must do, and why — the behaviour, as requirements.
- **System Design** — How and where it will be built, and the quality it must reach.
- **Implementation** — Build it, with a test named for every requirement.
- **Migration** — Move it into use, with the evidence that it works.
- **Operations** — Released and running.

**Before starting an item:** `kanbanr lessons --for <CODE>` — what this project already learned. **Before calling one done:** `kanbanr check <CODE>`.

| Question | Command |
|---|---|
| What is planned, in progress and done? | `kanbanr board` |
| What can I pick up now? | `kanbanr ready` |
| Why does this item exist, and how is it verified? | `kanbanr feature show <CODE>` |
| Why is the architecture like this? | `kanbanr adr list` |
| Why does this line of code exist? | `kanbanr why <file>:<line>` |
| What did the last wave cost? | `kanbanr retro <MS-00x>` |

Regenerate this block with `kanbanr claude sync`.

<!-- kanbanr:end -->

## Project conventions (hand-written; outside the generated block)

- **System Design = low-level design in the spec.** Before an item leaves System Design, its spec holds a `## Design` section covering everything in the board doc `method/system-design-spec-standard.md`: crates, modules, files, public signatures with responsibilities, data formats, algorithms, concurrency and failure handling, configuration, dependencies, performance budgets and the test plan. It ends with an implementation task list detailed enough for a low-cost subagent to build each task without design decisions, and those tasks are mirrored in the item's todo-list. Implementation requires the user's `design-review` sign-off.
- **Living documentation.** The architecture repository is in board docs: `architecture/` (views, component catalog, Zachman coverage) and `design/components/` (one design doc per component, as built). Before Migration, each item's Design is folded into the docs it affects; this is the "Definition of done: documentation" task on every item, and Migration requires the user's `docs-folded` sign-off. See `method/living-documentation.md`.
- **Live links only.** In board docs and specs, every reference to an item, requirement, ADR, doc, milestone or goal is a clickable link (`/p/katalogr/...`). A spec's `## Design` ends with an "Affects docs" list of links, and that list is the dependency list for fold-in and drift checks. See the linking rules in `method/living-documentation.md`.
