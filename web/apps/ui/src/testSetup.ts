// jsdom has no layout, so it lacks the two browser APIs cmdk calls: ResizeObserver for
// the list height and scrollIntoView for the active option. Node-environment tests skip.
if (typeof window !== "undefined") {
  window.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
  Element.prototype.scrollIntoView ??= function scrollIntoView() {};
}
