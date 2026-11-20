//! Role-marker removal for part slugs: the algorithm, rules and cap of Node's
//! `stripMarkers` (`web/packages/domain/src/parsers.ts`), whose comment states
//! them. Characters compare through `regress`, as the old regexes did.

use std::collections::HashMap;

/// Repeat rounds before one combined scan finishes. Results equal the old
/// repeat-until-stable regexes for names nested up to this many alternating
/// levels; past it, the kept separator may differ. Node's `MARKER_ROUNDS`
/// has the same value.
const MAX_ROUNDS: usize = 32;

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
#[derive(Default)]
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
        let mut prepared = Self::default();
        for marker in markers {
            // The old code skipped a marker whose regex did not compile.
            let whole: String = marker.chars().map(escape).collect();
            if marker.is_empty() || regress::Regex::with_flags(&whole, "i").is_err() {
                continue;
            }
            let classes: Option<Vec<usize>> = marker.chars().map(|c| prepared.intern(c)).collect();
            if let Some(classes) = classes {
                prepared.markers.push((strips_anywhere(marker), classes));
            }
        }
        prepared
    }

    /// `c`'s class, adding one for it when it matches no marker character yet.
    fn intern(&mut self, c: char) -> Option<usize> {
        if let Some(class) = self.class(c) {
            return Some(class);
        }
        let regex = regress::Regex::with_flags(&format!("^(?:{})$", escape(c)), "i").ok()?;
        self.characters.push(regex);
        self.classes.insert(c, Some(self.characters.len() - 1));
        Some(self.characters.len() - 1)
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
                strip_everywhere(&slug, &[marker])
            } else {
                strip_prefix(slug, marker)
            };
        }
        let anywhere: Vec<&[usize]> = self
            .markers
            .iter()
            .filter(|(anywhere, _)| *anywhere)
            .map(|(_, marker)| marker.as_slice())
            .collect();
        // Every removal shortens the name, so an unchanged length means no change.
        let mut before = name.len();
        for round in 0..=MAX_ROUNDS {
            if slug.len() == before {
                break;
            }
            before = slug.len();
            if round == MAX_ROUNDS {
                slug = strip_everywhere(&slug, &anywhere);
                break;
            }
            for marker in &anywhere {
                slug = strip_everywhere(&slug, &[marker]);
            }
        }
        slug.into_iter().map(|(c, _)| c).collect()
    }
}

/// Removes `markers` from `stem`; see `stripMarkers` in Node.
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

/// Removes every occurrence of `markers`, leftmost first; with several, the
/// first in role order that the stack ends with. Each step shortens the stack
/// or consumes input.
fn strip_everywhere(input: &[Unit], markers: &[&[usize]]) -> Vec<Unit> {
    let mut out: Vec<Unit> = Vec::with_capacity(input.len());
    let mut next = 0;
    while let Some(&unit) = input.get(next) {
        next += 1;
        out.push(unit);
        while let Some(marker) = markers
            .iter()
            .find(|m| out.len() >= m.len() && matches(&out[out.len() - m.len()..], m))
        {
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
        let sep = units.get(marker.len()).is_some_and(|(c, _)| separator(*c));
        units.drain(..marker.len() + usize::from(sep));
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 342b2c09's `strip_marker`, `strip_exposed_markers` and the preview's
    /// two passes, with compiled regexes cached.
    fn reference(name: &str, markers: &[&str]) -> String {
        thread_local! {
            static REGEXES: std::cell::RefCell<HashMap<String, Option<regress::Regex>>> =
                Default::default();
        }
        let strip_marker = |stem: &str, marker: &str| {
            let anywhere = strips_anywhere(marker);
            let escaped: String = marker.chars().map(escape).collect();
            let pattern = if anywhere {
                format!("([ _-]?){escaped}([ _-]?)")
            } else {
                format!("^(){escaped}([ _-]?)")
            };
            let regex = REGEXES.with(|cache| {
                let mut cache = cache.borrow_mut();
                let entry = cache.entry(pattern.clone());
                entry
                    .or_insert_with(|| regress::Regex::with_flags(&pattern, "i").ok())
                    .clone()
            });
            let Some(regex) = regex else {
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
        };
        let mut slug = name.to_owned();
        for marker in markers {
            slug = strip_marker(&slug, marker);
        }
        if slug != name {
            loop {
                let before = slug.len();
                for marker in markers.iter().filter(|m| strips_anywhere(m)) {
                    slug = strip_marker(&slug, marker);
                }
                if slug.len() == before {
                    break;
                }
            }
        }
        slug
    }

    /// Role-ordered markers of the profiles in `parsers.test.ts`.
    const PROFILES: [&[&str]; 4] = [
        &["[a]", "[c]", "[o]"],
        &["[a]-[c]", "[a]", "[c]", "[o]"],
        &["[Σ]", "[c]", "[o]"],
        &["A", "[c]", "[o]"],
    ];

    /// `parsers.test.ts`'s tokens, generator (mulberry32) and seed.
    #[rustfmt::skip]
    const TOKENS: [&str; 30] = [
        "[", "]", "a", "A", "c", "C", "o", "O", "-", "_", " ", ".", "P", "n", "l", "x", "2",
        "Σ", "ς", "σ", "é", "中", "[a]", "[A]", "[c]", "[o]", "[Σ]", "[ς]", "[a]-[c]", "[a[c]]",
    ];
    const SEED: u32 = 1396;
    const NAMES: usize = 20_000;

    fn names() -> Vec<String> {
        let mut state = SEED;
        let mut next = || {
            state = state.wrapping_add(0x6d2b_79f5);
            let mut t = state;
            t = (t ^ (t >> 15)).wrapping_mul(t | 1);
            t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
            t ^ (t >> 14)
        };
        (0..NAMES)
            .map(|_| {
                let length = (next() % 41) as usize;
                let mut name = String::new();
                while name.chars().count() < length {
                    name.push_str(TOKENS[next() as usize % TOKENS.len()]);
                }
                name
            })
            .collect()
    }

    /// Names of at most 47 characters nest fewer than `MAX_ROUNDS` levels,
    /// so every result must equal the reference.
    #[test]
    fn strips_like_the_342b2c09_regexes_for_20000_generated_names() {
        let names = names();
        for markers in PROFILES {
            let mut prepared = Markers::new(markers);
            let mut changed = 0;
            for name in &names {
                let expected = reference(name, markers);
                changed += usize::from(&expected != name);
                assert_eq!(prepared.strip(name), expected, "{name:?} with {markers:?}");
            }
            // The generator must exercise removal, not only pass names through.
            assert!(changed > NAMES / 2, "{markers:?}: {changed} changed");
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
            assert_eq!(reference(stem, markers), slug, "{stem}");
        }
    }

    /// `Panel` + `[c` and `[a` each wrapping `levels` of `[a[c`…`]]`: the old
    /// regexes need `levels` repeat rounds. `parsers.test.ts` pins the same.
    fn capped(levels: usize) -> String {
        let nest = format!("{}{}", "[a[c".repeat(levels), "]]".repeat(levels));
        format!("Panel[c{nest}]-_[a{nest}]")
    }

    #[test]
    fn matches_the_old_regexes_at_32_alternating_levels_and_scans_once_at_33() {
        let markers = PROFILES[0];
        assert_eq!(reference(&capped(32), markers), "Panel");
        assert_eq!(strip_markers(&capped(32), markers), "Panel");
        // Past the cap the combined scan keeps the `-` the rounds would drop.
        assert_eq!(reference(&capped(33), markers), "Panel");
        assert_eq!(strip_markers(&capped(33), markers), "Panel-");
    }

    #[test]
    fn strips_30000_characters_of_alternating_nesting_within_5_seconds() {
        let name = format!("P_{}{}_C", "[a[c".repeat(5000), "]]".repeat(5000));
        let started = std::time::Instant::now();
        assert_eq!(strip_markers(&name, PROFILES[0]), "P_C");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }
}
