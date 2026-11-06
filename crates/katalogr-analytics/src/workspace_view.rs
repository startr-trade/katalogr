//! The project-level map of a workspace (FEAT-020): which project consumes what another provides.
//! Pure; the caller passes only the ends the user can read, so an end the user cannot read counts
//! nowhere.

use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// One end of a contract in one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractEndRow {
    pub project: String,
    pub contract_kind: String,
    /// The contract's canonical name.
    pub contract: String,
    /// `provides`, `consumes` or `declares`.
    pub role: String,
    pub repo: String,
}

/// Contracts that `consumer` takes from `provider`, of one kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ProjectEdge {
    pub consumer: String,
    pub provider: String,
    pub kind: String,
    pub contracts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorkspaceMap {
    pub projects: Vec<String>,
    pub edges: Vec<ProjectEdge>,
}

/// Pure. Takes only rows the user can read. Counts each contract once per (consumer, provider,
/// kind). Skips a pair of ends in one project. Sorted.
pub fn workspace_map(projects: &[String], rows: &[ContractEndRow]) -> WorkspaceMap {
    // (kind, contract) to the projects providing and consuming it.
    let mut by_contract: BTreeMap<(&str, &str), (BTreeSet<&str>, BTreeSet<&str>)> = BTreeMap::new();
    for r in rows {
        let entry = by_contract
            .entry((r.contract_kind.as_str(), r.contract.as_str()))
            .or_default();
        match r.role.as_str() {
            "provides" => {
                entry.0.insert(r.project.as_str());
            }
            "consumes" => {
                entry.1.insert(r.project.as_str());
            }
            _ => {}
        }
    }
    let mut counts: BTreeMap<(&str, &str, &str), u64> = BTreeMap::new();
    for ((kind, _), (providers, consumers)) in &by_contract {
        for p in providers {
            for c in consumers {
                if p != c {
                    *counts.entry((c, p, kind)).or_default() += 1;
                }
            }
        }
    }
    let mut names: BTreeSet<String> = projects.iter().cloned().collect();
    names.extend(rows.iter().map(|r| r.project.clone()));
    WorkspaceMap {
        projects: names.into_iter().collect(),
        edges: counts
            .into_iter()
            .map(|((consumer, provider, kind), contracts)| ProjectEdge {
                consumer: consumer.to_string(),
                provider: provider.to_string(),
                kind: kind.to_string(),
                contracts,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(project: &str, kind: &str, contract: &str, role: &str, repo: &str) -> ContractEndRow {
        ContractEndRow {
            project: project.into(),
            contract_kind: kind.into(),
            contract: contract.into(),
            role: role.into(),
            repo: repo.into(),
        }
    }

    /// R-11: only the rows passed in count; one contract counts once per pair and kind; two ends
    /// in one project make no edge.
    #[test]
    fn map_counts_visible_ends_only() {
        let projects = vec!["b".to_string(), "a".to_string()];
        let visible = vec![
            row("a", "svc", "contract:svc:x", "provides", "r1"),
            row("a", "svc", "contract:svc:x", "provides", "r2"),
            row("b", "svc", "contract:svc:x", "consumes", "r1"),
            row("b", "svc", "contract:svc:x", "consumes", "r2"),
            row("b", "svc", "contract:svc:y", "provides", "r1"),
            row("b", "svc", "contract:svc:y", "consumes", "r1"),
            row("a", "topic", "contract:topic:t", "provides", "r1"),
            row("b", "topic", "contract:topic:t", "consumes", "r1"),
        ];
        let m = workspace_map(&projects, &visible);
        assert_eq!(m.projects, ["a", "b"]);
        assert_eq!(
            m.edges,
            vec![
                ProjectEdge {
                    consumer: "b".into(),
                    provider: "a".into(),
                    kind: "svc".into(),
                    contracts: 1
                },
                ProjectEdge {
                    consumer: "b".into(),
                    provider: "a".into(),
                    kind: "topic".into(),
                    contracts: 1
                },
            ]
        );
        // A restricted end is simply not passed: the map equals one built without it.
        let mut extra = visible.clone();
        extra.truncate(2);
        assert!(workspace_map(&projects, &extra).edges.is_empty());
    }
}
