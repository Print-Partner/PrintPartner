// @vitest-environment node
import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const UI_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const FONTS_DIR = path.join(UI_ROOT, "src/fonts");
const read = (relative: string) => readFileSync(path.join(UI_ROOT, relative), "utf8");

type Face = { family: string; weight: string; file: string };

function fontFaces(css: string): Face[] {
  return [...css.matchAll(/@font-face\s*{([^}]*)}/g)].map(([, body]) => ({
    family: /font-family:\s*"([^"]+)"/.exec(body)?.[1] ?? "",
    weight: /font-weight:\s*(\d+)/.exec(body)?.[1] ?? "",
    file: /src:\s*url\("([^"]+)"\)\s*format\("woff2"\);/.exec(body)?.[1] ?? "",
  }));
}

describe("bundled fonts", () => {
  const faces = fontFaces(read("src/fonts/fonts.css"));

  it("declares Plex Sans 400/500/600 and Plex Mono 400/500, latin and latin-ext", () => {
    const keys = faces.map((face) => `${face.family} ${face.weight}`).sort();
    expect(keys).toEqual(
      [
        "IBM Plex Mono 400",
        "IBM Plex Mono 500",
        "IBM Plex Sans 400",
        "IBM Plex Sans 500",
        "IBM Plex Sans 600",
      ].flatMap((key) => [key, key]),
    );
  });

  it("declares each latin-ext face before its latin face, so latin wins the overlap", () => {
    // The later @font-face wins where unicode ranges overlap, such as U+0131 and U+0153.
    for (const [index, face] of faces.entries()) {
      const subset = index % 2 === 0 ? "latin-ext" : "latin";
      expect(face.file).toMatch(new RegExp(`-${subset}-${face.weight}-normal\\.woff2$`));
      if (index % 2 === 1) expect(faces[index - 1].family + faces[index - 1].weight).toBe(face.family + face.weight);
    }
  });

  it("points every face at a WOFF2 file in this package", () => {
    for (const face of faces) {
      expect(face.file).toMatch(/^\.\/[a-z0-9-]+\.woff2$/);
      const bytes = readFileSync(path.join(FONTS_DIR, face.file));
      expect(bytes.subarray(0, 4).toString("latin1")).toBe("wOF2");
    }
    const declared = new Set(faces.map((face) => face.file.slice(2)));
    const shipped = readdirSync(FONTS_DIR).filter((name) => name.endsWith(".woff2"));
    expect(new Set(shipped)).toEqual(declared);
  });

  it("ships the OFL text with the font files", () => {
    const licence = read("src/fonts/OFL.txt");
    expect(licence).toContain("SIL OPEN FONT LICENSE Version 1.1");
    expect(licence).toContain("IBM Plex Sans: Copyright 2019 IBM Corp.");
    expect(licence).toContain("IBM Plex Mono: Copyright 2017 IBM Corp.");
  });

  it.each(["index.html", "src/fonts/fonts.css", "src/typography.css", "src/index.css"])(
    "%s names no remote origin or local() font source",
    (file) => {
      const text = read(file);
      expect(text).not.toMatch(/https?:\/\/|url\(\s*["']?\/\/|@import|local\(/i);
    },
  );
});
