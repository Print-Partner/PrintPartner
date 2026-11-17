import type { ReactNode } from "react";
import { Navigate, type RouteObject } from "react-router";
import { LoginScreen, SetupScreen } from "./auth/AuthScreens";
import { BuildCheckoff } from "./builds/BuildCheckoff";
import { BuildHistory } from "./builds/BuildHistory";
import { BuildPlan } from "./builds/BuildPlan";
import { BuildProduction } from "./builds/BuildProduction";
import {
  BuildOverview,
  BUILD_TABS,
  BuildWorkspace,
  type BuildTabPath,
} from "./builds/BuildWorkspace";
import { BuildsList } from "./builds/BuildsList";
import { FarmLog } from "./farm/FarmLog";
import { FarmScreen } from "./farm/FarmScreen";
import { BuildSources } from "./library/BuildSources";
import { LibraryScreen } from "./library/LibraryScreen";
import { MatchReview } from "./farm/MatchReview";
import { DESTINATIONS } from "./frame/destinations";
import { Frame } from "./frame/Frame";
import { DestinationScreen, NotFound, StatesSpecimen } from "./screen/screens";
import { SettingsScreen } from "./settings/SettingsScreen";
import { PRINTER_DISPLAY_STATES, PrinterStatus } from "./status/PrinterStatus";
import { TodayScreen } from "./today/TodayScreen";
import { TypeSpecimen } from "./type/TypeSpecimen";

function PrinterStates() {
  return (
    <>
      <h1>Printer states</h1>
      <ul className="printer-states">
        {PRINTER_DISPLAY_STATES.map((state) => (
          <li key={state}>
            <PrinterStatus state={state} />
          </li>
        ))}
      </ul>
    </>
  );
}

/** Every Build tab's screen. A new tab without one is a type error, not a placeholder. */
const BUILD_TAB_SCREENS: Record<BuildTabPath, ReactNode> = {
  sources: <BuildSources />,
  plan: <BuildPlan />,
  production: <BuildProduction />,
  checkoff: <BuildCheckoff />,
  history: <BuildHistory />,
};

export const routes: RouteObject[] = [
  // Outside the frame: nothing on these screens needs a session.
  { path: "/login", element: <LoginScreen /> },
  { path: "/setup", element: <SetupScreen /> },
  {
    path: "/",
    element: <Frame />,
    children: [
      { index: true, element: <Navigate to="/today" replace /> },
      ...DESTINATIONS.map((d) => ({
        path: d.path.slice(1),
        element:
          d.path === "/builds" ? (
            <BuildsList />
          ) : d.path === "/today" ? (
            <TodayScreen />
          ) : d.path === "/farm" ? (
            <FarmScreen />
          ) : d.path === "/library" ? (
            <LibraryScreen />
          ) : d.path === "/settings" ? (
            <SettingsScreen />
          ) : (
            <DestinationScreen destination={d} />
          ),
      })),
      {
        path: "builds/:id",
        element: <BuildWorkspace />,
        children: [
          { index: true, element: <BuildOverview /> },
          ...BUILD_TABS.flatMap((t) =>
            t.path === "" ? [] : [{ path: t.path, element: BUILD_TAB_SCREENS[t.path] }],
          ),
        ],
      },
      { path: "library/:sourceId", element: <LibraryScreen /> },
      { path: "farm/match", element: <MatchReview /> },
      { path: "farm/log", element: <FarmLog /> },
      { path: "states/printers", element: <PrinterStates /> },
      { path: "states/:kind", element: <StatesSpecimen /> },
      { path: "type", element: <TypeSpecimen /> },
      { path: "*", element: <NotFound /> },
    ],
  },
];
