//! Technology-neutral model: names, ids, nodes, links, fragments and the extractor SPI (FEAT-001, ADR-0012).

pub mod annotate;
pub mod embed;
pub mod extract;
pub mod fragment;
pub mod graph;
pub mod insight;
pub mod lexicon;
pub mod name;
pub mod relpath;
pub mod structure;

pub use embed::{
    CALL_CALLEE_MAX, CallContext, EMBED_MAX_LEN, EMBED_MAX_PER_FILE, EMBED_MIN_LEN, EmbeddedInput,
    EmbeddedText, innermost_owner, offer,
};
pub use extract::{ExtractError, ExtractInput, Extraction, Extractor};
pub use fragment::{Binding, ContractRole, Export, ExportKey, Fragment, OutRef};
pub use graph::{
    ArtifactNode, Confidence, Fact, Link, LinkKind, LinkTarget, Provenance, Span, SymbolNode,
};
pub use lexicon::{Field, tokenize};
pub use name::{
    AnyName, ArtifactName, ContractName, Id, Name, NameError, Segment, SymbolName, parse_name,
};
pub use relpath::RelPath;
