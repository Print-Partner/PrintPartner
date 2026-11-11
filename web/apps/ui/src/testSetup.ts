import { configure } from "@testing-library/react";
import { afterEach } from "vitest";
import { FakeSocket, resetSockets } from "./testSockets";

// First renders can exceed the default one-second query budget on CPU-loaded runners.
configure({ asyncUtilTimeout: 5_000 });

// jsdom has no layout, so it lacks the two browser APIs cmdk calls: ResizeObserver for
// the list height and scrollIntoView for the active option. Node-environment tests skip.
// The frame opens /ws/printers; tests get a fake socket instead of a network connection.
if (typeof window !== "undefined") {
  window.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
  Element.prototype.scrollIntoView ??= function scrollIntoView() {};
  window.WebSocket = FakeSocket as unknown as typeof WebSocket;
  afterEach(resetSockets);
}
