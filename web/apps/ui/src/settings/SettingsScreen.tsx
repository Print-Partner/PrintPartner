import type { Health } from "@print-partner/rust-client";
import { useQuery } from "@tanstack/react-query";
import { useId, type ReactNode } from "react";
import { healthQuery, type ModuleName, type ModuleStatus } from "../core/modules";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { ScreenTemplate, type ScreenState } from "../screen/ScreenTemplate";

/** What each Module adds when it is on, in the order GET /health lists them. */
const MODULE_WORDS: Record<ModuleName, { name: string; adds: string }> = {
  farm: { name: "Farm", adds: "Printers, the send queue and the farm log" },
  spoolman: { name: "Spoolman", adds: "Filament stock from Spoolman and shortfalls per Build" },
  orders: { name: "Orders", adds: "Orders and due dates on Builds" },
  bambu: { name: "Bambu", adds: "Bambu printers on the local network" },
  assembly_tracking: { name: "Assembly tracking", adds: "Assembly progress for printed parts" },
};

/** Why a Module is off, from the reason codes in docs/desktop-rebuild.md. */
const OFF_REASONS: Record<Extract<ModuleStatus, { state: "off" }>["reason"], string> = {
  default_off: "Off by default. You have not turned it on.",
  turned_off: "You turned it off.",
  setting_unreadable: "Off because its setting could not be read. The core tries again.",
};

/** The Settings sections, in page order. Only Modules has a screen so far. */
const SECTIONS = [
  { id: "modules", label: "Modules" },
  { id: "integrations", label: "Integrations" },
  { id: "sharing", label: "Sharing and devices" },
  { id: "backups", label: "Backups" },
] as const;

type Modules = NonNullable<Health["modules"]>;

function ModuleList({ modules }: { modules: Modules }) {
  if (modules.length === 0) return <p>The core lists no Modules.</p>;
  return (
    <dl className="pp-facts pp-modules">
      {modules.map((m) => {
        const words = MODULE_WORDS[m.module];
        return (
          <div key={m.module} className="pp-fact" data-module={m.module} data-module-state={m.state}>
            <dt>{words.name}</dt>
            <dd>
              {m.state === "on" ? "On" : "Off"}
              {m.state === "off" && <span className="pp-fact-detail">{OFF_REASONS[m.reason]}</span>}
              <span className="pp-fact-detail">{words.adds}</span>
            </dd>
          </div>
        );
      })}
    </dl>
  );
}

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
        <p>Module state comes from the core. Switching a Module on or off here is not available yet.</p>
        <ModuleList modules={modules} />
      </Section>
      {SECTIONS.slice(1).map((s) => (
        <Section key={s.id} id={s.id} label={s.label}>
          <p>Not available yet.</p>
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
 * from GET /health; the other sections land in later PRs. Settings will be where a Module is
 * turned on (GRE-1337), so it never shows the module-off state itself.
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
