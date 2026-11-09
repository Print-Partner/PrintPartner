export const queryKeys = {
  health: ["health"] as const,
  printers: ["printers"] as const,
  slicerProfileOptions: ["slicerProfileOptions"] as const,
  integrations: ["integrations"] as const,
  tlsPin: (integrationId: string) => ["tlsPin", integrationId] as const,
  profiles: ["profiles"] as const,
  profile: (id: number) => ["profiles", id] as const,
  sources: ["sources"] as const,
  sourceActivity: ["sourceActivity"] as const,
  sourceCategories: ["sourceCategories"] as const,
  source: (id: number) => ["sources", id] as const,
  planReviews: ["planReview"] as const,
  planReview: (
    profileId: number,
    includeExcluded = false,
    spoolmanAllowed = true,
    revisionId?: number | null,
  ) =>
    revisionId === undefined
      ? (["planReview", profileId, includeExcluded, spoolmanAllowed] as const)
      : (["planReview", profileId, includeExcluded, spoolmanAllowed, revisionId] as const),
  allPlanLayers: ["planLayers"] as const,
  planLayers: (profileId: number) => ["planLayers", profileId] as const,
  planRecipeBundle: (profileId: number) => ["planRecipeBundle", profileId] as const,
  planDrafts: (profileId: number) => ["planDrafts", profileId] as const,
  planDraft: (profileId: number, draftId: number) => ["planDraft", profileId, draftId] as const,
  buildWorkflow: (profileId: number) => ["buildWorkflow", profileId] as const,
  checkoff: (profileId: number) => ["checkoff", profileId] as const,
  printerCheckoffLinks: ["printerCheckoffLinks"] as const,
  unattributedPrints: ["unattributedPrints"] as const,
  roleFilaments: (profileId: number, spoolmanAllowed = true) => ["roleFilaments", profileId, spoolmanAllowed] as const,
  filamentCatalogs: ["filamentCatalog"] as const,
  filamentCatalog: (profileId?: number | null, spoolmanAllowed = true) =>
    profileId != null ? (["filamentCatalog", profileId, spoolmanAllowed] as const) : (["filamentCatalog"] as const),
  spoolmanSpools: (integrationId: string, profileId?: number | null, spoolmanAllowed = true) =>
    profileId != null
      ? (["spoolmanSpools", integrationId, profileId, spoolmanAllowed] as const)
      : (["spoolmanSpools", integrationId] as const),
  workflowGuide: ["workflowGuide"] as const,
  dateFormatSetting: ["dateFormatSetting"] as const,
  buildTrackingSettings: ["buildTrackingSettings"] as const,
  externalAccessSettings: ["externalAccessSettings"] as const,
  tenantFeatures: ["tenantFeatures"] as const,
  planFeatures: (profileId: number) => ["planFeatures", profileId] as const,
  acceptedPlateWorkspace: (profileId: number) => ["acceptedPlateWorkspace", profileId] as const,
  acceptedPlateExportJobs: (profileId: number) => ["acceptedPlateExportJobs", profileId] as const,
};
