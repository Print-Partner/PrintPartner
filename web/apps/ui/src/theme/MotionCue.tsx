import { useSyncExternalStore, type ReactNode } from "react";

const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

function reducedMotionQuery(): MediaQueryList | null {
  return typeof window.matchMedia === "function" ? window.matchMedia(REDUCED_MOTION_QUERY) : null;
}

function subscribe(onChange: () => void): () => void {
  const query = reducedMotionQuery();
  query?.addEventListener("change", onChange);
  return () => query?.removeEventListener("change", onChange);
}

const prefersReducedMotion = () => reducedMotionQuery()?.matches ?? false;
const serverSnapshot = () => false;

/** True while the system asks for reduced motion. */
export function usePrefersReducedMotion(): boolean {
  return useSyncExternalStore(subscribe, prefersReducedMotion, serverSnapshot);
}

/**
 * Wraps something that just moved, such as a part that went to a plate. With full
 * motion the subject animates in and `message` is read to assistive technology.
 * With reduced motion nothing animates and `message` is shown as visible text, so
 * the user learns where the thing went either way.
 */
export function MotionCue({ message, children }: { message: string; children: ReactNode }) {
  const reduced = usePrefersReducedMotion();
  return (
    <div className="pp-motion-cue" data-motion={reduced ? "reduced" : "full"}>
      <div className="pp-motion-subject">{children}</div>
      <p role="status" className={reduced ? "pp-motion-text" : "pp-visually-hidden"}>
        {message}
      </p>
    </div>
  );
}
