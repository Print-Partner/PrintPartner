# @print-partner/ui

The replacement Print Partner UI. It talks only to the Rust core. The design it
builds toward is [UI and UX target](../../../docs/product/ui-ux-target.md);
[ADR 0008](../../../docs/adr/0008-replace-the-old-screens.md) says why it is a new
package instead of a port of `apps/web`.

Today it is a placeholder shell: an app frame, a core reachability line, a list
of the printer states, and a Not found route. Product screens arrive in their own slices.

## Commands

Run from the repository root after `npm --prefix web ci`:

```sh
npm --prefix web run dev --workspace @print-partner/ui
npm --prefix web run build --workspace @print-partner/ui
npm --prefix web run typecheck --workspace @print-partner/ui
npm --prefix web run lint --workspace @print-partner/ui
npm --prefix web run test --workspace @print-partner/ui
```

`just ci --only ui-package` runs typecheck, lint, test and build. The legacy root
`build`, `typecheck` and `test` scripts do not include this package.

## Talking to the Rust core

- The dev server listens on `127.0.0.1:5174` and forwards `/api`, `/ws` and
  `/health` to `PP_UI_CORE_URL`, for example
  `PP_UI_CORE_URL=http://127.0.0.1:8080`. The value must be an http(s) origin
  with no path. There is no default: without it the dev server forwards nothing,
  so it cannot reach the Node server by accident. `core-proxy.ts` holds this
  logic and `test/core-proxy.test.ts` tests it.
- UI code calls the core through `fetchCore` in `src/core/client.ts`. It accepts
  same-origin paths only and sends same-origin credentials. In the desktop app
  and Docker the Rust core serves this UI, so the paths reach it directly.

## Import boundary

The ESLint rule `print-partner/ui-boundary` (`web/eslint-rules/ui-boundary.js`)
applies to every file in this package. It fails:

- any relative import that resolves outside `web/apps/ui`, such as
  `../../web/src/App` or `../../../packages/contracts/src/index`;
- any `@print-partner/*` package that is not on its allowlist, including
  `@print-partner/contracts`, `@print-partner/domain`, `@print-partner/server`
  and `@print-partner/web`.

`test/boundary.test.ts` plants each of those imports and expects the rule to
fail it. Do not add `paths` or Vite aliases that point outside this package.

## Choices

- **Router:** React Router 7 (`react-router`), the library `apps/web` already
  uses. TanStack Router is compared against it in the Builds slice.
- **Server state:** TanStack Query 5 is the one cache. Routes and core calls
  must not keep a second copy.
- Both were already in `web/package-lock.json`, so this package added no new
  runtime library.

## Generated Rust client

`@print-partner/rust-client` is the one workspace package on the boundary
allowlist. Build typed calls on it instead of on `fetchCore`, and keep
`fetchCore`'s same-origin rule or move it into the client's transport.

## Printer status

`src/status/PrinterStatus.tsx` renders a printer state as an icon plus a word.
The states, labels, Lucide icon names and colour tokens come from
`printerDisplayDescriptors` in the generated client, which Rust's
`pp_domain::printer_display_state` produces. Do not add a state, label or
colour here; change the Rust enum and regenerate.

- The icon map is keyed by the generated icon names, so a new Rust icon fails
  `typecheck` until it has a component.
- `printerStatusPresentation` throws on an empty label instead of rendering an
  icon-only or blank status.
- Each descriptor names a `--status-*` token defined in `src/index.css`.
  `--interaction` is cyan and is for focus, selection and the primary button
  only; `PrinterStatus.test.tsx` fails if a status token uses it or any other
  cyan.
