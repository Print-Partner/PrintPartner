# Put the Accept gate and the single writer on one path

Status: accepted on 2026-11-02.

The engine owns two rules that the product treats as non-negotiable, and both are enforced by the shape of its interface rather than by convention or by a screen remembering to behave.

The first is the single writer. One process writes to a data directory at a time, and inside that process one writer thread owns SQLite. Only `pp-engine` may link the database crate, so every write is a command the engine executes in one transaction. This is not a new guarantee; it is what the storage crate already does with an advisory lock and a write queue. What changes is that the guarantee becomes checkable per crate instead of depending on every future crate choosing not to open the database.

The second is the Accept gate. Every Plan change is a command that produces a proposal, and only a command from an interactive user actor accepts it. Checkoff is not a Plan change and does not pass the gate, because it records what physically happened. Printer sends are commands only a user actor can construct. The consequence is that the gate is a property of the write path, so the UI, MCP and any future transport cannot disagree about it, and an assistant can stage a change but never publish one.

The interface is `open`, `query`, `execute`, `subscribe` and `shutdown`, with `Command`, `Query`, `View` and `Actor` defined in the model crate. Commands and queries must nest per operation family, as in `Command::Plans(plans::Command)`, rather than sitting in one flat enumeration. The current writer thread funnels every family through a single `enum Envelope` with 16 variants in a 989-line file, and a flat replacement of that enum in the new interface would rebuild the same collision point at a larger size.

Two seams were deliberately not created. Storage gets no trait, because SQLite is the only adapter and tests already open real SQLite in temporary directories; a storage trait would have exactly one implementation. The same reasoning rejects a repository trait per table. Seams are kept only where two or more concrete adapters exist today: printer drivers, printers already have Moonraker, PrusaLink and Bambu; filament stock has Spoolman and a fixture; source fetch has git, local directories and archives; slicer handoff has two native slicers; notifiers have three.

The rejected alternative was to let each transport own its own validation and gating, which is where the current Node server ended up with separate paths for HTTP, MCP and the in-app assistant. That produces the number of Accept gates equal to the number of surfaces, and the drift is invisible until a user finds two different answers to "is this change live".
