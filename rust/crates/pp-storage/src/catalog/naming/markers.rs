//! Role-marker removal for part slugs, as Node's `stripMarkers` does.
//!
//! The rules are the ones the per-marker regexes `([ _-]?)<marker>([ _-]?)`
//! (case-insensitive) applied until GRE-1396's fourth review round:
//!
//! - Markers run one at a time, in role order. A marker that starts and ends
//!   with a symbol, like `[a]`, goes everywhere, leftmost first, repeating on
//!   the shortened name. A marker that starts or ends with a letter or digit,
//!   like a custom `A`, goes once, only as a prefix.
//! - Each removal takes one separator (space, `_` or `-`) on each side, and
//!   puts one back when text remains on both sides: `Duct_[c]_Left` becomes
//!   `Duct_Left`.
//! - When that changed the name, the symbol markers run again, in role order,
//!   until a round removes nothing. A marker that removing a nested one
//!   exposes, as in `Panel_[a[c]]_Clip`, goes in the next round.
//!
//! Each marker's pass scans the name once, keeping the text already scanned
//! on a stack and removing a marker as soon as the stack ends with it, so a
//! pass is linear in the name's length. Characters compare through the same
//! `regress` case-insensitive match the regexes used, once per distinct
//! character.

use std::collections::HashMap;

fn strips_anywhere(marker: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    !word(marker.chars().next()) && !word(marker.chars().next_back())
}

fn separator(c: char) -> bool {
    matches!(c, ' ' | '_' | '-')
}

fn escape(c: char) -> String {
    if ".*+?^${}()|[]\\".contains(c) {
        format!("\\{c}")
    } else {
        c.to_string()
    }
}

/// A character and the marker character it equals ignoring case, if any.
type Unit = (char, Option<usize>);

/// A profile's markers, ready to strip from many names.
pub(crate) struct Markers {
    /// One case-insensitive regex per distinct marker character.
    characters: Vec<regress::Regex>,
    /// Each character seen so far and the first of `characters` it matches.
    classes: HashMap<char, Option<usize>>,
    /// Each marker as indexes into `characters`, and whether it strips anywhere.
    markers: Vec<(bool, Vec<usize>)>,
}

impl Markers {
    pub(crate) fn new(markers: &[&str]) -> Self {
        let mut prepared = Self {
            characters: Vec::new(),
            classes: HashMap::new(),
            markers: Vec::new(),
        };
        for marker in markers {
            // The old code skipped a marker whose regex did not compile.
            let whole: String = marker.chars().map(escape).collect();
            if marker.is_empty() || regress::Regex::with_flags(&whole, "i").is_err() {
                continue;
            }
            let mut classes = Vec::new();
            for c in marker.chars() {
                let class = match prepared.class(c) {
                    Some(class) => class,
                    None => {
                        let Ok(regex) =
                            regress::Regex::with_flags(&format!("^(?:{})$", escape(c)), "i")
                        else {
                            break;
                        };
                        prepared.characters.push(regex);
                        let class = prepared.characters.len() - 1;
                        prepared.classes.insert(c, Some(class));
                        class
                    }
                };
                classes.push(class);
            }
            if classes.len() == marker.chars().count() {
                prepared.markers.push((strips_anywhere(marker), classes));
            }
        }
        prepared
    }

    fn class(&mut self, c: char) -> Option<usize> {
        let characters = &self.characters;
        *self.classes.entry(c).or_insert_with(|| {
            let text = c.to_string();
            characters.iter().position(|re| re.find(&text).is_some())
        })
    }

    pub(crate) fn strip(&mut self, stem: &str) -> String {
        let name: Vec<Unit> = stem.chars().map(|c| (c, self.class(c))).collect();
        let mut slug = name.clone();
        for (anywhere, marker) in &self.markers {
            slug = if *anywhere {
                strip_everywhere(&slug, marker)
            } else {
                strip_prefix(slug, marker)
            };
        }
        if slug != name {
            loop {
                let before = slug.len();
                for (_, marker) in self.markers.iter().filter(|(anywhere, _)| *anywhere) {
                    slug = strip_everywhere(&slug, marker);
                }
                if slug.len() == before {
                    break;
                }
            }
        }
        slug.into_iter().map(|(c, _)| c).collect()
    }
}

/// Removes `markers` from `stem`; see the module comment.
pub(crate) fn strip_markers(stem: &str, markers: &[&str]) -> String {
    Markers::new(markers).strip(stem)
}

fn matches(units: &[Unit], marker: &[usize]) -> bool {
    units.len() == marker.len()
        && units
            .iter()
            .zip(marker)
            .all(|((_, class), m)| *class == Some(*m))
}

/// One marker everywhere, leftmost first, as repeated regex removals did.
/// The stack holds the scanned text with every earlier match removed, so
/// the first match in the current name is the first time the stack ends
/// with the marker. Each loop shortens the stack or consumes input.
fn strip_everywhere(input: &[Unit], marker: &[usize]) -> Vec<Unit> {
    let mut out: Vec<Unit> = Vec::with_capacity(input.len());
    let mut next = 0;
    while let Some(&unit) = input.get(next) {
        next += 1;
        out.push(unit);
        while out.len() >= marker.len() && matches(&out[out.len() - marker.len()..], marker) {
            out.truncate(out.len() - marker.len());
            let before = out.last().copied().filter(|(c, _)| separator(*c));
            if before.is_some() {
                out.pop();
            }
            let after = input.get(next).copied().filter(|(c, _)| separator(*c));
            if after.is_some() {
                next += 1;
            }
            if let Some(keep) = before.or(after)
                && !out.is_empty()
                && next < input.len()
            {
                out.push(keep);
            }
        }
    }
    out
}

/// One marker once, only at the start, with the separator after it.
fn strip_prefix(mut units: Vec<Unit>, marker: &[usize]) -> Vec<Unit> {
    if units.len() >= marker.len() && matches(&units[..marker.len()], marker) {
        let end =
            marker.len() + usize::from(units.get(marker.len()).is_some_and(|(c, _)| separator(*c)));
        units.drain(..end);
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The marker removal at 342b2c09 (`naming::strip_marker`,
    /// `strip_exposed_markers` and the preview's two passes), kept to check
    /// `Markers::strip` against. Compiled regexes are cached; nothing else
    /// changed.
    mod reference {
        use std::cell::RefCell;
        use std::collections::HashMap;

        thread_local! {
            static REGEXES: RefCell<HashMap<String, Option<regress::Regex>>> =
                RefCell::new(HashMap::new());
        }

        fn regex(pattern: &str) -> Option<regress::Regex> {
            REGEXES.with(|cache| {
                cache
                    .borrow_mut()
                    .entry(pattern.to_owned())
                    .or_insert_with(|| regress::Regex::with_flags(pattern, "i").ok())
                    .clone()
            })
        }

        fn strips_anywhere(marker: &str) -> bool {
            let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
            !word(marker.chars().next()) && !word(marker.chars().next_back())
        }

        fn strip_exposed_markers(mut slug: String, markers: &[&str]) -> String {
            loop {
                let before = slug.len();
                for marker in markers.iter().filter(|m| strips_anywhere(m)) {
                    slug = strip_marker(&slug, marker);
                }
                if slug.len() == before {
                    return slug;
                }
            }
        }

        fn strip_marker(stem: &str, marker: &str) -> String {
            let anywhere = strips_anywhere(marker);
            let mut escaped = String::new();
            for c in marker.chars() {
                if ".*+?^${}()|[]\\".contains(c) {
                    escaped.push('\\')
                }
                escaped.push(c)
            }
            let pattern = if anywhere {
                format!("([ _-]?){escaped}([ _-]?)")
            } else {
                format!("^(){escaped}([ _-]?)")
            };
            let Some(regex) = regex(&pattern) else {
                return stem.to_owned();
            };
            let mut current = stem.to_owned();
            while let Some(m) = regex.find(&current) {
                let range = m.range();
                let side = |group: usize| m.group(group).map_or("", |r| &current[r]);
                let keep = if range.start > 0 && range.end < current.len() {
                    if side(1).is_empty() { side(2) } else { side(1) }
                } else {
                    ""
                };
                current = format!("{}{keep}{}", &current[..range.start], &current[range.end..]);
                if !anywhere {
                    break;
                }
            }
            current
        }

        pub(super) fn strip(name: &str, markers: &[&str]) -> String {
            let mut slug = name.to_owned();
            for marker in markers {
                slug = strip_marker(&slug, marker);
            }
            if slug != name {
                slug = strip_exposed_markers(slug, markers);
            }
            slug
        }
    }

    /// Role-ordered marker lists: the default profile and three saved ones.
    const PROFILES: [&[&str]; 4] = [
        &["[a]", "[c]", "[o]"],
        &["[a]-[c]", "[a]", "[c]", "[o]"],
        &["[Σ]", "[c]", "[o]"],
        &["A", "[c]", "[o]"],
    ];

    /// The same token list, generator and seed as `parsers.test.ts`.
    const TOKENS: [&str; 30] = [
        "[", "]", "a", "A", "c", "C", "o", "O", "-", "_", " ", ".", "P", "n", "l", "x", "2", "Σ",
        "ς", "σ", "é", "中", "[a]", "[A]", "[c]", "[o]", "[Σ]", "[ς]", "[a]-[c]", "[a[c]]",
    ];
    const SEED: u32 = 1396;
    const NAMES: usize = 20_000;

    /// mulberry32, as in `parsers.test.ts`.
    fn next(state: &mut u32) -> u32 {
        *state = state.wrapping_add(0x6d2b_79f5);
        let mut t = *state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        t ^ (t >> 14)
    }

    fn names() -> Vec<String> {
        let mut state = SEED;
        (0..NAMES)
            .map(|_| {
                let length = (next(&mut state) % 41) as usize;
                let mut name = String::new();
                while name.chars().count() < length {
                    name.push_str(TOKENS[next(&mut state) as usize % TOKENS.len()]);
                }
                name
            })
            .collect()
    }

    #[test]
    fn strips_like_the_342b2c09_regexes_for_20000_generated_names() {
        let names = names();
        for markers in PROFILES {
            let mut prepared = Markers::new(markers);
            let mut changed = 0;
            for name in &names {
                let expected = reference::strip(name, markers);
                changed += usize::from(&expected != name);
                assert_eq!(
                    prepared.strip(name),
                    expected,
                    "name {name:?} with markers {markers:?} (seed {SEED})"
                );
            }
            eprintln!("{markers:?}: {changed} of {NAMES} names changed");
            // The generator must exercise marker removal, not only pass names through.
            assert!(changed > NAMES / 2, "{markers:?}: {changed}");
        }
    }

    #[test]
    fn keeps_the_342b2c09_results_sol_found_changed() {
        for (stem, markers, slug) in [
            ("Panel[c]-_[a]", PROFILES[0], "Panel"),
            ("Panel[a]-[c]_Clip", PROFILES[1], "Panel_Clip"),
            ("Panel_[ς]_Clip", PROFILES[2], "Panel_Clip"),
        ] {
            assert_eq!(strip_markers(stem, markers), slug, "{stem}");
            assert_eq!(reference::strip(stem, markers), slug, "{stem}");
        }
    }
}
