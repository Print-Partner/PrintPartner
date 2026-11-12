//! Read the same optional addon documents as Node. Paths are installation
//! constants; no MCP argument chooses a filesystem path.
use super::super::yaml::{Document, Node};
use super::*;
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;
fn yaml(bytes: &[u8]) -> Option<Value> {
    let flag = AtomicBool::new(false);
    let mut budget = PreparationBudget::new(&flag, Default::default()).ok()?;
    let doc = Document::parse(bytes, &mut budget).ok()?;
    fn value(
        doc: &Document,
        id: usize,
        depth: usize,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<Value> {
        budget.visit(depth)?;
        Ok(match doc.node(id, budget)? {
            Node::Null => Value::Null,
            Node::Bool(b) => json!(b),
            Node::Number(n) => json!(n),
            Node::String(s) => json!(String::from_utf16_lossy(s.units())),
            Node::Sequence(ids) => Value::Array(
                ids.iter()
                    .map(|id| value(doc, *id, depth + 1, budget))
                    .collect::<ReadResult<_>>()?,
            ),
            Node::Mapping(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(k, id)| {
                        Ok((
                            String::from_utf16_lossy(k.units()),
                            value(doc, *id, depth + 1, budget)?,
                        ))
                    })
                    .collect::<ReadResult<_>>()?,
            ),
            Node::Alias(_) => unreachable!(),
        })
    }
    value(&doc, doc.root, 0, &mut budget).ok()
}
fn read(path: &Path) -> Option<Value> {
    // Bound optional document input like the existing Source read port.
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 1024 * 1024 {
        return None;
    }
    if path.extension().is_some_and(|s| s == "json") {
        serde_json::from_slice(&bytes).ok()
    } else {
        yaml(&bytes)
    }
}
pub(crate) fn load(data: &Path) -> Value {
    let shipped_catalog = yaml(include_bytes!(
        // ast-grep-ignore: no-writer-family-literal -- Read-only addon evidence; no catalog or domain-pack writes.
        "../../../../../../web/apps/server/src/data/kit-catalog.yaml"
    ))
    .unwrap_or_default();
    let catalog = [
        // ast-grep-ignore: no-writer-family-literal -- Read-only addon evidence; no catalog or domain-pack writes.
        "kit-catalog.json",
        // ast-grep-ignore: no-writer-family-literal -- Read-only addon evidence; no catalog or domain-pack writes.
        "kit-catalog.yaml",
        // ast-grep-ignore: no-writer-family-literal -- Read-only addon evidence; no catalog or domain-pack writes.
        "manifests/kit-catalog.json",
        // ast-grep-ignore: no-writer-family-literal -- Read-only addon evidence; no catalog or domain-pack writes.
        "manifests/kit-catalog.yaml",
    ]
    .iter()
    .find_map(|p| read(&data.join(p)).filter(Value::is_object))
    .unwrap_or_else(|| shipped_catalog.clone());
    // ast-grep-ignore: no-writer-family-literal -- Read-only addon evidence; no catalog or domain-pack writes.
    let root = data.join("assistant-domain");
    let sources: Vec<_> = fs::read_dir(root.join("sources"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|d| d.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|d| read(&d.path().join("compatibility.yaml")))
        .collect();
    // ast-grep-ignore: no-writer-family-literal -- Read-only addon evidence; no catalog or domain-pack writes.
    let conflicts=read(&root.join("_global/merge_conflicts.yaml")).unwrap_or_else(||yaml(include_bytes!("../../../../../../web/apps/server/src/data/assistant-domain/_global/merge_conflicts.yaml")).unwrap_or_default());
    json!({"catalog":catalog,"default_catalog":shipped_catalog,"sources":sources,"merge_conflicts":conflicts})
}
