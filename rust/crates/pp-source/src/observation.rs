use crate::directory::Directory;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{self, Read},
    path::Path,
};

pub trait ReadBudget {
    type Error;
    fn entry(&mut self, depth: usize, bytes: usize) -> Result<(), Self::Error>;
    fn artifact(&mut self, total: u64, chunk: usize) -> Result<(), Self::Error>;
}
#[derive(Debug)]
pub enum Failure<E> {
    Budget(E),
    Io(io::ErrorKind),
    UnsafePath,
}
pub struct Inventory {
    root: Directory,
    paths: Vec<String>,
    ambiguous: HashSet<String>,
}
impl Inventory {
    pub fn matching_path(&self, path: &str) -> Option<usize> {
        let key = crate::path_policy::collision_key_unicode16(path);
        if key
            .match_indices('/')
            .map(|(end, _)| &key[..end])
            .chain(std::iter::once(key.as_str()))
            .any(|prefix| self.ambiguous.contains(prefix))
        {
            return None;
        }
        let mut matches = self
            .paths
            .iter()
            .enumerate()
            .filter(|(_, value)| crate::path_policy::collision_key_unicode16(value) == key);
        let (index, _) = matches.next()?;
        matches.next().is_none().then_some(index)
    }
    pub fn scan<B: ReadBudget>(
        logical: &Path,
        budget: &mut B,
    ) -> Result<Option<Self>, Failure<B::Error>> {
        let Ok(root) = Directory::open_root(logical) else {
            return Ok(None);
        };
        let mut inventory = Self {
            root,
            paths: Vec::new(),
            ambiguous: HashSet::new(),
        };
        walk(
            &inventory.root,
            "",
            0,
            &mut inventory.paths,
            &mut inventory.ambiguous,
            budget,
        )?;
        Ok(Some(inventory))
    }
    pub fn hash<B: ReadBudget>(
        &self,
        index: usize,
        budget: &mut B,
    ) -> Result<(u64, String), Failure<B::Error>> {
        let path = self.paths.get(index).ok_or(Failure::UnsafePath)?;
        let mut file = self
            .root
            .file(path, false)
            .map_err(|_| Failure::Io(io::ErrorKind::Other))?;
        let mut buf = [0u8; 65536];
        let mut hash = Sha256::new();
        let mut total = 0;
        loop {
            let n = file.read(&mut buf).map_err(|e| Failure::Io(e.kind()))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            budget.artifact(total, n).map_err(Failure::Budget)?;
            hash.update(&buf[..n]);
        }
        Ok((total, hex::encode(hash.finalize())))
    }
}
fn walk<B: ReadBudget>(
    dir: &Directory,
    prefix: &str,
    depth: usize,
    out: &mut Vec<String>,
    ambiguous: &mut HashSet<String>,
    budget: &mut B,
) -> Result<(), Failure<B::Error>> {
    let Ok(entries) = dir.0.entries() else {
        return Ok(());
    };
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.to_string_lossy();
        if prefix.is_empty() && name == crate::MANIFEST {
            continue;
        }
        let len = prefix.len() + usize::from(!prefix.is_empty()) + name.len();
        budget.entry(depth, len).map_err(Failure::Budget)?;
        names.push(name.into_owned());
    }
    names.sort();
    let mut seen = HashSet::new();
    for name in names {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let key = crate::path_policy::collision_key_unicode16(&path);
        if !seen.insert(key.clone()) {
            ambiguous.insert(key);
        }
        let Ok(status) = dir.0.status_nofollow(&name) else {
            continue;
        };
        if status.is_dir() {
            if let Ok(child) = dir.child(&name, false) {
                walk(&child, &path, depth + 1, out, ambiguous, budget)?;
            }
        } else if status.is_file() && name.to_lowercase().ends_with(".stl") {
            out.push(path);
        }
    }
    Ok(())
}
