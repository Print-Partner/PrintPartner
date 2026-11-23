import { useId } from "react";
import { plateName, type CheckoffPart, type CheckoffUnit } from "./checkoff";

type Plate = NonNullable<CheckoffUnit["plate"]>;

/** A Plate of the accepted layout, or the units on none. Exists only because a unit names it. */
interface PlateBox {
  key: string;
  name: string;
  units: number;
  printed: number;
  onPlate: boolean;
}

const tally = (box: PlateBox, unit: CheckoffUnit) => {
  box.units += 1;
  if (unit.printed) box.printed += 1;
};

/** One box per Plate in ordinal order, then one "On no Plate" box when any unit is on none. */
function boxesOf(parts: readonly CheckoffPart[]): PlateBox[] {
  const plates = new Map<string, { plate: Plate; box: PlateBox }>();
  const none: PlateBox = { key: "none", name: "On no Plate", units: 0, printed: 0, onPlate: false };
  for (const unit of parts.flatMap((p) => p.units)) {
    const plate = unit.plate;
    if (!plate) {
      tally(none, unit);
      continue;
    }
    let entry = plates.get(plate.plate_id);
    if (!entry) {
      const box = {
        key: plate.plate_id,
        name: plateName(plate),
        units: 0,
        printed: 0,
        onPlate: true,
      };
      entry = { plate, box };
      plates.set(plate.plate_id, entry);
    }
    tally(entry.box, unit);
  }
  const boxes = [...plates.values()]
    .sort(
      ({ plate: a }, { plate: b }) =>
        a.ordinal - b.ordinal ||
        a.printer_name.localeCompare(b.printer_name) ||
        a.plate_id.localeCompare(b.plate_id),
    )
    .map((entry) => entry.box);
  return none.units > 0 ? [...boxes, none] : boxes;
}

const sentence = (b: PlateBox) =>
  `${b.name}: ${b.units} ${b.units === 1 ? "unit" : "units"}, ${b.printed} printed`;

const share = (n: number, of: number) => String(+((100 * n) / of).toFixed(2));

/**
 * The text beside it says everything this shows, so it is hidden from assistive tech and
 * holds no text. Lengths are relative to the busiest Plate; nothing here stands for bed size.
 */
function BoxDrawing({ box, maxUnits }: { box: PlateBox; maxUnits: number }) {
  return (
    <svg
      className="pp-plate-bar"
      viewBox="0 0 100 10"
      preserveAspectRatio="none"
      aria-hidden="true"
      focusable="false"
    >
      <rect
        data-units
        width={share(box.units, maxUnits)}
        height="10"
        fill="none"
        stroke="currentColor"
        strokeDasharray={box.onPlate ? undefined : "4 3"}
        vectorEffect="non-scaling-stroke"
      />
      <rect data-printed width={share(box.printed, maxUnits)} height="10" fill="currentColor" />
    </svg>
  );
}

/** Each Plate's units and how many are printed, drawn as SVG bars with a sentence for each. */
export function PlatePreview({ parts }: { parts: readonly CheckoffPart[] }) {
  const heading = useId();
  const boxes = boxesOf(parts);
  const maxUnits = Math.max(1, ...boxes.map((b) => b.units));
  return (
    <section aria-labelledby={heading} className="pp-plate-preview">
      <h2 id={heading}>Plate preview</h2>
      <p className="pp-muted">
        Each bar is a Plate of the accepted layout. Its length is the number of units on it, and the
        filled part is the units printed. Bed size and where each part sits are not available here.
      </p>
      {boxes.length === 0 ? (
        <p>The accepted Plates hold no units.</p>
      ) : (
        <ul aria-labelledby={heading}>
          {boxes.map((b) => (
            <li key={b.key}>
              <BoxDrawing box={b} maxUnits={maxUnits} />
              <span>{sentence(b)}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
