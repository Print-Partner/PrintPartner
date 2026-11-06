import { Navigate, type RouteObject } from "react-router";
import { BuildOverview, BUILD_TABS, BuildWorkspace, LaterTab } from "./builds/BuildWorkspace";
import { BuildsList } from "./builds/BuildsList";
import { FarmScreen } from "./farm/FarmScreen";
import { DESTINATIONS } from "./frame/destinations";
import { Frame } from "./frame/Frame";
import { DestinationScreen, NotFound, StatesSpecimen } from "./screen/screens";
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

export const routes: RouteObject[] = [
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
          ) : (
            <DestinationScreen destination={d} />
          ),
      })),
      {
        path: "builds/:id",
        element: <BuildWorkspace />,
        children: [
          { index: true, element: <BuildOverview /> },
          ...BUILD_TABS.filter((t) => t.path).map((t) => ({
            path: t.path,
            element: <LaterTab label={t.label} holds={t.holds} />,
          })),
        ],
      },
      { path: "states/printers", element: <PrinterStates /> },
      { path: "states/:kind", element: <StatesSpecimen /> },
      { path: "type", element: <TypeSpecimen /> },
      { path: "*", element: <NotFound /> },
    ],
  },
];
