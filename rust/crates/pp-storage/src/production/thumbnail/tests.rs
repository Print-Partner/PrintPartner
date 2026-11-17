use super::*;

/// The 12 triangles of an axis-aligned box from the origin to `size`.
fn box_triangles(size: [f64; 3]) -> Vec<[[f64; 3]; 3]> {
    let [x, y, z] = size;
    let corner = |i: usize| {
        [
            if i & 1 == 0 { 0.0 } else { x },
            if i & 2 == 0 { 0.0 } else { y },
            if i & 4 == 0 { 0.0 } else { z },
        ]
    };
    let faces = [
        [0, 1, 3, 2],
        [4, 6, 7, 5],
        [0, 4, 5, 1],
        [2, 3, 7, 6],
        [0, 2, 6, 4],
        [1, 5, 7, 3],
    ];
    faces
        .iter()
        .flat_map(|[a, b, c, d]| {
            [
                [corner(*a), corner(*b), corner(*c)],
                [corner(*a), corner(*c), corner(*d)],
            ]
        })
        .collect()
}

fn binary(triangles: &[[[f64; 3]; 3]]) -> Vec<u8> {
    let mut bytes = vec![0u8; 80];
    bytes.extend((triangles.len() as u32).to_le_bytes());
    for triangle in triangles {
        bytes.extend([0u8; 12]);
        for vertex in triangle {
            for value in vertex {
                bytes.extend((*value as f32).to_le_bytes());
            }
        }
        bytes.extend([0u8; 2]);
    }
    bytes
}

fn ascii(triangles: &[[[f64; 3]; 3]]) -> Vec<u8> {
    let mut text = String::from("solid part\n");
    for triangle in triangles {
        text.push_str("facet normal 0 0 0\nouter loop\n");
        for [x, y, z] in triangle {
            text.push_str(&format!("vertex {x} {y} {z}\n"));
        }
        text.push_str("endloop\nendfacet\n");
    }
    text.push_str("endsolid part");
    text.into_bytes()
}

/// Width, height and RGBA pixels of a PNG.
fn decode(png_bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(
        (frame.color_type, frame.bit_depth),
        (png::ColorType::Rgba, png::BitDepth::Eight)
    );
    pixels.truncate(frame.buffer_size());
    (frame.width, frame.height, pixels)
}

fn alpha(pixels: &[u8], x: usize, y: usize) -> u8 {
    pixels[(y * SIZE as usize + x) * 4 + 3]
}

#[test]
fn a_box_renders_a_256_pixel_png_with_the_part_centred() {
    let rendered = render(
        &binary(&box_triangles([20.0, 10.0, 5.0])),
        GeometryLimits::default(),
    );
    let (width, height, pixels) = decode(&rendered.unwrap());

    assert_eq!((width, height), (SIZE, SIZE));
    assert_eq!(alpha(&pixels, 128, 128), 255, "the centre shows the part");
    for (x, y) in [(0, 0), (255, 0), (0, 255), (255, 255)] {
        assert_eq!(alpha(&pixels, x, y), 0, "corner {x},{y} is background");
    }
    let opaque = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] == 255)
        .count();
    assert!(opaque > 10_000, "the part fills the frame: {opaque} pixels");
    let shades: std::collections::BTreeSet<_> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] == 255)
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect();
    assert!(
        shades.len() >= 3,
        "three visible faces, three shades: {shades:?}"
    );
}

#[test]
fn binary_and_ascii_of_the_same_part_render_the_same_bytes() {
    let triangles = box_triangles([30.0, 30.0, 12.0]);
    let limits = GeometryLimits::default();
    let from_binary = render(&binary(&triangles), limits).unwrap();
    let from_ascii = render(&ascii(&triangles), limits).unwrap();
    assert_eq!(from_binary, from_ascii);
    assert_eq!(render(&binary(&triangles), limits).unwrap(), from_binary);
}

#[test]
fn corrupt_or_oversized_stl_is_unreadable() {
    let limits = GeometryLimits::default();
    for bytes in [&b"not an stl"[..], b"", b"solid x\nendsolid x"] {
        assert!(matches!(
            render(bytes, limits),
            Err(ThumbnailError::Unreadable(_))
        ));
    }
    let part = binary(&box_triangles([1.0, 1.0, 1.0]));
    let few = GeometryLimits {
        max_bytes: part.len(),
        max_triangles: 11,
    };
    assert!(matches!(
        render(&part, few),
        Err(ThumbnailError::Unreadable(_))
    ));
    let small = GeometryLimits {
        max_bytes: part.len() - 1,
        max_triangles: 12,
    };
    assert!(matches!(
        render(&part, small),
        Err(ThumbnailError::Unreadable(_))
    ));
}
