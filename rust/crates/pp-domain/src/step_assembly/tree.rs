//! The part tree of a checked [`ProductStructure`] (GRE-1855).
//!
//! Every definition that no occurrence places is a root, in entity order.
//! Each occurrence is one child node of its parent, in entity order, so a
//! definition placed twice appears twice, with its whole subtree each time.
//! The build refuses a definition that contains itself, directly or through
//! its children, as [`StepError::Cycle`], whether or not a root reaches it.
//!
//! The build is iterative: no input can deepen the call stack. Each node is
//! checked against [`TreeLimits`] and the cancel flag before it is created, so
//! nothing over a cap is retained and a refusal costs at most the cap.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use super::{Limit, ProductStructure, StepError, Usage};

/// Bounds on one tree, each checked before a node is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeLimits {
    /// Nodes in the whole tree.
    pub max_nodes: usize,
    /// Levels; a root is level 1. 60 keeps the tree's JSON (two nesting
    /// levels per tree level) under serde_json's nesting limit of 128.
    pub max_levels: usize,
    /// Bytes of every node's product id, name and instance, added up.
    pub max_name_bytes: usize,
}

impl Default for TreeLimits {
    fn default() -> Self {
        Self {
            max_nodes: 100_000,
            max_levels: 60,
            max_name_bytes: 32 << 20,
        }
    }
}

/// The assembly tree of one STEP file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartTree {
    pub roots: Vec<PartNode>,
}

/// One placed part or sub-assembly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartNode {
    /// The `PRODUCT` id.
    pub product_id: String,
    /// The `PRODUCT` name; empty for `$`.
    pub name: String,
    /// The occurrence that places this node: its name, or its id when the
    /// name is empty. `None` for a root, or when both are empty.
    pub instance: Option<String>,
    /// In the entity order of their occurrences.
    pub children: Vec<PartNode>,
}

/// Builds the part tree of `structure`. `cancelled` is checked once per
/// definition while cycles are sought and once per node while the tree is
/// built; setting it ends the build with [`StepError::Cancelled`].
pub fn part_tree(
    structure: &ProductStructure,
    limits: TreeLimits,
    cancelled: &AtomicBool,
) -> Result<PartTree, StepError> {
    build(structure, limits, || cancelled.load(Ordering::Relaxed))
}

/// The occurrences inside each definition, in entity order.
type Children<'a> = HashMap<u64, Vec<&'a Usage>>;

pub(super) fn build(
    structure: &ProductStructure,
    limits: TreeLimits,
    mut cancelled: impl FnMut() -> bool,
) -> Result<PartTree, StepError> {
    if structure.definitions.is_empty() {
        return Err(StepError::NoProducts);
    }
    let mut children: Children = HashMap::new();
    for usage in structure.usages.values() {
        children.entry(usage.parent).or_default().push(usage);
    }
    refuse_cycles(structure, &children, &mut cancelled)?;
    let placed: HashSet<u64> = structure.usages.values().map(|usage| usage.child).collect();
    let mut builder = Builder {
        structure,
        children: &children,
        limits,
        nodes: 0,
        name_bytes: 0,
    };
    let mut roots = Vec::new();
    for &definition in structure.definitions.keys() {
        if !placed.contains(&definition) {
            builder.add_root(definition, &mut roots, &mut cancelled)?;
        }
    }
    Ok(PartTree { roots })
}

/// Fails with [`StepError::Cycle`] when a definition contains itself. An
/// iterative depth-first search over every definition, so it sees cycles no
/// root reaches and visits each definition and occurrence once.
fn refuse_cycles(
    structure: &ProductStructure,
    children: &Children,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(), StepError> {
    // Absent: not yet seen. false: on the current path. true: finished.
    let mut finished: HashMap<u64, bool> = HashMap::new();
    for &start in structure.definitions.keys() {
        if finished.contains_key(&start) {
            continue;
        }
        let mut path = vec![(start, 0)];
        finished.insert(start, false);
        if cancelled() {
            return Err(StepError::Cancelled);
        }
        while let Some((definition, next)) = path.last_mut() {
            let below = children.get(definition).map_or(&[][..], Vec::as_slice);
            let Some(usage) = below.get(*next) else {
                finished.insert(*definition, true);
                path.pop();
                continue;
            };
            *next += 1;
            match finished.get(&usage.child) {
                Some(false) => {
                    return Err(StepError::Cycle {
                        entity: usage.child,
                    });
                }
                Some(true) => {}
                None => {
                    if cancelled() {
                        return Err(StepError::Cancelled);
                    }
                    finished.insert(usage.child, false);
                    path.push((usage.child, 0));
                }
            }
        }
    }
    Ok(())
}

struct Builder<'a> {
    structure: &'a ProductStructure,
    children: &'a Children<'a>,
    limits: TreeLimits,
    nodes: usize,
    name_bytes: usize,
}

impl Builder<'_> {
    /// One node at `level`, checked against the limits and the cancel flag.
    fn node(
        &mut self,
        definition: u64,
        instance: Option<&String>,
        level: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<PartNode, StepError> {
        if cancelled() {
            return Err(StepError::Cancelled);
        }
        if level > self.limits.max_levels {
            return Err(StepError::TooLarge(Limit::Levels));
        }
        if self.nodes == self.limits.max_nodes {
            return Err(StepError::TooLarge(Limit::Nodes));
        }
        let structure = self.structure;
        let formation = structure.definitions[&definition];
        let (product_id, name) = &structure.products[&structure.formations[&formation]];
        let bytes = product_id.len() + name.len() + instance.map_or(0, String::len);
        if bytes > self.limits.max_name_bytes - self.name_bytes {
            return Err(StepError::TooLarge(Limit::NameBytes));
        }
        self.nodes += 1;
        self.name_bytes += bytes;
        Ok(PartNode {
            product_id: product_id.clone(),
            name: name.clone(),
            instance: instance.cloned(),
            children: Vec::new(),
        })
    }

    /// Adds the whole subtree of the root `definition` to `roots`, built depth
    /// first with an explicit path whose length is the level, so the level
    /// cap is checked before each descent.
    fn add_root(
        &mut self,
        definition: u64,
        roots: &mut Vec<PartNode>,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), StepError> {
        let children = self.children;
        let below = |definition| children.get(&definition).map_or(&[][..], Vec::as_slice);
        let root = self.node(definition, None, 1, cancelled)?;
        let mut path = vec![(root, below(definition).iter())];
        while let Some((node, mut usages)) = path.pop() {
            if let Some(usage) = usages.next() {
                // `node` is at level `path.len() + 1`, its child one below.
                let level = path.len() + 2;
                let child = self.node(usage.child, usage.instance.as_ref(), level, cancelled)?;
                path.push((node, usages));
                path.push((child, below(usage.child).iter()));
            } else if let Some((parent, _)) = path.last_mut() {
                parent.children.push(node);
            } else {
                roots.push(node);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
