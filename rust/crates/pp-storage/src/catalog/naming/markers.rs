//! Role-marker removal for part slugs: the algorithm, rules and cap of Node's
//! `stripMarkers` (`web/packages/domain/src/parsers.ts`), whose comment states
//! them. Characters compare through `regress`, as the old regexes did.

use std::collections::HashMap;

/// Repeat rounds before one combined scan finishes. Results equal the old
/// repeat-until-stable regexes for names nested up to this many alternating
/// levels; past it, the kept separator may differ. Node's `MARKER_ROUNDS`
/// has the same value.
const MAX_ROUNDS: usize = 8;

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

/// A symbol-bounded marker as character classes, with its KMP prefix table.
struct Marker {
    classes: Vec<usize>,
    /// `fail[i]`: the longest proper prefix of `classes[..=i]` that is also its suffix.
    fail: Vec<usize>,
}

/// One marker's removal, in role order.
enum Strip {
    Anywhere(Marker),
    /// A letter-bounded marker, removed once at the start; compared at the
    /// current offset through `Markers::prefix_characters`.
    Prefix(Vec<char>),
}

/// A profile's markers, ready to strip from many names.
#[derive(Default)]
pub(crate) struct Markers {
    /// One case-insensitive regex per distinct symbol-bounded marker character.
    characters: Vec<regress::Regex>,
    /// Each character seen so far and the first of `characters` it matches.
    classes: HashMap<char, Option<usize>>,
    /// One case-insensitive regex per distinct prefix-marker character.
    prefix_characters: HashMap<char, Option<regress::Regex>>,
    markers: Vec<Strip>,
}

fn character_regex(c: char) -> Option<regress::Regex> {
    regress::Regex::with_flags(&format!("^(?:{})$", escape(c)), "i").ok()
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
            if !super::symbol_bounded(marker) {
                for c in marker.chars() {
                    let regexes = &mut prepared.prefix_characters;
                    regexes.entry(c).or_insert_with(|| character_regex(c));
                }
                prepared
                    .markers
                    .push(Strip::Prefix(marker.chars().collect()));
                continue;
            }
            let classes: Option<Vec<usize>> = marker.chars().map(|c| prepared.intern(c)).collect();
            if let Some(classes) = classes {
                let mut fail = vec![0; classes.len()];
                let mut q = 0;
                for i in 1..classes.len() {
                    while q > 0 && classes[i] != classes[q] {
                        q = fail[q - 1];
                    }
                    q += usize::from(classes[i] == classes[q]);
                    fail[i] = q;
                }
                prepared
                    .markers
                    .push(Strip::Anywhere(Marker { classes, fail }));
            }
        }
        prepared
    }

    /// `c`'s class, adding one for it when it matches no marker character yet.
    fn intern(&mut self, c: char) -> Option<usize> {
        if let Some(class) = self.class(c) {
            return Some(class);
        }
        self.characters.push(character_regex(c)?);
        self.classes.insert(c, Some(self.characters.len() - 1));
        Some(self.characters.len() - 1)
    }

    fn class(&mut self, c: char) -> Option<usize> {
        let characters = &self.characters;
        *self.classes.entry(c).or_insert_with(|| {
            work(characters.len());
            let text = c.to_string();
            characters.iter().position(|re| re.find(&text).is_some())
        })
    }

    /// How many units a prefix marker and the separator after it take from
    /// the start of `units`; 0 when `units` does not start with it.
    fn prefix(&self, units: &[Unit], marker: &[char]) -> usize {
        let starts = units.len() >= marker.len()
            && units.iter().zip(marker).all(|(&(c, _), m)| {
                work(1);
                let re = self.prefix_characters[m].as_ref();
                re.is_some_and(|re| re.find(c.encode_utf8(&mut [0; 4])).is_some())
            });
        if !starts {
            return 0;
        }
        marker.len() + usize::from(units.get(marker.len()).is_some_and(|(c, _)| separator(*c)))
    }

    pub(crate) fn strip(&mut self, stem: &str) -> String {
        let mut slug: Vec<Unit> = stem.chars().map(|c| (c, self.class(c))).collect();
        let mut before = slug.len();
        // Prefix removals advance `start`; the slug is copied only by a scan.
        let mut start = 0;
        for marker in &self.markers {
            match marker {
                Strip::Anywhere(marker) => {
                    slug = strip_everywhere(&slug[start..], &[marker]);
                    start = 0;
                }
                Strip::Prefix(marker) => start += self.prefix(&slug[start..], marker),
            }
        }
        work(slug.len() - start);
        slug.drain(..start);
        let anywhere: Vec<&Marker> = (self.markers.iter())
            .filter_map(|m| match m {
                Strip::Anywhere(marker) => Some(marker),
                Strip::Prefix(_) => None,
            })
            .collect();
        // Every removal shortens the name, so an unchanged length means no change.
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

/// Pushes `unit` and each marker's KMP state after it onto `states`; returns
/// the length of the first marker in role order that the stack now ends with.
fn push(
    out: &mut Vec<Unit>,
    states: &mut Vec<usize>,
    markers: &[&Marker],
    unit: Unit,
) -> Option<usize> {
    let previous = states.len().saturating_sub(markers.len());
    let mut found = None;
    for j in 0..markers.len() {
        let (classes, fail) = (&markers[j].classes, &markers[j].fail);
        let mut q = if out.is_empty() {
            0
        } else {
            states[previous + j]
        };
        if q == classes.len() {
            q = fail[q - 1];
        }
        while q > 0 && Some(classes[q]) != unit.1 {
            q = fail[q - 1];
        }
        if Some(classes[q]) == unit.1 {
            q += 1;
        }
        if q == classes.len() && found.is_none() {
            found = Some(q);
        }
        states.push(q);
    }
    out.push(unit);
    found
}

/// Removes every occurrence of `markers`, leftmost first; with several, the
/// first in role order that the stack ends with. Each marker's KMP state is
/// kept per stacked character, so a step costs O(1) amortized per marker and
/// a scan is linear in the name's length.
fn strip_everywhere(input: &[Unit], markers: &[&Marker]) -> Vec<Unit> {
    let k = markers.len();
    work(input.len());
    let mut out: Vec<Unit> = Vec::with_capacity(input.len());
    let mut states: Vec<usize> = Vec::with_capacity(input.len() * k);
    let mut next = 0;
    while let Some(&unit) = input.get(next) {
        next += 1;
        let mut matched = push(&mut out, &mut states, markers, unit);
        while let Some(len) = matched {
            out.truncate(out.len() - len);
            let before = out.last().copied().filter(|(c, _)| separator(*c));
            if before.is_some() {
                out.pop();
            }
            states.truncate(out.len() * k);
            let after = input.get(next).copied().filter(|(c, _)| separator(*c));
            if after.is_some() {
                next += 1;
            }
            matched = None;
            if let Some(keep) = before.or(after)
                && !out.is_empty()
                && next < input.len()
            {
                matched = push(&mut out, &mut states, markers, keep);
            }
        }
    }
    out
}

#[cfg(test)]
thread_local!(static WORK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });

/// Counts regex comparisons and copied units, for the cost tests.
fn work(_units: usize) {
    #[cfg(test)]
    WORK.with(|work| work.set(work.get() + _units));
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
            let anywhere = super::super::symbol_bounded(marker);
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
                for marker in markers.iter().filter(|m| super::super::symbol_bounded(m)) {
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

    /// No generated name needs more than `MAX_ROUNDS` rounds, so every
    /// result must equal the reference.
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
    fn matches_the_old_regexes_at_8_alternating_levels_and_scans_once_at_9() {
        let markers = PROFILES[0];
        assert_eq!(reference(&capped(8), markers), "Panel");
        assert_eq!(strip_markers(&capped(8), markers), "Panel");
        // Past the cap the combined scan keeps the `-` the rounds would drop.
        assert_eq!(reference(&capped(9), markers), "Panel");
        assert_eq!(strip_markers(&capped(9), markers), "Panel-");
    }

    /// The same markers and answers as `parsers.test.ts`.
    #[test]
    #[rustfmt::skip]
    fn classifies_symbol_bounded_markers_like_node() {
        for (marker, bounded) in [
            ("[a]", true), ("_x_", true), ("-", true), ("[a]-[c]", true), ("[中]", true),
            ("A", false), ("é]", false), ("[é", false), ("Σ]", false), ("中", false),
            ("x_", false), ("²]", false), ("_7", false),
        ] {
            assert_eq!(super::super::symbol_bounded(marker), bounded, "{marker}");
        }
    }

    fn validate(markers: Vec<String>) -> Result<(), String> {
        let mut profile = super::super::NamingProfile::default();
        profile.roles.truncate(1);
        profile.roles[0].markers = markers;
        profile.validate().map(|_| ()).map_err(|e| e.to_string())
    }

    #[test]
    #[rustfmt::skip]
    fn limits_symbol_bounded_markers_but_not_letter_bounded_ones() {
        assert_eq!(validate((0..513).map(|i| format!("missing-marker-{i}")).collect()), Ok(()));
        assert_eq!(validate(vec!["[a]".into(); 16]), Ok(()));
        assert_eq!(validate(vec!["[a]".into(); 17]), Err("Too many naming markers".into()));
        assert_eq!(validate(vec!["[".repeat(64)]), Ok(()));
        assert_eq!(validate(vec!["[".repeat(65)]), Err("Naming marker too long".into()));
        // Lengths count UTF-16 units, as Node does: each 😀 is two.
        for (marker, ok) in [("😀".repeat(32), true), ("😀".repeat(32) + "!", false),
            (format!("A{}B", "😀".repeat(31)), true), (format!("A{}BB", "😀".repeat(31)), false)] {
            assert_eq!(validate(vec![marker.clone()]).is_ok(), ok, "{marker}");
        }
    }

    /// The most work the limits allow: 14 64-unit markers that match 63 units
    /// then miss, plus `[a]` and `[c]` nested 12 levels so every round runs,
    /// in 30,000 characters. `parsers.test.ts` runs the same case.
    #[test]
    fn strips_the_worst_case_the_limits_allow_within_2_seconds() {
        let mut markers: Vec<String> = (0..14)
            .map(|i| format!("{}{}", "[".repeat(63), char::from_u32(0x2190 + i).unwrap()))
            .collect();
        markers.extend(["[a]".into(), "[c]".into()]);
        assert_eq!(validate(markers.clone()), Ok(()));
        let nest = format!("{}{}", "[a[c".repeat(12), "]]".repeat(12));
        let name = format!("P_{}{nest}_C", "[".repeat(30_000 - 4 - nest.len()));
        let markers: Vec<&str> = markers.iter().map(String::as_str).collect();
        let started = std::time::Instant::now();
        let slug = strip_markers(&name, &markers);
        let elapsed = started.elapsed();
        assert_eq!(slug, format!("P_{}_C", "[".repeat(30_000 - 4 - nest.len())));
        assert!(elapsed < std::time::Duration::from_secs(2), "{elapsed:?}");
    }

    /// Sol's GRE-1870 cases cost work linear in the name: 4,000 prefix
    /// removals, and 1,024 prefix markers against 1,024 other characters.
    /// `parsers.test.ts` counts the same cases.
    #[test]
    fn strips_4000_prefixes_and_checks_1024_prefix_markers_in_linear_work() {
        let counted = |name: &str, markers: &[&str]| {
            WORK.with(|work| work.set(0));
            let mut prepared = Markers::new(markers);
            assert!(prepared.characters.is_empty());
            (prepared.strip(name), WORK.with(std::cell::Cell::get))
        };
        let name = format!("{}Panel", "A".repeat(4000));
        let (slug, work) = counted(&name, &["A"; 4000]);
        assert_eq!((slug.as_str(), work), ("Panel", 4005));
        let char = |i: u32| char::from_u32(i).unwrap();
        let markers: Vec<String> = (0..1024).map(|i| char(0x4e00 + i).to_string()).collect();
        let markers: Vec<&str> = markers.iter().map(String::as_str).collect();
        let name: String = (0..1024).map(|i| char(0x6000 + i)).collect();
        assert_eq!(counted(&name, &markers), (name.clone(), 2048));
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
