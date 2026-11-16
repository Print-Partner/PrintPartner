import { useLayoutEffect, useRef, useState } from "react";
import { isInView, thumbnailUrl } from "./sourceThumb";

export type ThumbStatus = "off" | "pending" | "done";

const quiet = (_path: string, _status: ThumbStatus) => {};

/** The stand-in for an STL thumbnail that did not load. */
export function ThumbFallback({ failed }: { failed: boolean }) {
  return (
    <span
      className="pp-stl-placeholder"
      role={failed ? "img" : undefined}
      aria-label={failed ? "No thumbnail" : undefined}
    >
      {failed ? "No preview" : null}
    </span>
  );
}

/**
 * One STL thumbnail. The image is requested once the box is in the viewport.
 * A failed response is a placeholder, not a broken image.
 */
export function StlThumb({
  sourceId,
  path,
  report = quiet,
}: {
  sourceId: number;
  path: string;
  report?: (path: string, status: ThumbStatus) => void;
}) {
  const ref = useRef<HTMLSpanElement>(null);
  const [show, setShow] = useState(false);
  const [failed, setFailed] = useState(false);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    if (isInView(element)) {
      setShow(true);
      report(path, "pending");
      return;
    }
    report(path, "off");
    if (typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver((entries) => {
      if (!entries.some((entry) => entry.isIntersecting)) return;
      setShow(true);
      report(path, "pending");
      observer.disconnect();
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [path, report]);
  return (
    <span className="pp-stl-thumb" ref={ref} data-path={path}>
      {show && !failed ? (
        <img
          src={thumbnailUrl(sourceId, path)}
          alt=""
          width={64}
          height={64}
          onLoad={() => report(path, "done")}
          onError={() => {
            setFailed(true);
            report(path, "done");
          }}
        />
      ) : (
        <ThumbFallback failed={failed} />
      )}
    </span>
  );
}
