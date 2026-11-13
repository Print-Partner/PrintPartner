import type { Health } from "@print-partner/rust-client";
import { useQuery } from "@tanstack/react-query";
import { useId, type ReactNode } from "react";
import { healthQuery } from "../core/modules";
import { IntegrationsSection } from "./IntegrationsSection";
import { ModuleList } from "./ModulesSection";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { ScreenTemplate, type ScreenState } from "../screen/ScreenTemplate";

/** The Settings sections, in page order. Modules are editable and integrations listed; the other sections name their availability. */
const SECTIONS = [
  { id: "modules", label: "Modules" },
  { id: "integrations", label: "Integrations" },
  { id: "sharing", label: "Sharing and devices" },
  { id: "backups", label: "Backups" },
] as const;

type Modules = NonNullable<Health["modules"]>;

function Section({ id, label, children }: { id: string; label: string; children: ReactNode }) {
  const headingId = useId();
  return (
    <section id={id} aria-labelledby={headingId} className="pp-settings-section">
      <h2 id={headingId}>{label}</h2>
      {children}
    </section>
  );
}

function Sections({ modules }: { modules: Modules }) {
  return (
    <>
      <Section id="modules" label="Modules">
        <p>Module state comes from the core. Changing a Module keeps saved Builds and history.</p>
        <ModuleList modules={modules} />
      </Section>
      {SECTIONS.slice(1).map((s) => (
        <Section key={s.id} id={s.id} label={s.label}>
          {s.id === "integrations" ? <IntegrationsSection /> : s.id === "sharing" ? (
            <>
              <p>This screen cannot show sharing status or paired devices yet.</p>
              <p>Share on network is off by default. Pairing, device revocation and HTTPS trust
                are not available here yet.</p>
            </>
          ) : <p>Not available yet.</p>}
        </Section>
      ))}
    </>
  );
}

function SectionLinks() {
  return (
    <nav aria-label="Settings sections" className="pp-tabs">
      <ul>
        {SECTIONS.map((s) => (
          <li key={s.id}>
            <a className="pp-tab" href={`#${s.id}`}>
              {s.label}
            </a>
          </li>
        ))}
      </ul>
    </nav>
  );
}

const hasData = (state: ScreenState<Modules>) =>
  state.kind === "ready" ||
  ((state.kind === "error" || state.kind === "offline") && state.lastData !== undefined);

/**
 * `/settings`: Modules, integrations, sharing and devices, and backups. Module state is read
 * from GET /health and written through setModule. Other sections land as their Rust
 * contracts arrive. Settings stays reachable when a Module is off.
 */
export function SettingsScreen() {
  // The same cached GET /health that useModule reads, seen as its Module list. A core that
  // lists no Modules gives an empty list, which is the empty state.
  const modules = useQuery({ ...healthQuery, select: (health: Health) => health.modules ?? [] });
  const state = queryScreenState(modules, useConnection(), (list) => list.length === 0);
  return (
    <ScreenTemplate
      title="Settings"
      // The section links point into the sections, which exist only once there is data.
      subnav={hasData(state) && <SectionLinks />}
      state={state}
      renderData={(modules) => <Sections modules={modules} />}
      empty={{ message: "This core does not report its Modules, so there is nothing to set yet." }}
    />
  );
}
