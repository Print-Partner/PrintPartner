import type { RouteObject } from "react-router";
import { NotFound, Placeholder, Shell } from "./shell/Shell";
import { TypeSpecimen } from "./type/TypeSpecimen";

export const routes: RouteObject[] = [
  {
    path: "/",
    element: <Shell />,
    children: [
      { index: true, element: <Placeholder /> },
      { path: "type", element: <TypeSpecimen /> },
      { path: "*", element: <NotFound /> },
    ],
  },
];
