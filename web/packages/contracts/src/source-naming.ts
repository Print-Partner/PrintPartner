import { z } from "zod";

export class SourceNamingContractError extends Error {
  constructor(
    readonly field: string,
    message: string,
  ) {
    super(field ? `${field}: ${message}` : message);
    this.name = "SourceNamingContractError";
  }
}

function captureGroupCount(pattern: string): number {
  const match = new RegExp(`(?:${pattern})|`).exec("");
  return match === null ? 0 : match.length - 1;
}

const QuantityRegexSchema = z
  .string()
  .trim()
  .min(1, "must not be blank")
  .superRefine((pattern, context) => {
    try {
      new RegExp(pattern, "i");
    } catch {
      context.addIssue({ code: "custom", message: "is an invalid regular expression" });
      return;
    }
    if (captureGroupCount(pattern) !== 1) {
      context.addIssue({ code: "custom", message: "must contain exactly one capture group" });
    }
  });

export const StlNamingRoleIdSchema = z.enum(["primary", "accent", "clear", "opaque"]);
export type StlNamingRoleId = z.infer<typeof StlNamingRoleIdSchema>;

/**
 * A marker is symbol-bounded when neither its first nor its last character is a letter or
 * digit (Unicode `Alphabetic` or `N`): `[a]`, `_x_` or `-`. Only these are removed anywhere
 * in a name, so only these scan the whole name. Rust's `catalog::naming::symbol_bounded`
 * is the same test.
 */
export function isSymbolBoundedMarker(marker: string): boolean {
  const word = (c?: string) => c !== undefined && /^[\p{Alphabetic}\p{N}]$/u.test(c);
  const characters = [...marker];
  return !word(characters[0]) && !word(characters.at(-1));
}

/** Bounds marker-stripping work per name (UTF-16 units); Rust's `catalog::naming` matches. */
export const MAX_STL_NAMING_MARKER_LENGTH = 64;
export const MAX_STL_NAMING_SYMBOL_MARKERS = 16;
const MarkerSchema = z.string().max(MAX_STL_NAMING_MARKER_LENGTH, "must be at most 64 characters");

export const StlNamingRoleSchema = z.strictObject({
  id: StlNamingRoleIdSchema,
  label: z.string().trim().min(1, "must not be blank"),
  markers: z.array(MarkerSchema),
});
export type StlNamingRole = z.infer<typeof StlNamingRoleSchema>;

const StlNamingRoleOverrideSchema = z.strictObject({
  id: StlNamingRoleIdSchema,
  label: z.string().trim().min(1, "must not be blank").optional(),
  markers: z.array(MarkerSchema).optional(),
});

const StlNamingRoleOverridesSchema = z
  .array(StlNamingRoleOverrideSchema)
  .min(1, "must contain at least one role override")
  .superRefine((roles, context) => {
    const ids = roles.map((role) => role.id);
    if (new Set(ids).size !== ids.length) {
      context.addIssue({ code: "custom", message: "role ids must be unique" });
    }
  });

const StlNamingRolesSchema = z
  .array(StlNamingRoleSchema)
  .min(1, "must contain at least one role")
  .superRefine((roles, context) => {
    const ids = roles.map((role) => role.id);
    if (new Set(ids).size !== ids.length) {
      context.addIssue({ code: "custom", message: "role ids must be unique" });
    }
    if (!ids.includes("primary")) {
      context.addIssue({ code: "custom", message: "must include the primary role" });
    }
    const scanning = roles.flatMap((role) => role.markers).filter((m) => m.trim() && isSymbolBoundedMarker(m));
    if (scanning.length > MAX_STL_NAMING_SYMBOL_MARKERS) {
      context.addIssue({ code: "custom", message: "must contain at most 16 symbol-bounded markers" });
    }
  });

export const StlNamingFolderRuleSchema = z.strictObject({
  path_contains: z.string().trim().min(1, "must not be blank"),
  role_id: StlNamingRoleIdSchema,
  functional_class: z.enum(["functional", "cosmetic"]).optional(),
});
export type StlNamingFolderRule = z.infer<typeof StlNamingFolderRuleSchema>;

const StlNamingQuantitySchema = z.strictObject({
  regex: QuantityRegexSchema,
  default: z.number().finite().int().min(1),
});

const StlNamingSlugSchema = z.strictObject({
  strip_markers: z.boolean(),
  strip_quantity: z.boolean(),
});

const StlNamingExportRoleOrderSchema = z
  .array(StlNamingRoleIdSchema)
  .length(4)
  .superRefine((roles, context) => {
    if (new Set(roles).size !== 4) {
      context.addIssue({
        code: "custom",
        message: "must list each role id exactly once",
      });
    }
  });

export const StlNamingProfileSchema = z.strictObject({
  roles: StlNamingRolesSchema,
  quantity: StlNamingQuantitySchema,
  slug: StlNamingSlugSchema,
  folder_rules: z.array(StlNamingFolderRuleSchema),
  export_role_order: StlNamingExportRoleOrderSchema,
});
export type StlNamingProfile = z.infer<typeof StlNamingProfileSchema>;

export const StlNamingProfileOverrideSchema = z.strictObject({
  roles: StlNamingRoleOverridesSchema.optional(),
  quantity: StlNamingQuantitySchema.partial().optional(),
  slug: StlNamingSlugSchema.partial().optional(),
  folder_rules: z.array(StlNamingFolderRuleSchema).optional(),
  export_role_order: StlNamingExportRoleOrderSchema.optional(),
});
export type StlNamingProfileOverride = z.infer<typeof StlNamingProfileOverrideSchema>;

export const SourceNamingPutInputSchema = z.discriminatedUnion("use_defaults", [
  z.strictObject({ use_defaults: z.literal(true) }),
  z.strictObject({
    use_defaults: z.literal(false),
    override: StlNamingProfileSchema,
  }),
]);
export type SourceNamingPutInput = z.infer<typeof SourceNamingPutInputSchema>;

const SourceNamingResponseFields = {
  effective: StlNamingProfileSchema,
  effective_digest: z.string().regex(/^[0-9a-f]{64}$/),
};

export const SourceNamingResponseSchema = z.discriminatedUnion("use_defaults", [
  z.strictObject({
    use_defaults: z.literal(true),
    override: z.strictObject({}),
    ...SourceNamingResponseFields,
  }),
  z.strictObject({
    use_defaults: z.literal(false),
    override: StlNamingProfileOverrideSchema,
    ...SourceNamingResponseFields,
  }),
]);
export type SourceNamingResponse = z.infer<typeof SourceNamingResponseSchema>;

export const SourceNamingEndpointErrorSchema = z.discriminatedUnion("code", [
  z.strictObject({
    code: z.literal("invalid_source_naming"),
    detail: z.string().trim().min(1),
  }),
  z.strictObject({
    code: z.literal("source_not_found"),
    detail: z.literal("Source not found"),
  }),
  z.strictObject({
    code: z.literal("source_naming_conflict"),
    detail: z.literal("Source metadata changed while saving naming rules"),
  }),
  z.strictObject({
    code: z.literal("invalid_source_naming_state"),
    detail: z.literal("Stored Source naming settings are invalid"),
  }),
]);
export type SourceNamingEndpointError = z.infer<typeof SourceNamingEndpointErrorSchema>;

export const DEFAULT_STL_NAMING_PROFILE = {
  roles: [
    { id: "primary", label: "Primary", markers: [] },
    { id: "accent", label: "Accent", markers: ["[a]"] },
    { id: "clear", label: "Clear", markers: ["[c]"] },
    { id: "opaque", label: "Opaque", markers: ["[o]"] },
  ],
  quantity: {
    regex: String.raw`[ _]x([0-9]+)\.stl$`,
    default: 1,
  },
  slug: {
    strip_markers: true,
    strip_quantity: true,
  },
  folder_rules: [],
  export_role_order: ["primary", "accent", "clear", "opaque"],
} satisfies StlNamingProfile;

function parseSchema<T>(schema: z.ZodType<T>, value: unknown, label: string): T {
  const result = schema.safeParse(value);
  if (result.success) return result.data;
  const issue = result.error.issues[0];
  const issuePath = issue?.path.map(String).join(".") ?? "";
  const field = issuePath ? `${label}.${issuePath}` : label;
  throw new SourceNamingContractError(field, issue?.message ?? "is invalid");
}

export function parseStlNamingProfile(value: unknown): StlNamingProfile {
  return parseSchema(StlNamingProfileSchema, value, "profile");
}

export function parseStlNamingProfileOverride(value: unknown): StlNamingProfileOverride {
  return parseSchema(StlNamingProfileOverrideSchema, value, "override");
}

export function parseSourceNamingPutInput(value: unknown): SourceNamingPutInput {
  return parseSchema(SourceNamingPutInputSchema, value, "source naming request");
}

export function parseSourceNamingResponse(value: unknown): SourceNamingResponse {
  return parseSchema(SourceNamingResponseSchema, value, "source naming response");
}

export function sourceNamingEndpointErrorStatus(
  error: SourceNamingEndpointError,
): 400 | 404 | 409 | 500 {
  switch (error.code) {
    case "invalid_source_naming":
      return 400;
    case "source_not_found":
      return 404;
    case "source_naming_conflict":
      return 409;
    case "invalid_source_naming_state":
      return 500;
    default: {
      const exhaustive: never = error;
      return exhaustive;
    }
  }
}

export function parseSourceNamingEndpointError(
  value: unknown,
  httpStatus?: number,
): SourceNamingEndpointError {
  const error = parseSchema(SourceNamingEndpointErrorSchema, value, "source naming error");
  if (httpStatus !== undefined && sourceNamingEndpointErrorStatus(error) !== httpStatus) {
    throw new SourceNamingContractError("source naming error.status", "does not match HTTP status");
  }
  return error;
}

export function invalidSourceNaming(detail: string): SourceNamingEndpointError {
  return { code: "invalid_source_naming", detail };
}

export function sourceNotFound(): SourceNamingEndpointError {
  return { code: "source_not_found", detail: "Source not found" };
}

export function sourceNamingConflict(): SourceNamingEndpointError {
  return {
    code: "source_naming_conflict",
    detail: "Source metadata changed while saving naming rules",
  };
}

export function invalidSourceNamingState(): SourceNamingEndpointError {
  return {
    code: "invalid_source_naming_state",
    detail: "Stored Source naming settings are invalid",
  };
}
