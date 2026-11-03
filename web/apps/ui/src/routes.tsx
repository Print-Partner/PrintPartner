import type { RouteObject } from "react-router";
import { NotFound, Placeholder, Shell } from "./shell/Shell";

export const routes: RouteObject[] = [
  {
    path: "/",
    element: <Shell />,
    children: [
      { index: true, element: <Placeholder /> },
      { path: "*", element: <NotFound /> },
    ],
  },
];
