// Shared by the printer Checkoff parity generators (printer-checkoff-verify,
// printer-file-assignment).
// Generated ids and times differ per run. Seeded times use 2020, so any other ISO time is "now".
export function normalizer() {
  const ids = new Map();
  const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
  const iso = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/;
  const walk = (value) => {
    if (Array.isArray(value)) return value.map(walk);
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, walk(v)]));
    if (typeof value !== 'string') return value;
    if (uuid.test(value)) {
      if (!ids.has(value)) ids.set(value, '<uuid:' + ids.size + '>');
      return ids.get(value);
    }
    return iso.test(value) && !value.startsWith('2020-') ? '<now>' : value;
  };
  return walk;
}
