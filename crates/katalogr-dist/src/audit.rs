//! `katalogr audit record` and `katalogr audit verify` (FEAT-094 R-14).
//!
//! A record is a small text file in the kindex's git: the positions of the project's repos, the
//! facts version and the SHA-256 of the canonical graph. A re-index at the same positions
//! should reproduce the digest; `verify` says whether it does. Records hold commits and
//! digests, never a path.

use crate::error::DistError;
use crate::{Consistency, Katalogr, Options};
use camino::Utf8Path;
use katalogr_engine::{QueryOptions, Scope, canonical_graph};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Instant;

/// One repo's position in a record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PositionText {
    /// 40 lower-case hex, or `null` for a directory with no commit.
    pub commit: Option<String>,
    pub repo: String,
    /// The hash of uncommitted changes (64 lower-case hex), or `null` when the checkout is clean.
    pub tree: Option<String>,
}

/// What `audit record` writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditRecord {
    /// The facts version the graph was built under (extractor ids, salts and profile hashes).
    pub facts_version: String,
    /// SHA-256 of the canonical graph export, lower-case hex.
    pub graph_digest: String,
    /// One entry per repo with a root here, sorted by repo.
    pub positions: Vec<PositionText>,
}

impl AuditRecord {
    /// The record's file name below the project's `audit/` folder: the first 12 hex digits of
    /// the SHA-256 of the positions' compact JSON, then `.json`.
    pub fn file_name(&self) -> String {
        let compact = serde_json::to_string(&self.positions).expect("serialise positions");
        let digest = Sha256::digest(compact.as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        format!("{}.json", &hex[..12])
    }
}

/// How the current index compares with a record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AuditOutcome {
    /// Same facts version, same positions, same digest.
    Match,
    /// Same facts version and positions, different graphs.
    Mismatch { expected: String, found: String },
    /// Built under another facts version, so the digests cannot be compared.
    NotComparable {
        record_facts: String,
        current_facts: String,
    },
    /// The checkouts are not at the recorded positions. `command` makes an index at them.
    NotAtPosition {
        want: Vec<PositionText>,
        have: Vec<PositionText>,
        command: String,
    },
}

fn refused(exit: i32, message: impl Into<String>) -> DistError {
    DistError::Refused {
        exit,
        message: message.into(),
    }
}

impl Katalogr {
    /// The current facts version, graph digest and positions, after waiting for the index to
    /// catch up with the checkouts. Writes nothing.
    fn audit_now(&self, opts: &Options) -> Result<AuditRecord, DistError> {
        let timeout = opts.timeout()?;
        let deadline = Instant::now() + timeout;
        let consistency = opts.consistency.unwrap_or(Consistency::Wait);
        let engine = self.engine_for(consistency, deadline)?;
        let answer = engine.with_consistency(
            &QueryOptions {
                consistency: consistency.into(),
                timeout,
                scope: Scope::Project,
            },
            |tx| {
                let digest = Sha256::digest(canonical_graph(tx).as_bytes());
                (digest, engine.facts_version(tx))
            },
        )?;
        if answer.stale {
            return Err(refused(
                5,
                "the index is not current: run `katalogr index`, or raise --timeout",
            ));
        }
        let (digest, facts) = answer.value;
        let mut positions = Vec::new();
        for repo in engine.roots().keys() {
            let p = engine.live_position(repo)?;
            positions.push(PositionText {
                commit: p.commit,
                repo: p.repo,
                tree: p.dirty,
            });
        }
        positions.sort_by(|a, b| a.repo.cmp(&b.repo));
        Ok(AuditRecord {
            facts_version: facts?,
            graph_digest: digest.iter().map(|b| format!("{b:02x}")).collect(),
            positions,
        })
    }

    /// Takes the record of the current index and writes it to
    /// `<project>/audit/<positions-hash12>.json`, replacing a record at the same positions (git
    /// holds the older one). The file is sorted text.
    pub fn audit_record(&self, opts: &Options) -> Result<AuditRecord, DistError> {
        let record = self.audit_now(opts)?;
        let dir = self.project.root().join("audit");
        std::fs::create_dir_all(&dir).map_err(|e| DistError::io(&dir, e))?;
        let file = dir.join(record.file_name());
        let value = serde_json::to_value(&record).expect("serialise record");
        std::fs::write(&file, katalogr_store::shard::to_sorted_json(&value))
            .map_err(|e| DistError::io(&file, e))?;
        Ok(record)
    }

    /// Compares the current index with `record`, or with the stored record of the current
    /// positions when none is given (a refusal with status 4 when there is none). The order:
    /// facts version, positions, digest.
    pub fn audit_verify(
        &self,
        record: Option<&Utf8Path>,
        opts: &Options,
    ) -> Result<AuditOutcome, DistError> {
        let now = self.audit_now(opts)?;
        let file = match record {
            Some(p) => p.to_owned(),
            None => self.project.root().join("audit").join(now.file_name()),
        };
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(refused(
                    4,
                    match record {
                        Some(_) => "the audit record does not exist".to_string(),
                        None => "no audit record for the current positions: run `katalogr audit record` at a release"
                            .to_string(),
                    },
                ));
            }
            Err(e) => return Err(DistError::io(&file, e)),
        };
        let want: AuditRecord = serde_json::from_str(&text)
            .map_err(|e| DistError::usage(format!("{file}: not an audit record: {e}")))?;
        if want.facts_version != now.facts_version {
            return Ok(AuditOutcome::NotComparable {
                record_facts: want.facts_version,
                current_facts: now.facts_version,
            });
        }
        if want.positions != now.positions {
            let rev = want
                .positions
                .iter()
                .find_map(|p| p.commit.clone())
                .unwrap_or_else(|| "<rev>".to_string());
            return Ok(AuditOutcome::NotAtPosition {
                want: want.positions,
                have: now.positions,
                command: format!("katalogr context add --commit {rev}"),
            });
        }
        Ok(if want.graph_digest == now.graph_digest {
            AuditOutcome::Match
        } else {
            AuditOutcome::Mismatch {
                expected: want.graph_digest,
                found: now.graph_digest,
            }
        })
    }
}

/// The JSON Schema of an [`AuditRecord`].
pub fn schema() -> serde_json::Value {
    schemars::schema_for!(AuditRecord).to_value()
}
