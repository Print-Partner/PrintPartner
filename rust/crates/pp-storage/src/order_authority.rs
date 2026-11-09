//! One order is one Build (GRE-1095). An order number belongs to at most one
//! unarchived Build of a tenant, compared after [`normalize`]. Accept is the
//! only Rust write of `build_profiles.order_number`, and it checks
//! [`claim`] inside its immediate transaction, so two Accepts that give
//! different Builds the same order number serialize and the second is refused.
//!
//! Existing duplicates, such as two Builds in copied Node data that already
//! share an order, are left as they are: [`holders`] reports them, and a Build
//! keeps an order number it already holds. Nothing merges or rewrites them.
//!
//! Compiled with `desktop_rebuild`; service callers must check `PP_DESKTOP_REBUILD`.
use anyhow::Result;
pub use pp_contracts::publication::OrderHolder;
use rusqlite::{Transaction, params};

/// JavaScript's `String.prototype.trim` whitespace, so the web UI's
/// `normalizeOrderNumber` splits on the same characters.
const JS_SPACE: &[char] = &[
    '\t', '\n', '\u{b}', '\u{c}', '\r', ' ', '\u{a0}', '\u{1680}', '\u{2000}', '\u{2001}',
    '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}', '\u{2008}', '\u{2009}',
    '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}', '\u{feff}',
];

/// The form two order numbers are compared in: JavaScript whitespace trimmed,
/// each inner whitespace run collapsed to one space, then lowercased.
/// `None` for a missing or blank order number, which holds no order.
/// `" PO-7 "`, `"po-7"` and `"PO-7"` are one order; `"#1001"` and `"1001"`
/// are two, because punctuation is kept.
pub fn normalize(order_number: &str) -> Option<String> {
    let words: Vec<&str> = order_number
        .split(JS_SPACE)
        .filter(|word| !word.is_empty())
        .collect();
    (!words.is_empty()).then(|| words.join(" ").to_lowercase())
}

/// Every unarchived Build of `tenant` other than `except` whose order number
/// normalizes to `order`, by id.
pub fn holders(
    tx: &Transaction<'_>,
    tenant: &str,
    order: &str,
    except: i64,
) -> Result<Vec<OrderHolder>> {
    let mut statement = tx.prepare(
        "SELECT id,name,order_number FROM build_profiles WHERE tenant_id=?1 AND id<>?2 AND archived_at IS NULL AND order_number IS NOT NULL ORDER BY id",
    )?;
    let rows = statement.query_map(params![tenant, except], |row| {
        Ok(OrderHolder {
            profile_id: row.get(0)?,
            name: row.get(1)?,
            order_number: row.get(2)?,
        })
    })?;
    let mut found = Vec::new();
    for holder in rows {
        let holder = holder?;
        if normalize(&holder.order_number).as_deref() == Some(order) {
            found.push(holder);
        }
    }
    Ok(found)
}

/// Whether Build `profile` may take `proposed` as its order number: `Ok(())`
/// for none, for the order it already holds, or for one no other unarchived
/// Build holds; otherwise the Builds that hold it. Call it inside the
/// immediate transaction that writes the order number.
pub fn claim(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    current: Option<&str>,
    proposed: Option<&str>,
) -> Result<std::result::Result<(), Vec<OrderHolder>>> {
    let Some(order) = proposed.and_then(normalize) else {
        return Ok(Ok(()));
    };
    if current.and_then(normalize).as_deref() == Some(order.as_str()) {
        return Ok(Ok(()));
    }
    let builds = holders(tx, tenant, &order, profile)?;
    Ok(if builds.is_empty() {
        Ok(())
    } else {
        Err(builds)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn order_authority_normalizes_case_and_javascript_whitespace() {
        for same in ["PO-7", " po-7 ", "\u{feff}Po-7\u{3000}", "\tPO-7\n"] {
            assert_eq!(normalize(same).as_deref(), Some("po-7"), "{same:?}");
        }
        assert_eq!(normalize("Order \u{a0}\t 12").as_deref(), Some("order 12"));
        assert_eq!(normalize("#1001").as_deref(), Some("#1001"));
        assert_ne!(normalize("#1001"), normalize("1001"));
        for blank in ["", "   ", "\u{feff}\u{2028}"] {
            assert_eq!(normalize(blank), None, "{blank:?}");
        }
        // U+0085 is Rust whitespace but not JavaScript whitespace.
        assert_eq!(normalize("\u{85}PO-7").as_deref(), Some("\u{85}po-7"));
    }

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE build_profiles(id INTEGER PRIMARY KEY,tenant_id TEXT,name TEXT,order_number TEXT,archived_at TEXT);
                 INSERT INTO build_profiles VALUES
                   (1,'default','One','PO-7',NULL),
                   (2,'default','Two',' po-7',NULL),
                   (3,'default','Archived','PO-7','2026-01-01'),
                   (4,'other','Other tenant','PO-7',NULL),
                   (5,'default','Blank','  ',NULL),
                   (6,'default','None',NULL,NULL);",
            )
            .unwrap();
        connection
    }

    #[test]
    fn order_authority_claim_refuses_only_a_new_order_another_unarchived_build_holds() {
        let mut connection = database();
        let tx = connection.transaction().unwrap();
        let held = |except| {
            holders(&tx, "default", "po-7", except)
                .unwrap()
                .into_iter()
                .map(|holder| holder.profile_id)
                .collect::<Vec<_>>()
        };
        // Archived Builds, other tenants and blank order numbers hold nothing.
        assert_eq!(held(6), [1, 2]);
        assert_eq!(held(1), [2]);
        let claim = |profile, current, proposed| {
            claim(&tx, "default", profile, current, proposed)
                .unwrap()
                .map_err(|builds| builds.iter().map(|b| b.profile_id).collect::<Vec<_>>())
        };
        assert_eq!(claim(6, None, Some("PO-7 ")), Err(vec![1, 2]));
        assert_eq!(claim(6, None, Some("PO-8")), Ok(()));
        assert_eq!(claim(6, None, Some("  ")), Ok(()));
        assert_eq!(claim(6, None, None), Ok(()));
        // An existing duplicate keeps its order, in any spelling.
        assert_eq!(claim(1, Some("PO-7"), Some("po-7")), Ok(()));
    }
}
