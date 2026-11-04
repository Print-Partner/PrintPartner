# Replacement UI tokens

Design tokens for the replacement UI in `web/apps/ui`. The design they implement is the
Design system section of [UI and UX target](ui-ux-target.md). The old app's tokens are
in [Design system](../design-system.md) and do not change.

| File | What it holds |
| --- | --- |
| `web/apps/ui/src/theme/tokens.css` | Every token, per theme, plus the forced-colors and reduced-motion overrides |
| `web/apps/ui/src/theme/theme.css` | Base rules that read the tokens: page colours, focus ring, selection, filament swatch, motion |
| `web/apps/ui/src/theme/tokenGate.ts` | The contrast and colour-role gate |
| `web/apps/ui/src/theme/cssTokens.ts` | CSS parser and WCAG contrast arithmetic, shared with the old app's gate |
| `web/apps/ui/src/theme/MotionCue.tsx` | Wrapper that states where a moved thing went, as text under reduced motion |

## Themes

Dark is the default and applies with no attribute. `data-theme="light"` and
`data-theme="shop-floor"` on the root element switch theme. Shop floor is a
high-contrast dark theme for reading at a distance: text reaches 7:1, body text is
16 px, and rows are 48 px touch targets. `data-density="compact"` switches rows from
40 px to 32 px. No setting stores the theme yet.

## Groups

- **Colour roles.** Four surfaces (`--surface-sunken`, `-base`, `-raised`,
  `-overlay`), ink (`--text`, `--text-muted`), lines (`--line` for decoration,
  `--line-strong` for control edges), interaction, status and filament.
- **Interaction.** `--interaction-accent`, `-accent-hover`, `-on-accent`, `-text`,
  `-focus-ring`, `-selection` and `-on-selection` (selected text, `var(--text)` outside
  forced colors). These are the only cyan tokens: primary button,
  focus ring and selection. Status and filament tokens are never cyan.
  `--interaction-on-accent` must read on both the resting and the hover fill.
- **Status.** `--status-active`, `-positive`, `-attention`, `-danger` and
  `-neutral`. The names match the Rust `StatusColor` enum that the generated
  printer display descriptors use, so they change only with that enum. A status
  always shows an icon and a word too; colour is never the only signal. Pills use
  `--status-pill-radius` (4 px), `-height` and `-padding-x`.
- **Filament.** Swatches are round chips with a border: `--filament-swatch-size`
  (16 px), `-radius` (50%), `-border-width` (1 px), `-border` and `-empty` (no colour
  known). `.pp-filament-swatch` in `theme.css` applies them. A swatch is round and a
  status pill is not, so the two never look alike.
- **Type.** `--font-sans` (IBM Plex Sans) and `--font-mono` (IBM Plex Mono), weights
  400, 500 and 600, `--font-numeric: tabular-nums` for counts, ETAs, grams and IDs,
  and sizes `--text-xs` to `--text-2xl`. The Plex files are not bundled yet, so the
  stacks fall back to system fonts.
- **Density.** `--row-comfortable` 40 px, `--row-compact` 32 px, `--row-height`,
  `--touch-target-min` 44 px and `--touch-target` 48 px.
- **Motion.** `--motion-duration-fast` 120 ms, `-base` 160 ms, `-slow` 200 ms and
  `--motion-ease-out`. Under `prefers-reduced-motion: reduce` every duration is 0 ms,
  and `MotionCue` shows its message as visible text instead of animating.

## Forced colors

Under `forced-colors: active` every colour token maps to a system colour: surfaces to
`Canvas`, ink, lines, status and swatch borders to `CanvasText`, and interaction to
`Highlight`, `HighlightText` and `LinkText`. Selection uses `Highlight` with
`HighlightText`, so selected text stays distinct from the `Canvas` page. Only interaction tokens may use the
highlight family, so status never borrows the system accent.

The forced-colors and reduced-motion blocks select `:root, :root[data-theme]`. The
theme rules are `:root[data-theme="..."]`, which outranks a bare `:root`, so without
the second selector light and shop floor would keep their authored colours and
durations.

`.pp-filament-swatch` sets `forced-color-adjust: none` there: a filament colour is
data, like an image, so it keeps its real colour inside a `CanvasText` border. A
control filled with `--interaction-accent` needs the same opt-out when it lands.
Without it, Chromium draws a Canvas backplate behind the label, and a
`HighlightText` label can disappear against it.

## The gate

```sh
node web/scripts/design-system-contrast.mjs           # print the tables
node web/scripts/design-system-contrast.mjs --write   # rewrite the tables below
node web/scripts/design-system-contrast.mjs --check   # fail on any problem
npm --prefix web run test --workspace @print-partner/ui
```

`--check` and the package tests fail when:

- a text ink is below 4.5:1 (7:1 on shop floor) on any surface, a control edge, the
  accent, the focus ring or a swatch border is below 3:1, or on-accent text (on the
  resting and hover fills) or selection text is below the text minimum, in dark,
  light, shop floor or either forced-colors palette;
- a token outside `--interaction-*` resolves to cyan (hue 165 to 200 degrees,
  saturation 30% or more);
- in any theme under forced colors, a colour token, after following `var()`, is not
  a system colour, resolves to nothing, uses a highlight keyword outside
  `--interaction-*`, or differs from its forced value in dark;
- in either forced-colors palette, `--interaction-selection` is the same colour as a
  surface, which would make selected text look unselected;
- in any theme under reduced motion, alone or with forced colors, a
  `--motion-duration-*` token is above 0 ms (forced colors is also checked with
  reduced motion on);
- `tokens.css` sets a custom property in a way the gate does not model: a selector
  other than `:root`, `:root[attr]` or `:root[attr="value"]`; a rule inside any
  at-rule other than a top-level `@media (forced-colors: active)` or
  `@media (prefers-reduced-motion: reduce)`; `!important`; or a value that is not a
  hex colour, `var(--token)`, a system colour (forced colors only), a length, a
  number, a font stack, `cubic-bezier()` or `tabular-nums`. The gate fails on these
  instead of skipping them;
- the tables below are stale (`--check` only).

The gate evaluates each theme with CSS specificity and source order, so a theme rule
that outranks a media block is caught. The forced-colors tables use the Windows "Night sky" and "Desert" contrast themes as
sample palettes. The browser chooses the real colours.

## Contrast

Ratios by surface. `min` is the ratio the gate requires.

<!-- ui-tokens:start (generated by web/scripts/design-system-contrast.mjs --write) -->

Dark:

| Ink | sunken | base | raised | overlay | min |
| --- | --- | --- | --- | --- | --- |
| text | 15.79 | 14.90 | 13.72 | 12.27 | 4.5 |
| text-muted | 8.29 | 7.82 | 7.20 | 6.44 | 4.5 |
| interaction-text | 10.72 | 10.11 | 9.32 | 8.33 | 4.5 |
| status-positive | 10.93 | 10.32 | 9.50 | 8.50 | 4.5 |
| status-attention | 11.41 | 10.77 | 9.92 | 8.87 | 4.5 |
| status-danger | 6.89 | 6.50 | 5.99 | 5.36 | 4.5 |
| status-active | 7.44 | 7.02 | 6.46 | 5.78 | 4.5 |
| status-neutral | 7.45 | 7.03 | 6.48 | 5.80 | 4.5 |
| line-strong | 4.36 | 4.11 | 3.79 | 3.39 | 3 |
| interaction-accent | 9.00 | 8.49 | 7.82 | 7.00 | 3 |
| interaction-focus-ring | 10.72 | 10.11 | 9.32 | 8.33 | 3 |
| filament-swatch-border | 6.19 | 5.84 | 5.38 | 4.81 | 3 |
| interaction-on-accent on interaction-accent | 8.54 | | | | 4.5 |
| interaction-on-accent on interaction-accent-hover | 10.17 | | | | 4.5 |
| interaction-on-selection on interaction-selection | 10.18 | | | | 4.5 |

Light:

| Ink | sunken | base | raised | overlay | min |
| --- | --- | --- | --- | --- | --- |
| text | 14.89 | 16.32 | 17.65 | 17.65 | 4.5 |
| text-muted | 5.82 | 6.38 | 6.90 | 6.90 | 4.5 |
| interaction-text | 5.54 | 6.07 | 6.57 | 6.57 | 4.5 |
| status-positive | 4.98 | 5.46 | 5.90 | 5.90 | 4.5 |
| status-attention | 5.32 | 5.83 | 6.31 | 6.31 | 4.5 |
| status-danger | 5.51 | 6.04 | 6.54 | 6.54 | 4.5 |
| status-active | 5.13 | 5.63 | 6.08 | 6.08 | 4.5 |
| status-neutral | 5.82 | 6.38 | 6.90 | 6.90 | 4.5 |
| line-strong | 3.24 | 3.56 | 3.85 | 3.85 | 3 |
| interaction-accent | 4.27 | 4.68 | 5.06 | 5.06 | 3 |
| interaction-focus-ring | 4.27 | 4.68 | 5.06 | 5.06 | 3 |
| filament-swatch-border | 4.00 | 4.39 | 4.75 | 4.75 | 3 |
| interaction-on-accent on interaction-accent | 5.06 | | | | 4.5 |
| interaction-on-accent on interaction-accent-hover | 6.57 | | | | 4.5 |
| interaction-on-selection on interaction-selection | 14.39 | | | | 4.5 |

Shop-floor:

| Ink | sunken | base | raised | overlay | min |
| --- | --- | --- | --- | --- | --- |
| text | 21.00 | 20.28 | 19.19 | 17.81 | 7 |
| text-muted | 13.49 | 13.02 | 12.33 | 11.44 | 7 |
| interaction-text | 14.21 | 13.73 | 12.99 | 12.05 | 7 |
| status-positive | 13.40 | 12.94 | 12.25 | 11.37 | 7 |
| status-attention | 13.58 | 13.11 | 12.41 | 11.51 | 7 |
| status-danger | 10.29 | 9.94 | 9.40 | 8.72 | 7 |
| status-active | 12.03 | 11.61 | 10.99 | 10.20 | 7 |
| status-neutral | 13.49 | 13.02 | 12.33 | 11.44 | 7 |
| line-strong | 8.30 | 8.01 | 7.58 | 7.03 | 3 |
| interaction-accent | 12.72 | 12.28 | 11.62 | 10.78 | 3 |
| interaction-focus-ring | 14.74 | 14.24 | 13.47 | 12.50 | 3 |
| filament-swatch-border | 13.49 | 13.02 | 12.33 | 11.44 | 3 |
| interaction-on-accent on interaction-accent | 11.51 | | | | 7 |
| interaction-on-accent on interaction-accent-hover | 13.35 | | | | 7 |
| interaction-on-selection on interaction-selection | 12.35 | | | | 7 |

Forced colors, dark palette:

| Ink | sunken | base | raised | overlay | min |
| --- | --- | --- | --- | --- | --- |
| text | 21.00 | 21.00 | 21.00 | 21.00 | 4.5 |
| text-muted | 21.00 | 21.00 | 21.00 | 21.00 | 4.5 |
| interaction-text | 19.56 | 19.56 | 19.56 | 19.56 | 4.5 |
| status-positive | 21.00 | 21.00 | 21.00 | 21.00 | 4.5 |
| status-attention | 21.00 | 21.00 | 21.00 | 21.00 | 4.5 |
| status-danger | 21.00 | 21.00 | 21.00 | 21.00 | 4.5 |
| status-active | 21.00 | 21.00 | 21.00 | 21.00 | 4.5 |
| status-neutral | 21.00 | 21.00 | 21.00 | 21.00 | 4.5 |
| line-strong | 21.00 | 21.00 | 21.00 | 21.00 | 3 |
| interaction-accent | 14.37 | 14.37 | 14.37 | 14.37 | 3 |
| interaction-focus-ring | 14.37 | 14.37 | 14.37 | 14.37 | 3 |
| filament-swatch-border | 21.00 | 21.00 | 21.00 | 21.00 | 3 |
| interaction-on-accent on interaction-accent | 14.37 | | | | 4.5 |
| interaction-on-accent on interaction-accent-hover | 14.37 | | | | 4.5 |
| interaction-on-selection on interaction-selection | 14.37 | | | | 4.5 |

Forced colors, light palette:

| Ink | sunken | base | raised | overlay | min |
| --- | --- | --- | --- | --- | --- |
| text | 10.43 | 10.43 | 10.43 | 10.43 | 4.5 |
| text-muted | 10.43 | 10.43 | 10.43 | 10.43 | 4.5 |
| interaction-text | 6.94 | 6.94 | 6.94 | 6.94 | 4.5 |
| status-positive | 10.43 | 10.43 | 10.43 | 10.43 | 4.5 |
| status-attention | 10.43 | 10.43 | 10.43 | 10.43 | 4.5 |
| status-danger | 10.43 | 10.43 | 10.43 | 10.43 | 4.5 |
| status-active | 10.43 | 10.43 | 10.43 | 10.43 | 4.5 |
| status-neutral | 10.43 | 10.43 | 10.43 | 10.43 | 4.5 |
| line-strong | 10.43 | 10.43 | 10.43 | 10.43 | 3 |
| interaction-accent | 7.27 | 7.27 | 7.27 | 7.27 | 3 |
| interaction-focus-ring | 7.27 | 7.27 | 7.27 | 7.27 | 3 |
| filament-swatch-border | 10.43 | 10.43 | 10.43 | 10.43 | 3 |
| interaction-on-accent on interaction-accent | 7.00 | | | | 4.5 |
| interaction-on-accent on interaction-accent-hover | 7.00 | | | | 4.5 |
| interaction-on-selection on interaction-selection | 7.00 | | | | 4.5 |

<!-- ui-tokens:end -->
