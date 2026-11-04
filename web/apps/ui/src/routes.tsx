import { Navigate, type RouteObject } from "react-router";
import { DESTINATIONS } from "./frame/destinations";
import { Frame } from "./frame/Frame";
import { DestinationScreen, NotFound, StatesSpecimen } from "./screen/screens";
import { PRINTER_DISPLAY_STATES, PrinterStatus } from "./status/PrinterStatus";
import { TypeSpecimen } from "./type/TypeSpecimen";

function PrinterStates() {
  return (
    <>
      <h2>Printer states</h2>
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
        element: (
          <DestinationScreen destination={d}>
            {d.path === "/farm" && <PrinterStates />}
          </DestinationScreen>
        ),
      })),
      { path: "states/:kind?", element: <StatesSpecimen /> },
      { path: "type", element: <TypeSpecimen /> },
      { path: "*", element: <NotFound /> },
    ],
  },
];
