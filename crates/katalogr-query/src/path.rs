//! `path`: the shortest chain of links between two symbols, in either direction.

use crate::refs::link_confidence;
use crate::result::{Direction, PathStep, SymbolHit};
use katalogr_model::{Id, Name};
use katalogr_store::{LinkKey, Reader, StoreError};
use std::collections::{BTreeMap, VecDeque};

/// Breadth-first over links followed forwards and backwards, at most `max_len` steps. Among
/// the shortest chains the first by node names is returned (neighbours are tried in order of
/// name, then link kind, then direction). `Some(empty)` when `a` and `b` are the same symbol;
/// `None` when no chain exists within `max_len`.
pub fn path(
    tx: &impl Reader,
    a: &SymbolHit,
    b: &SymbolHit,
    max_len: u32,
) -> Result<Option<Vec<PathStep>>, StoreError> {
    let start = Id::of(&a.name);
    let goal = Id::of(&b.name);
    if start == goal {
        return Ok(Some(Vec::new()));
    }
    // node -> (previous node, link kind, direction)
    let mut from: BTreeMap<Id, (Id, String, Direction)> = BTreeMap::new();
    let mut names: BTreeMap<Id, String> = BTreeMap::new();
    let mut frontier = VecDeque::from([(start, 0u32)]);
    let mut seen = std::collections::BTreeSet::from([start]);
    'search: while let Some((id, depth)) = frontier.pop_front() {
        if depth >= max_len {
            continue;
        }
        let mut next: Vec<(String, String, Direction, Id)> = Vec::new();
        for (key, _) in tx.links_out(id, None)? {
            neighbour(
                tx,
                &mut names,
                &mut next,
                key.to,
                key.kind.0,
                Direction::Forward,
            )?;
        }
        for key in tx.links_in(id)? {
            neighbour(
                tx,
                &mut names,
                &mut next,
                key.from,
                key.kind.0,
                Direction::Backward,
            )?;
        }
        next.sort();
        for (_, kind, dir, other) in next {
            if !seen.insert(other) {
                continue;
            }
            from.insert(other, (id, kind, dir));
            if other == goal {
                break 'search;
            }
            frontier.push_back((other, depth + 1));
        }
    }
    if !from.contains_key(&goal) {
        return Ok(None);
    }
    let mut steps = Vec::new();
    let mut at = goal;
    while at != start {
        let (prev, kind, dir) = from[&at].clone();
        let key = match dir {
            Direction::Forward => LinkKey {
                from: prev,
                kind: katalogr_model::LinkKind(kind.clone()),
                to: at,
            },
            Direction::Backward => LinkKey {
                from: at,
                kind: katalogr_model::LinkKind(kind.clone()),
                to: prev,
            },
        };
        steps.push(PathStep {
            from: name_of(tx, &mut names, prev)?,
            confidence: link_confidence(tx, &key)?,
            kind,
            direction: dir,
            to: name_of(tx, &mut names, at)?,
        });
        at = prev;
    }
    steps.reverse();
    Ok(Some(steps))
}

fn name_of(
    tx: &impl Reader,
    names: &mut BTreeMap<Id, String>,
    id: Id,
) -> Result<String, StoreError> {
    if let Some(n) = names.get(&id) {
        return Ok(n.clone());
    }
    let n = match tx.symbol(id)? {
        Some(s) => s.name.canonical(),
        None => tx
            .contract(id)?
            .map(|c| c.name.canonical())
            .unwrap_or_default(),
    };
    names.insert(id, n.clone());
    Ok(n)
}

/// Queue `other` as a neighbour when it is a symbol or a contract node.
fn neighbour(
    tx: &impl Reader,
    names: &mut BTreeMap<Id, String>,
    next: &mut Vec<(String, String, Direction, Id)>,
    other: Id,
    kind: String,
    dir: Direction,
) -> Result<(), StoreError> {
    let name = name_of(tx, names, other)?;
    if !name.is_empty() {
        next.push((name, kind, dir, other));
    }
    Ok(())
}
