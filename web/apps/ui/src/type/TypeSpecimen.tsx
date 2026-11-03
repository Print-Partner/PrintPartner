import "./specimen.css";

const FACES = [
  { name: "Plex Sans 400", className: undefined, weight: 400 },
  { name: "Plex Sans 500", className: undefined, weight: 500 },
  { name: "Plex Sans 600", className: undefined, weight: 600 },
  { name: "Plex Mono 400", className: "num", weight: 400 },
  { name: "Plex Mono 500", className: "num", weight: 500 },
] as const;

// Sample values only; nothing here is read from the core.
const FIGURES = [
  { role: "Count", values: ["7 / 48", "11 / 120"] },
  { role: "ETA", values: ["0h 41m", "11h 07m"] },
  { role: "Grams", values: ["18.4 g", "1,284.6 g"] },
  { role: "ID", values: ["P-0001", "P-1188"] },
  { role: "Order number", values: ["#10482", "#11111"] },
] as const;

/** Shows every bundled face and the tabular-figure roles, for screenshot checks. */
export function TypeSpecimen() {
  return (
    <section className="specimen">
      <h1>Type</h1>
      <ul className="specimen-faces">
        {FACES.map((face) => (
          <li key={face.name} className={face.className} style={{ fontWeight: face.weight }}>
            {face.name}: Plate 3 of 12, 0123456789
          </li>
        ))}
      </ul>
      <table className="specimen-figures">
        <thead>
          <tr>
            <th scope="col">Role</th>
            <th scope="col">Value</th>
            <th scope="col">Value</th>
          </tr>
        </thead>
        <tbody>
          {FIGURES.map((row) => (
            <tr key={row.role}>
              <th scope="row">{row.role}</th>
              {row.values.map((value) => (
                <td key={value} className="num">
                  {value}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
