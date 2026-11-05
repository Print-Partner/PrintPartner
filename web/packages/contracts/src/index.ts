/** Shared API types between web client and server. */

export * from "./source-naming.js";
export * from "./source-category-tree.js";
export * from "./accepted-plates.js";
export * from "./accepted-media.js";
export * from "./plan-drafts.js";
export * from "./production-setup.js";
export * from "./build-workflow.js";
export * from "./reference-sharing.js";
export * from "./hosted-planning.js";
export * from "./tenant-features.js";
export * from "./checkoff-burst.js";
export * from "./plan-features.js";

export type DeployMode = "self-host" | "saas";

/**
 * External tools are layered because MCP authenticates through API keys.
 * Keeping this as one mode prevents the impossible "MCP on, API keys off"
 * combination from reaching either the UI or server.
 */
export type ExternalAccessMode = "off" | "api" | "api_and_mcp";

export type ExternalAccessSettings = Readonly<{
  mode: ExternalAccessMode;
}>;

export const EXTERNAL_ACCESS_DEFAULT: ExternalAccessMode = "api_and_mcp";

export function isExternalAccessMode(value: unknown): value is ExternalAccessMode {
  return value === "off" || value === "api" || value === "api_and_mcp";
}

export function externalApiAccessEnabled(mode: ExternalAccessMode): boolean {
  return mode === "api" || mode === "api_and_mcp";
}

export function mcpAccessEnabled(mode: ExternalAccessMode): boolean {
  return mode === "api_and_mcp";
}

/** Global preference controlling how timestamps (e.g. "Generated …", "Last synced …") are displayed. */
export type DateFormatId = "mdy_12h" | "dmy_12h" | "mdy_short" | "dmy_short" | "ymd_24h" | "iso";

export const DATE_FORMAT_DEFAULT: DateFormatId = "mdy_12h";

export const DATE_FORMAT_PRESETS: Array<{
  id: DateFormatId;
  label: string;
  example: string;
}> = [
  {
    id: "mdy_12h",
    label: "Jul 3, 2026, 9:01 PM",
    example: "Jul 3, 2026, 9:01 PM",
  },
  {
    id: "dmy_12h",
    label: "3 Jul 2026, 9:01 PM",
    example: "3 Jul 2026, 9:01 PM",
  },
  {
    id: "mdy_short",
    label: "07/03/2026, 9:01 PM",
    example: "07/03/2026, 9:01 PM",
  },
  { id: "dmy_short", label: "03/07/2026, 21:01", example: "03/07/2026, 21:01" },
  { id: "ymd_24h", label: "2026-07-03 21:01", example: "2026-07-03 21:01" },
  {
    id: "iso",
    label: "2026-07-03T21:01:32.727Z (ISO)",
    example: "2026-07-03T21:01:32.727Z",
  },
];

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/** Formats an ISO timestamp per the given global date-format preference. Works in both browser and Node (Intl is available in both). */
export function formatTimestamp(iso: string | null | undefined, formatId: DateFormatId = DATE_FORMAT_DEFAULT): string {
  if (!iso) return "";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  switch (formatId) {
    case "iso":
      return date.toISOString();
    case "ymd_24h":
      return `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())} ${pad2(date.getHours())}:${pad2(date.getMinutes())}`;
    case "mdy_short":
      return new Intl.DateTimeFormat("en-US", {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "numeric",
        minute: "2-digit",
      }).format(date);
    case "dmy_short":
      return new Intl.DateTimeFormat("en-GB", {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        hour12: false,
      }).format(date);
    case "dmy_12h":
      return new Intl.DateTimeFormat("en-GB", {
        day: "numeric",
        month: "short",
        year: "numeric",
        hour: "numeric",
        minute: "2-digit",
        hour12: true,
      }).format(date);
    case "mdy_12h":
    default:
      return new Intl.DateTimeFormat("en-US", {
        month: "short",
        day: "numeric",
        year: "numeric",
        hour: "numeric",
        minute: "2-digit",
        hour12: true,
      }).format(date);
  }
}

export type ApiError = {
  detail: string;
  code?: string;
  title?: string;
  status?: number;
  type?: string;
};

/**
 * Validates a Discord incoming-webhook URL: must be
 * `https://discord.com/api/webhooks/<numeric id>/<non-empty token>`
 * (also accepts the `ptb.` / `canary.` subdomains Discord itself uses,
 * and a trailing slash). Returns null when valid, or a human-readable
 * error message when invalid. Shared between the server route validator
 * and the Settings form's inline client-side check so both sides agree.
 */
export function validateDiscordWebhookUrl(url: string): string | null {
  const DISCORD_WEBHOOK_RE =
    /^https:\/\/(?:(?:ptb|canary)\.)?discord(?:app)?\.com\/api\/webhooks\/(\d+)\/([^/?#]+)\/?$/;
  const match = DISCORD_WEBHOOK_RE.exec(url.trim());
  if (!match) {
    return "Webhook URL must look like https://discord.com/api/webhooks/<id>/<token>";
  }
  const [, id, token] = match;
  if (!id || id.length === 0) {
    return "Webhook URL must include a numeric webhook id";
  }
  if (!token || token.trim().length === 0) {
    return "Webhook URL must include a non-empty token";
  }
  return null;
}

export const JOB_KINDS = [
  "sync",
  "import-scan",
  "extract-source-docs",
  "check-source-updates",
  "export-stl-pack",
  "export-checklist-html",
  "export-kit-bundle",
  "export-accepted-plate-3mf",
  "export-direct-3mf",
  "printer-upload",
] as const;

export type JobKind = (typeof JOB_KINDS)[number];

export type JobStartResponse = {
  job_id: string;
};

export type ExportArtifact = {
  path?: string;
  download_url: string | null;
  kind?: string;
  job_id?: string;
  created_at?: string;
  manifest_path?: string | null;
};

export type FleetPreset = {
  id: string;
  name: string;
  bed_width_mm: number;
  bed_depth_mm: number;
  bed_height_mm?: number;
  margin_mm?: number;
  enabled?: boolean;
};

export type IntegrationType =
  | "moonraker"
  | "prusalink"
  | "bambu"
  | "spoolman"
  | "slicer_sidecar"
  | "ai_assistant"
  | "home_assistant";

export type IntegrationConfig = Record<string, unknown>;

/**
 * What a linked host can actually do, derived from its adapter.
 *
 * The server owns this matrix. A client that re-derives it from the
 * integration type gets it wrong the moment a provider gains a capability.
 */
export type IntegrationCapabilities = {
  /** Browse and open already-sliced files on the host. */
  files: boolean;
  /** Serve camera views through the server proxy. */
  cameras: boolean;
  /** Report print state, which is what makes a print attributable to a Build. */
  status: boolean;
};

export type IntegrationSummary = {
  id: string;
  type: IntegrationType;
  name: string;
  config: IntegrationConfig;
  created_at: string;
  updated_at: string;
  /**
   * Derived from the adapter on read. Never persisted, so it cannot go stale
   * against a provider that gained or lost a capability.
   */
  capabilities?: IntegrationCapabilities;
};

export type IntegrationTestResult = {
  ok: boolean;
  message?: string;
};

export type DeviceSummary = {
  id: string;
  name: string;
  type?: string;
  status?: string;
};

/** Live printer host status from IntegrationAdapter.getStatus. */
export type PrinterHostState = "idle" | "printing" | "paused" | "complete" | "error" | "offline" | "unknown";

export type PrinterHostStatus = {
  state: PrinterHostState;
  progress?: number;
  filename?: string;
  message?: string;
  eta_seconds?: number;
  ip_address?: string;
  uptime_seconds?: number;
  nozzle_temperature_c?: number;
  nozzle_target_c?: number;
  bed_temperature_c?: number;
  bed_target_c?: number;
};

/**
 * One entry in a printer host's print-file storage.
 *
 * `path` is relative to the host's print-file root and is the only identifier:
 * every provider addresses an entry by the path it reported, so a second opaque
 * id would only ever be a copy of it.
 */
export type PrinterStorageEntry =
  | {
      kind: "directory";
      path: string;
      name: string;
      modified_at?: string;
    }
  | {
      kind: "file";
      path: string;
      name: string;
      size_bytes?: number;
      modified_at?: string;
      /** Provider revision or etag, where the host supplies one. */
      provider_revision?: string;
    };

export type PrinterStoredDirectory = Extract<PrinterStorageEntry, { kind: "directory" }>;
export type PrinterStoredFile = Extract<PrinterStorageEntry, { kind: "file" }>;

/** One directory of a printer host's print-file storage. */
export type PrinterStorageListing = {
  /** The directory this listing describes. The empty string is the storage root. */
  path: string;
  entries: PrinterStorageEntry[];
};

/**
 * Provider-side identity of a linked file, recorded so PrintPartner can tell
 * that the file at a linked path changed after it was linked.
 */
export type PrinterFileIdentity = {
  size_bytes?: number;
  modified_at?: string;
  provider_revision?: string;
  /** SHA-256 of the bytes PrintPartner downloaded. */
  sha256?: string;
};

/** Why a linked remote file no longer matches the identity recorded for it. */
export type PrinterFileDriftReason = "missing" | "size" | "modified" | "revision" | "content";

export type PrinterFileDrift = {
  reason: PrinterFileDriftReason;
  detected_at: string;
};

/**
 * What a .3mf container actually holds. A 3MF is a ZIP/OPC package that may
 * carry only geometry, so it is not necessarily printer instructions.
 */
export type ThreeMfKind =
  | "slicer_project"
  | "model_package"
  | "toolpath_package"
  | "unsupported";

/** What PrintPartner proved about a print file, by inspecting its bytes. */
export type PrintFileClassification =
  | { format: "gcode" }
  | { format: "bgcode" }
  | { format: "3mf"; kind: ThreeMfKind };

/** True only for a classification PrintPartner can send to a printer as-is. */
export function isPrintReady(classification: PrintFileClassification): boolean {
  switch (classification.format) {
    case "gcode":
    case "bgcode":
      return true;
    case "3mf":
      return classification.kind === "toolpath_package";
    default: {
      const _exhaustive: never = classification;
      return _exhaustive;
    }
  }
}

const MANUAL_INTEGRATION_PREFIX = "manual:";

/**
 * The integration id for a printer tracked by hand. A manual printer has no
 * host polling it, so its links carry this synthetic id instead of a real one.
 */
export function manualIntegrationId(printerId: string): string {
  return `${MANUAL_INTEGRATION_PREFIX}${printerId}`;
}

/** Whether `integrationId` is the manual id, optionally for one specific printer. */
export function isManualIntegrationId(integrationId: string, printerId?: string): boolean {
  return printerId === undefined
    ? integrationId.startsWith(MANUAL_INTEGRATION_PREFIX)
    : integrationId === manualIntegrationId(printerId);
}

/**
 * Stands in for the printer that made a print PrintPartner does not manage.
 *
 * Recording a finished print must not require registering hardware. The whole
 * reason a file is uploaded rather than picked off a host is that PrintPartner
 * cannot talk to the machine that ran it, and it may not be in the fleet at
 * all. Its links carry this id so they are still attributable and still read
 * as manual.
 */
export const UNMANAGED_PRINTER_ID = "unmanaged";

/** What the operator sees where a printer name would go. */
export const UNMANAGED_PRINTER_NAME = "A printer PrintPartner does not manage";

/** True for a link recorded against no registered printer. */
export function isUnmanagedPrinterId(printerId: string): boolean {
  return printerId === UNMANAGED_PRINTER_ID;
}

/** A camera view that Print Partner can render without exposing host credentials. */
export type PrinterCamera = {
  id: string;
  name: string;
  /** MJPEG is rendered continuously; snapshots are refreshed by the user. */
  view: "mjpeg" | "snapshot";
  service?: string;
  aspect_ratio?: string;
};

/** One Progress unit linked to a sent print job (verify-first Phase D). */
export type PrinterCheckoffUnit = {
  part_id: number;
  unit_index: number;
  object_name?: string;
};

export type PrinterHostOutcome = "unknown" | "success" | "failed" | "cancelled";

export type PrinterCheckoffLinkState =
  | "watching"
  | "awaiting_verify"
  | "host_failed"
  | "dismissed"
  | "verified"
  /** @deprecated Legacy auto-tick; treated as verified when loading. */
  | "applied";

export type PrintRejectReason =
  | "bed_adhesion"
  | "layer_shift"
  | "warping"
  | "stringing"
  | "under_extrusion"
  | "over_extrusion"
  | "dimensional"
  | "collision"
  | "wrong_filament"
  | "other";

export type PrintOutcomeResult = "confirmed" | "rejected";

export type PrintVerifyDecision = {
  part_id: number;
  unit_index: number;
  result: PrintOutcomeResult;
  reason?: PrintRejectReason;
  note?: string;
};

export type PrintOutcomeEvent = {
  id: string;
  at: string;
  profile_id: number;
  part_id: number;
  unit_index: number;
  result: PrintOutcomeResult;
  reason?: PrintRejectReason;
  note?: string;
  host_integration_id?: string;
  filename?: string;
  match_key?: string;
  role?: string;
  filament_display?: string;
  link_id?: string;
};

export type PrinterObjectMapping = {
  object_index: number;
  part_id: number;
  unit_index: number;
};

export type PrintFileMatchReview = {
  notices?: string[];
  objects: Array<{ object_index: number; name: string }>;
  parts: Array<{
    part_id: number;
    filename: string;
    relative_path: string;
    source_label?: string;
    units: PrinterCheckoffUnit[];
  }>;
};

export type PrintOutcomesSummary = {
  profile_id: number;
  total_confirmed: number;
  total_rejected: number;
  by_reason: Partial<Record<PrintRejectReason, number>>;
  by_role: Record<string, { confirmed: number; rejected: number }>;
  recent_rejected: PrintOutcomeEvent[];
};

/**
 * Durable job ↔ Progress units mapping for verify-first checkoff.
 * Created at Export send with immutable profile_id (plan). Host `complete` →
 * awaiting_verify (user confirms). Plan-only links (no units) bind farm status.
 */
export type AdditionalPrintResult =
  | { result: "confirmed" }
  | { result: "rejected"; reason: PrintRejectReason; note?: string };

export type AdditionalPrintDecision = AdditionalPrintResult & { index: number };

export type ImportedPrintInventory = {
  extras: Array<{
    name: string;
    kind: "object" | "file";
    checkoff: { result: "pending" } | (AdditionalPrintResult & { checked_at: string });
  }>;
};

export type PrinterCheckoffLink = {
  id: string;
  profile_id: number;
  integration_id: string;
  printer_id: string;
  host_name: string;
  filename: string;
  remote_path?: string;
  upload_job_id?: string;
  /**
   * The Accepted Plan revision this link was created against. Together with
   * profile_id it pins the link to one Production work package.
   */
  plan_revision_id?: number;
  /** Provider identity of remote_path when the link was created. */
  remote_identity?: PrinterFileIdentity;
  /** Set once the provider file at remote_path stops matching remote_identity. */
  remote_drift?: PrinterFileDrift;
  /** How the print file's bytes classified when PrintPartner inspected them. */
  classification?: PrintFileClassification;
  units: PrinterCheckoffUnit[];
  /** Parsed object names that did not map to Progress units (visible, not confirmable). */
  unlabeled_names?: string[];
  imported_inventory?: ImportedPrintInventory;
  /** Units already confirmed/rejected via verify API. */
  resolved_units?: PrintVerifyDecision[];
  state: PrinterCheckoffLinkState;
  host_outcome?: PrinterHostOutcome;
  /** True after we observe printing/paused for this filename. */
  saw_active: boolean;
  /** Upload used start=true — allow complete with cleared filename before first poll. */
  started?: boolean;
  /** Last observed print progress (0–100) while this link was active. */
  last_progress?: number;
  created_at: string;
  /** When host finished successfully (entered awaiting_verify). */
  completed_at?: string;
  applied_at?: string;
  units_marked?: number;
};

/** Reconcile side-effect (no longer auto-ticks Progress). */
export type PrinterCheckoffReconcileUpdate = {
  link_id: string;
  host_name: string;
  profile_id: number;
  filename: string;
  event: "awaiting_verify" | "host_failed";
  host_outcome: PrinterHostOutcome;
  units_pending: number;
};

export type PrinterSendQueueState = "queued" | "sending" | "done" | "error" | "cancelled";

/** Legacy queue match value. Dispatch always uses the item's exact printer_id. */
export type PrinterSendQueueMatch = "pinned" | "compatible";

/** Durable outbound send queue item (Phase F thin farm assist). */
export type PrinterSendQueueItem = {
  id: string;
  filename: string;
  /** Absolute path under data/exports/printer-uploads/… */
  artifact_path: string;
  /** Printer this sliced file was prepared for. Dispatch never reassigns it. */
  printer_id: string;
  /** Older rows may contain `compatible`; dispatch still uses printer_id. */
  match?: PrinterSendQueueMatch;
  /** When true, wait for host Idle before dispatch (default). */
  wait_for_idle: boolean;
  start: boolean;
  profile_id?: number;
  checkoff_units?: PrinterCheckoffUnit[];
  state: PrinterSendQueueState;
  created_at: string;
  updated_at: string;
  upload_job_id?: string;
  error?: string;
  host_name?: string;
};

export type PrinterUploadResult = {
  ok: boolean;
  remote_path?: string;
  started?: boolean;
  message?: string;
  checkoff_link_id?: string;
  checkoff_units?: number;
};

export type WebhookEvent =
  | "job.done"
  | "job.error"
  | "source.synced"
  | "source.updated"
  | "source.sync_failed"
  | "source.update_available"
  | "print.verified"
  | "print.rejected"
  | "plan.exported";

export type WebhookRegistration = {
  id: string;
  url: string;
  events: WebhookEvent[];
  secret?: string | null;
  created_at: string;
};

export type ApiV1Index = {
  version: string;
  openapi: string;
  health: string;
};

export type RuntimeReleaseIdentity = {
  version: string;
  runtime_version: string;
  commit: string | null;
  tag: string | null;
  image_digest: string | null;
  deployment_mode: DeployMode;
  github_release_url: string | null;
  build_date: string | null;
};

export type HealthResponse = {
  ok: boolean;
  version: string;
  deploy_mode: DeployMode;
  multi_user?: boolean;
  /** Whether this request carried a valid user session. */
  authenticated?: boolean;
  authentication_required?: boolean;
  registration_open?: boolean;
  data_dir: string;
  port?: number;
  api_version?: string;
  capabilities?: string[];
  release?: RuntimeReleaseIdentity;
  db?: {
    connected: boolean;
    driver: string;
    postgres: boolean | null;
  };
  /**
   * Optional Google Drive (GIS) client id for parts-manifest open/save.
   * Public OAuth Web client id only — never a client secret.
   */
  google_drive?: {
    client_id: string | null;
  };
};

/** Response from GET /settings/update-check — app release availability. */
export type AppUpdateCheckResponse = {
  enabled: boolean;
  update_available: boolean;
  current_version: string;
  latest_version: string | null;
  release_url: string | null;
  release_notes_url: string | null;
  deploy_mode: DeployMode;
  checked_at: string | null;
};

export type AcceptedProgressUnavailableReason =
  "compatibility_dirty" | "uninitialized" | "integrity" | "concurrent_update";

export type AcceptedProgressSummary =
  | {
      readonly kind: "ready";
      readonly total_units: number;
      readonly remaining_units: number;
    }
  | { readonly kind: "empty" }
  | {
      readonly kind: "unavailable";
      readonly reason: AcceptedProgressUnavailableReason;
    };

export type ProfileSummary = {
  readonly id: number;
  readonly name: string;
  readonly order_number: string | null;
  /** Quiet operator note (e.g. contact customer before printing). */
  readonly special_request: string | null;
  readonly part_count: number;
  readonly accepted_progress: AcceptedProgressSummary;
  /** Compatibility flag for a definite stale Plan; use `freshness` for reasons. */
  readonly build_stale: boolean;
  /** Accepted Source/naming identity compared with the current Plan inputs. */
  readonly freshness: PlanFreshness;
  /** ISO timestamp when archived as a template; null if active. */
  readonly archived_at: string | null;
  /** ISO timestamp of last spine selection. */
  readonly last_used_at: string | null;
};

export type LegacyProfileSummaryV1 = Omit<ProfileSummary, "accepted_progress"> & {
  readonly remaining_units: number;
  readonly total_units: number;
};

export type PlanStaleReason =
  | {
      readonly kind: "source_revision_changed";
      readonly source_id: number;
      readonly source_name: string;
      readonly accepted_revision_id: number;
      readonly current_revision_id: number;
    }
  | {
      readonly kind: "source_revision_unavailable";
      readonly source_id: number;
      readonly source_name: string;
      readonly accepted_revision_id: number;
    }
  | {
      readonly kind: "naming_rules_changed";
      readonly source_id: number;
      readonly source_name: string;
      readonly accepted_digest: string;
      readonly current_digest: string;
    }
  | { readonly kind: "plan_inputs_invalid" }
  | { readonly kind: "plan_configuration_changed" };

export type PlanUntrackedReason =
  | { readonly kind: "no_accepted_inputs" }
  | {
      readonly kind: "source_revision_untracked";
      readonly source_id: number;
      readonly source_name: string;
    };

export type PlanFreshness =
  | {
      readonly status: "current";
      readonly accepted_input_set_id: number;
      readonly accepted_at: string;
    }
  | {
      readonly status: "stale";
      readonly accepted_input_set_id: number;
      readonly accepted_at: string;
      readonly reasons: readonly [PlanStaleReason, ...PlanStaleReason[]];
      readonly untracked_sources: readonly PlanUntrackedReason[];
    }
  | {
      readonly status: "untracked";
      readonly accepted_input_set_id: number | null;
      readonly accepted_at: string | null;
      readonly reasons: readonly [PlanUntrackedReason, ...PlanUntrackedReason[]];
    };

export type SourceSummary = {
  id: number;
  name: string;
  url: string;
  source_kind: string;
  source_type: string;
  /** @deprecated library uses `category`; kept for API compat */
  role: string;
  category: string | null;
  branch: string;
  tag: string | null;
  local_path: string | null;
  /** Server-side Source content is available without exposing its filesystem path. */
  content_available?: boolean;
  last_synced_at: string | null;
  last_commit_sha: string | null;
  current_source_revision_id: number | null;
  docs_url: string | null;
  manifest_community_slug: string | null;
  metadata: Record<string, unknown> | null;
  naming_use_defaults?: boolean;
  update_status?: "up_to_date" | "updates_available" | "unknown" | null;
  update_checked_at?: string | null;
  /** Count of synced markdown/PDF docs indexed for this source. */
  doc_count?: number;
  /** Observed catalog metadata version. Present only while the desktop rebuild guard is on. */
  catalog_metadata_version?: number;
};

export type SourceRevision = {
  readonly id: number;
  readonly source_id: number;
  readonly upstream_revision_key: string;
  readonly manifest_digest: string;
  readonly snapshot_locator: string;
  readonly synced_at: string;
  readonly completeness: "complete";
};

export type PlanRevisionInput = {
  readonly source_id: number;
  readonly source_layer: string;
  readonly layer_order: number;
  readonly tracking_kind: "revision" | "untracked";
  readonly source_revision_id: number | null;
  readonly manifest_digest: string | null;
  readonly effective_naming_digest: string;
};

export type PlanRevisionInputSet = {
  readonly id: number;
  readonly plan_id: number;
  readonly recorded_at: string;
  readonly published_at: string;
  readonly format_version: 1 | 2;
  readonly inputs: readonly PlanRevisionInput[];
};

export type PartRow = {
  id: number;
  match_key: string;
  relative_path: string;
  filename: string;
  source_layer: string | null;
  status: string;
  role: string | null;
  requirement: string | null;
  option_group_id: string | null;
  included: boolean;
  filament_color_id: string | null;
  filament_custom_hex?: string | null;
  spoolman_spool_id?: string | null;
  filament_display?: string;
  filament_hex?: string | null;
  quantity_auto: number;
  quantity_override: number | null;
  quantity_effective: number;
};

/** Plan review / checkoff sheet row (print progress + filament display). */
export type ReviewPart = PartRow & {
  print_units: boolean[];
  printed_count: number;
  /** Assembly tracking: which completed units have been physically installed. */
  assembled_units?: boolean[];
  /** Checkoff: not fully printed yet. */
  missing: boolean;
  /** On-disk STL absent for an included part. */
  stl_missing?: boolean;
  /** Included part has STL but no cached thumbnail PNG. */
  thumb_empty?: boolean;
  filament_display: string;
  spool_summary?: Array<{ remaining_g: number; spool_id: number }>;
  spool_badge?: string | null;
};

export type JobStatus = "pending" | "running" | "done" | "error" | "cancelled";

export type JobEvent = {
  status: JobStatus | string;
  message: string;
  progress: number | null;
  result: Record<string, unknown> | null;
  error: string | null;
};

export type JobSnapshot = JobEvent & {
  job_id: string;
  kind: string;
  finished_at?: string | null;
};

/** AI assistant provider id (server config / status). */
export type AiProviderId = "anthropic" | "openai" | "ollama" | "none";

/** Web search backend for assistant research tools. */
export type SearchProviderId = "anthropic-native" | "openai-native" | "brave" | "exa" | "duckduckgo" | "none";

export type SearchSetupOption = {
  id: SearchProviderId;
  label: string;
  summary: string;
  setup: string;
};

export type AssistantStatus = {
  enabled: boolean;
  provider: AiProviderId;
  model: string | null;
  /** When true, other accessible plans are summarized as few-shot examples (not model training). */
  use_other_builds_as_examples: boolean;
  /** Whether the active provider adapter exposes native tool calling. */
  tools_supported: boolean;
  /** Where active AI provider settings came from (`settings` = UI integration). */
  source?: "settings" | "env" | "none";
  /** Soft daily request cap (`null` / omitted = unlimited). */
  daily_request_budget?: number | null;
  /** Soft daily estimated-token cap (`null` / omitted = unlimited). */
  daily_token_budget?: number | null;
  /** Requests used today (UTC) for this tenant. */
  daily_requests_used?: number;
  /** Estimated tokens used today (UTC) for this tenant. */
  daily_tokens_used?: number;
  /**
   * Active web search backend + setup guidance for all options.
   * `configured` is true when the resolved provider has what it needs (e.g. Brave/Exa key)
   * without exposing secrets.
   */
  search?: {
    provider: SearchProviderId;
    configured: boolean;
    options: SearchSetupOption[];
  };
};

/**
 * Documented `ai_assistant` integration config keys (stored as IntegrationConfig bag).
 * Secrets (`api_key`, `search_api_key`) are redacted in list/get responses.
 *
 * - provider, model, api_key, base_url / ollama_url
 * - max_tokens, use_other_builds_as_examples, daily_*_budget, enabled
 * - search_provider (`SearchProviderId` | null/auto), search_api_key
 * - allow_url_ingest, guide_ingest_max_bytes, ollama_num_ctx
 */

export type AssistantChatRole = "user" | "assistant" | "system";

export type AssistantChatMessage = {
  role: AssistantChatRole;
  content: string;
};

/** Mutating assistant action — never applied until POST /assistant/actions/apply. */
export type AssistantActionType =
  | "apply_stack_preset"
  | "set_base"
  | "add_addon"
  | "remove_layer"
  | "update_kit_selections"
  | "start_recompute"
  | "start_sync"
  | "propose_source_mapping"
  | "propose_set_source_category"
  | "propose_create_source_category"
  | "propose_rename_source_category"
  | "propose_delete_source_category"
  | "set_source_git_ref"
  | "apply_build_recipe"
  | "restore_plan_snapshot"
  | "create_plan_snapshot"
  | "propose_add_source"
  | "import_guide_notes"
  | "propose_exclude_replaced_parts"
  | "duplicate_plan"
  | "archive_plan"
  | "propose_create_build"
  | "propose_import_build_inputs"
  | "propose_update_source"
  | "propose_import_source_files"
  | "propose_edit_plan_draft_parts"
  | "propose_update_source_naming"
  | "propose_add_build_checklist_items"
  | "propose_add_custom_filament"
  | "propose_import_3mf_checkoff"
  | "propose_set_plan_progress"
  | "propose_set_plan_assembly"
  | "propose_verify_printer_checkoff"
  | "propose_set_build_source_roles"
  | "propose_update_build_brief"
  | "propose_resolve_build_differences"
  | "propose_assign_role_filament"
  | "propose_rebuild_plan"
  | "propose_apply_plan_draft"
  /** Client-executed UI opens — auto-run; do not require Apply. */
  | "ui_navigate"
  | "ui_open_source"
  | "ui_open_docs"
  | "ui_highlight_part"
  | "ui_focus_stl_search"
  | "ui_focus_kit_option";

export const ASSISTANT_UI_ACTION_TYPES = [
  "ui_navigate",
  "ui_open_source",
  "ui_open_docs",
  "ui_highlight_part",
  "ui_focus_stl_search",
  "ui_focus_kit_option",
] as const satisfies readonly AssistantActionType[];

export function isAssistantUiAction(type: string): boolean {
  return (ASSISTANT_UI_ACTION_TYPES as readonly string[]).includes(type);
}

export type AssistantProposedAction = {
  id: string;
  type: AssistantActionType;
  plan_id: number;
  label: string;
  summary: string;
  params: Record<string, unknown>;
};

export type PlanDecisionKind = "applied_action" | "dismissed_action" | "user_note" | "choice";

export type PlanDecisionActor = "assistant" | "user";

export type PlanDecision = {
  id: number;
  plan_id: number;
  created_at: string;
  actor: PlanDecisionActor;
  kind: PlanDecisionKind;
  action_type: string | null;
  params: Record<string, unknown>;
  label: string;
  summary: string;
  rationale: string | null;
  result: Record<string, unknown> | null;
};

export type PlanDecisionsResponse = {
  decisions: PlanDecision[];
};

export type PlanDecisionCreateRequest = {
  kind?: PlanDecisionKind;
  actor?: PlanDecisionActor;
  action_type?: string | null;
  params?: Record<string, unknown>;
  label?: string;
  summary?: string;
  rationale?: string | null;
  result?: Record<string, unknown> | null;
};

export type ManifestSelection = string | string[];

export type ManifestSelections = Record<string, ManifestSelection>;

export type BuildRecipe = {
  plan_id: number;
  plan_name: string;
  base: {
    source_name: string | null;
    project_id: number | null;
    tag: string | null;
    branch: string | null;
  };
  addons: Array<{
    source_name: string;
    project_id: number;
    tag: string | null;
    branch: string | null;
  }>;
  stack_preset: string | null;
  kit_selections: ManifestSelections;
  include: string[];
  exclude: string[];
  decision_count: number;
  markdown: string;
};

export type PlanSnapshotSource = "user" | "assistant" | "pre_apply";

export type PlanSnapshotSummary = {
  id: number;
  plan_id: number;
  name: string;
  created_at: string;
  source: PlanSnapshotSource;
};

export type PlanSnapshot = PlanSnapshotSummary & {
  payload: Record<string, unknown>;
};

export type PlanSnapshotsResponse = {
  snapshots: PlanSnapshotSummary[];
};

export type PlanSnapshotCreateRequest = {
  name?: string;
  source?: PlanSnapshotSource;
};

export type AssistantActionDismissRequest = {
  action: AssistantProposedAction;
};

export type AssistantChatRequest = {
  messages: AssistantChatMessage[];
  plan_id?: number;
  /** When true (default), response is SSE. When false, JSON `{ message }`. */
  stream?: boolean;
  /**
   * Override Settings flag for this request. When omitted, uses integration/env
   * `use_other_builds_as_examples` (default true). Examples are context only — not training.
   */
  use_other_builds_as_examples?: boolean;
};

export type AssistantChatResponse = {
  message: AssistantChatMessage;
  /** Proposed mutations; apply only via POST /assistant/actions/apply after user confirm. */
  proposed_actions?: AssistantProposedAction[];
  /** True when tools were unavailable and context stuffing was used instead. */
  tools_degraded?: boolean;
};

export type AssistantActionApplyRequest = {
  action: AssistantProposedAction;
};

export type AssistantActionApplyResponse = {
  ok: boolean;
  detail?: string;
  job_id?: string;
  result?: Record<string, unknown>;
};

export type AssistantFeedbackRating = "up" | "down";

export type AssistantFeedbackRequest = {
  rating: AssistantFeedbackRating;
  /** Short excerpt of the assistant message being rated (no full transcript required). */
  message_excerpt?: string;
  plan_id?: number;
  comment?: string;
};

export type AssistantFeedbackResponse = {
  ok: boolean;
  id: string;
};

export type AssistantHistoryMessage = AssistantChatMessage & {
  id: string;
  created_at: string;
  /**
   * Pending confirm-to-apply cards for this assistant turn.
   * Persisted so reopen does not lose Apply/Dismiss UI.
   */
  proposed_actions?: AssistantProposedAction[];
};

export type AssistantHistoryResponse = {
  messages: AssistantHistoryMessage[];
};

export type UnattributedPrintCandidate = {
  stl_basename: string;
  copy_count: number;
  matching_filenames: string[];
};

export type UnattributedPrint = {
  id: string;
  integration_id: string;
  printer_id: string;
  host_name: string;
  filename: string;
  completed_at: string;
  gcode_objects: string[];
  candidates: UnattributedPrintCandidate[];
  claimed_at?: string;
  claimed_profile_id?: number;
};
export * from "./filename-grouping.js";
