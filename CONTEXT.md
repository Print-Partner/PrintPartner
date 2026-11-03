# Print Partner

Print Partner tracks 3D-printed parts from source files to finished machines. One person runs a small print farm, turns customer orders into printable work, and records what physically came off each printer.

## Language

### Build work

**Build**:
The long-lived unit of work the user names, orders and keeps. A Build selects the Sources it is built from and owns its drafts, accepted revisions, plates and checkoff history.
_Avoid_: Order, job, project

**Plan**:
The requirements of a Build at one accepted revision: which files, quantities, roles, colours and manifest options. A Build has one current Plan; older ones stay readable as revisions.
_Avoid_: Profile, build_profile, BOM

**Draft**:
An edited Plan that has not been accepted. Edits from the UI and from an assistant land in the same draft.
_Avoid_: Working plan, pending plan, unsaved changes

**Proposal**:
A specific change waiting for a decision at the Accept gate. A draft may carry several.
_Avoid_: Patch, pending action, edit

**Accept gate**:
The single decision point where a Plan change becomes real. Every Plan change passes through it, including renames, order details, customer, and due date. Checkoff does not pass through it, because checkoff records what already happened.
_Avoid_: Commit, publish, apply, save

**Required unit**:
One physical part a Plan calls for. A Build with three of a bracket has three Required units of that bracket. Identity is fixed when the Plan is accepted and never renumbered.
_Avoid_: Item, instance, copy

**Plate**:
An arrangement of Required units for one print run, produced from an accepted Plan revision. Its files and exports stay bound to that revision.
_Avoid_: Bed, build plate job, batch

### Sources and files

**Source**:
A reference to where model files come from: a Git repository, a local directory or an uploaded archive.
_Avoid_: Project, library item, repo

**Source revision**:
An immutable snapshot of a Source at one sync. A new sync creates a new revision; readers keep the previous one until the new one validates.
_Avoid_: Version, commit, snapshot

**Library**:
The collection of Sources available to pick from, independent of any Build.
_Avoid_: Catalog, repository, store

### Making

**Production**:
Making accepted work: producing plates, exporting files and handing them to a printer or slicer. Production and Checkoff repeat for as long as a Build is open.
_Avoid_: Queue, jobs, dispatch

**Checkoff**:
The record of which Required units have physically come off a printer and been assembled. It records what happened and never changes a Plan.
_Avoid_: Verify, progress, tracking, ticking

**Checkoff burst**:
The group of checkoff records made in one sitting. It is the unit of undo, not a change to the Plan.
_Avoid_: Batch, session

### Farm

**Printer**:
A machine part of the farm. A printer's reported state describes execution only: printing, paused, failed, idle, unreachable, not set up. Whether a bed still needs clearing is a separate fact.
_Avoid_: Device, machine (when a printer is meant), node

**Bed to clear**:
A printer with a finished print still on it. Clearing the bed is the physical act; confirming what came off is Checkoff.
_Avoid_: Finished, done, complete

**Driver**:
The adapter for one printer protocol: Moonraker for Klipper printers, PrusaLink, and status-only for the Bambu H2D.
_Avoid_: Connector, integration, client

**Send**:
Handing a plate's file to a printer. Always an explicit user action; never automatic, never a consequence of a bed becoming free.
_Avoid_: Dispatch, submit, push, start

**Stock**:
Filament inventory. Spoolman is the only stock source; Print Partner never keeps its own count.
_Avoid_: Inventory, spool count, filament db

### Running the app

**Data directory**:
The single folder holding the database, files and secrets for one installation. Exactly one runtime owns it at a time.
_Avoid_: Workspace, data root, volume

**Module**:
An optional product area that can be off: farm and printers, Spoolman, orders, Bambu, assembly tracking. An off module hides its controls and says why; it never erases history.
_Avoid_: Feature flag, plugin, add-on

**Share on network**:
Off by default. When on, other devices on the LAN reach the same install with a signed-in session or a paired device.
_Avoid_: LAN mode, remote access, server mode

**Job**:
Background work the app runs and tracks to completion, such as an export or a printer upload. Distinct from a print job on a printer.
_Avoid_: Task, worker, print job

**Snapshot**:
A restorable saved state of a Build. It is the safety net for a bad accept, not a backup of the installation.
_Avoid_: Backup, checkpoint, version

**Backup**:
A copy of the whole data directory for disaster recovery. Restoring one is not an undo.
_Avoid_: Snapshot, export
