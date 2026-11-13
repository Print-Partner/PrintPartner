//! Node's `loadAcceptedArtifactGeometry` (`services/accepted-artifact-geometry.ts`):
//! each Required unit's pinned STL is opened through
//! [`crate::accepted_artifacts::open_verified`] and measured.

use crate::accepted_artifacts::open_verified;
use crate::production::geometry::{GeometryLimits, dimensions_um, parse_accepted_stl};
use crate::read_model::graph::Artifact;
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

/// Node's route limits (`WORKSPACE_LIMITS` in `routes/accepted-plates.ts`).
#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub total_source_bytes: u64,
    pub objects: usize,
    pub triangles: usize,
}
pub(super) const LIMITS: Limits = Limits {
    total_source_bytes: 256 * 1024 * 1024,
    objects: 10_000,
    triangles: 5_000_000,
};

/// Why a unit's geometry could not be loaded, with Node's codes.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Unavailable { token: String, reason: &'static str },
    InvalidStl(String),
    Degenerate(String),
    LimitExceeded(&'static str),
}

/// The width, depth and height of each unit's STL in micrometres by token, or the first failure in unit
/// order. An STL shared by several units is read once, and its triangles
/// count once per unit, as Node counts them.
pub(super) fn load<'a>(
    repos: &Path,
    units: impl ExactSizeIterator<Item = (&'a str, &'a Artifact)>,
    limits: Limits,
) -> Result<HashMap<String, [u64; 3]>, Failure> {
    if units.len() > limits.objects {
        return Err(Failure::LimitExceeded("objects"));
    }
    let mut by_descriptor: HashMap<&Artifact, ([u64; 3], usize)> = HashMap::new();
    let mut by_token = HashMap::new();
    let mut total_bytes = 0u64;
    let mut triangles = 0usize;
    for (token, artifact) in units {
        let unavailable = |reason| Failure::Unavailable {
            token: token.to_owned(),
            reason,
        };
        let (dimensions, count) = match by_descriptor.get(artifact) {
            Some(known) => *known,
            None => {
                let (mut file, size) = open_verified(repos, artifact)
                    .map_err(|failure| unavailable(failure.code()))?;
                total_bytes += size;
                if total_bytes > limits.total_source_bytes {
                    return Err(Failure::LimitExceeded("total_source_bytes"));
                }
                let mut bytes = Vec::new();
                file.seek(SeekFrom::Start(0))
                    .and_then(|_| file.take(size).read_to_end(&mut bytes))
                    .map_err(|_| unavailable("io_error"))?;
                let unbounded = GeometryLimits {
                    max_bytes: usize::MAX,
                    max_triangles: usize::MAX,
                };
                let mesh = parse_accepted_stl(&bytes, unbounded)
                    .map_err(|_| Failure::InvalidStl(token.to_owned()))?;
                let size =
                    dimensions_um(&mesh).map_err(|_| Failure::Degenerate(token.to_owned()))?;
                let dimensions =
                    [size.width_um, size.depth_um, size.height_um].map(|value| value.get());
                by_descriptor.insert(artifact, (dimensions, mesh.triangles));
                (dimensions, mesh.triangles)
            }
        };
        triangles = triangles.saturating_add(count);
        if triangles > limits.triangles {
            return Err(Failure::LimitExceeded("triangles"));
        }
        by_token.insert(token.to_owned(), dimensions);
    }
    Ok(by_token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accepted_artifacts::tests::Snapshot;

    const BOX: &str = "solid box
 facet normal 0 0 0
  outer loop
   vertex 0 0 0
   vertex 10 0 0
   vertex 10 20 5
  endloop
 endfacet
endsolid box
";

    fn load(
        snapshot: &Snapshot,
        units: &[(&str, &Artifact)],
        limits: Limits,
    ) -> Result<HashMap<String, [u64; 3]>, Failure> {
        super::load(&snapshot.repos(), units.iter().copied(), limits)
    }

    fn unavailable(reason: &'static str) -> Result<HashMap<String, [u64; 3]>, Failure> {
        Err(Failure::Unavailable {
            token: "t".into(),
            reason,
        })
    }

    #[test]
    fn measures_a_pinned_stl() {
        let snapshot = Snapshot::new(&[("box.stl", BOX)]);
        let artifact = snapshot.artifact("BOX.stl", BOX);
        let sizes = load(&snapshot, &[("t", &artifact)], LIMITS).unwrap();
        assert_eq!(sizes["t"], [10_000, 20_000, 5_000]);
    }

    #[test]
    fn limits_count_objects_bytes_and_repeated_triangles() {
        let snapshot = Snapshot::new(&[("box.stl", BOX)]);
        let artifact = snapshot.artifact("box.stl", BOX);
        let units = [("t", &artifact), ("u", &artifact)];
        let limit = |objects, total_source_bytes, triangles| Limits {
            total_source_bytes,
            objects,
            triangles,
        };
        let bytes = BOX.len() as u64;
        assert_eq!(
            load(&snapshot, &units, limit(1, bytes, 2)),
            Err(Failure::LimitExceeded("objects"))
        );
        assert_eq!(
            load(&snapshot, &units, limit(2, bytes - 1, 2)),
            Err(Failure::LimitExceeded("total_source_bytes"))
        );
        assert_eq!(
            load(&snapshot, &units, limit(2, bytes, 1)),
            Err(Failure::LimitExceeded("triangles"))
        );
        assert_eq!(
            load(&snapshot, &units, limit(2, bytes, 2)).unwrap().len(),
            2
        );
    }

    #[test]
    fn a_refused_open_names_the_unit_and_its_reason() {
        let snapshot = Snapshot::new(&[("box.stl", BOX)]);
        let gone = snapshot.artifact("gone.stl", BOX);
        assert_eq!(
            load(&snapshot, &[("t", &gone)], LIMITS),
            unavailable("missing")
        );
        let legacy = Artifact::Unavailable { reason: "legacy" };
        assert_eq!(
            load(&snapshot, &[("t", &legacy)], LIMITS),
            unavailable("legacy")
        );
    }
}
