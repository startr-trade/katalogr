//! Extension context plumbing (FEAT-005 section 3, task T1; ADR-0015 amendment).
//!
//! An extension may name context files (`Extractor::context_files`), for example manifests.
//! The engine builds the extension's context for each repository from every such file in the
//! repository and passes it to `extract`. When the context's hash changes, every file of the
//! repository is queued for re-extraction.

use crate::consistency::Engine;
use crate::error::EngineError;
use katalogr_model::{Extractor, RelPath};
use sha2::{Digest, Sha256};

/// True when `path` is one of the extension's context files.
pub(crate) fn is_context_file(ext: &dyn Extractor, path: &RelPath) -> bool {
    ext.is_context_file(path)
}

pub(crate) fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Bumped when profile or embed application changes the output of extraction for the same
/// inputs. Part of every file revision.
pub(crate) const ENGINE_EXTRACT_REVISION: u32 = 1;

/// Everything but the blob id that the output of extracting one file depends on.
#[derive(Copy, Clone, Debug)]
pub(crate) struct RevisionFields<'a> {
    pub extension: &'a str,
    pub context: &'a [u8],
    pub profile: Option<[u8; 32]>,
    pub salt: &'a str,
    pub embed: Option<[u8; 32]>,
    pub guests: &'a [u8],
}

/// The revision an extractor is given: the blob id, then `+<16 hex digits>` of the SHA-256
/// over these fields in this order: extension id, context bytes, profile hash or nothing,
/// `ENGINE_EXTRACT_REVISION`, salt, embed signature or nothing, guest key bytes. Each field is
/// prefixed with its length (u64 little-endian), so one field's end cannot run into the next.
pub(crate) fn revision_of(blob: &str, fields: &RevisionFields) -> String {
    fn put(h: &mut Sha256, bytes: &[u8]) {
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    let mut h = Sha256::new();
    put(&mut h, fields.extension.as_bytes());
    put(&mut h, fields.context);
    put(&mut h, fields.profile.as_ref().map_or(&[][..], |p| &p[..]));
    put(&mut h, &ENGINE_EXTRACT_REVISION.to_le_bytes());
    put(&mut h, fields.salt.as_bytes());
    put(&mut h, fields.embed.as_ref().map_or(&[][..], |p| &p[..]));
    put(&mut h, fields.guests);
    let digest: [u8; 32] = h.finalize().into();
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("{blob}+{hex}")
}

/// Build the context of `ext` for `repo` from the files currently on disk. `None` when the
/// repo has no root here or the extension refuses its files (the previous context is kept).
pub(crate) fn build(
    engine: &Engine,
    repo: &str,
    ext: &dyn Extractor,
) -> Result<Option<Vec<u8>>, EngineError> {
    let Some(source) = engine.source(repo) else {
        return Ok(None);
    };
    let (include, exclude) = engine.repo_filters(repo);
    let mut files = Vec::new();
    for p in source.list(&include, &exclude)? {
        if !is_context_file(ext, &p) {
            continue;
        }
        if let Some(bytes) = source.read(&p)? {
            files.push((p, bytes));
        }
    }
    Ok(ext.build_context(&files).ok())
}

#[cfg(test)]
mod revision {
    use super::*;

    fn base() -> RevisionFields<'static> {
        RevisionFields {
            extension: "ext",
            context: b"ctx",
            profile: Some([1; 32]),
            salt: "salt-1",
            embed: Some([2; 32]),
            guests: b"guests",
        }
    }

    #[test]
    fn each_input_changes_revision() {
        let r0 = revision_of("blob", &base());
        assert_eq!(r0, revision_of("blob", &base()));
        let variants = [
            RevisionFields {
                extension: "other",
                ..base()
            },
            RevisionFields {
                context: b"ctx2",
                ..base()
            },
            RevisionFields {
                profile: Some([3; 32]),
                ..base()
            },
            RevisionFields {
                profile: None,
                ..base()
            },
            RevisionFields {
                salt: "salt-2",
                ..base()
            },
            RevisionFields {
                embed: Some([4; 32]),
                ..base()
            },
            RevisionFields {
                embed: None,
                ..base()
            },
            RevisionFields {
                guests: b"guests2",
                ..base()
            },
        ];
        for (i, v) in variants.iter().enumerate() {
            assert_ne!(
                r0,
                revision_of("blob", v),
                "variant {i} left the revision unchanged"
            );
        }
        assert_ne!(r0, revision_of("blob2", &base()));
        assert!(r0.starts_with("blob+") && r0.len() == "blob+".len() + 16);
    }

    #[test]
    fn extension_id_separates_extractors() {
        // The same bytes split differently between neighbouring fields must not collide.
        let a = RevisionFields {
            extension: "ab",
            context: b"c",
            ..base()
        };
        let b = RevisionFields {
            extension: "a",
            context: b"bc",
            ..base()
        };
        assert_ne!(revision_of("x", &a), revision_of("x", &b));
        let c = RevisionFields {
            extension: "md",
            ..base()
        };
        let d = RevisionFields {
            extension: "text",
            ..base()
        };
        assert_ne!(revision_of("x", &c), revision_of("x", &d));
    }

    #[test]
    fn known_vector() {
        let f = RevisionFields {
            extension: "ext",
            context: b"c",
            profile: None,
            salt: "s",
            embed: None,
            guests: b"",
        };
        assert_eq!(revision_of("b", &f), "b+94bd504cb6266431");
    }
}
