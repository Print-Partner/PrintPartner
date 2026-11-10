/**
 * How a Plan change saves and lands, in the words Help, the Plan tab and History share
 * (GRE-1140). A draft saves itself as it changes; only Accept changes the Build, and
 * Accept saves a snapshot of the Plan it replaces first. The old app's guide says changes
 * save with no acceptance step; that is the old behavior and is not copied here.
 */

/** Help's "Plan changes" section, one sentence group per item. */
export const PLAN_HELP: readonly string[] = [
  "Changes to a Plan save to a draft as you make them. The draft is kept if the app closes.",
  "The Build does not change until you accept the draft. Review and accept shows what would change first.",
  "Accept saves a snapshot of the accepted Plan it replaces. History lists the snapshot, and Restore as draft brings it back as a new draft.",
];

/** The Plan tab's line while a draft is open. */
export const DRAFT_OPEN =
  "A draft is open. Changes save to it as you make them, and the Build changes only when you accept. Accept first saves a snapshot of the accepted Plan, if there is one, so History can restore it.";

/** The Plan tab's line when no draft is open. */
export const NO_DRAFT =
  "No draft is open, so this shows the accepted Plan. Changing a field below starts a draft that saves as you go; the Build changes only when you accept it.";

/** History's empty state. */
export const HISTORY_EMPTY =
  "Nothing has happened to this Build yet. Accepted Plans, checkoffs and sends appear here, and so does the snapshot each Accept saves of the Plan it replaces, which you can restore as a draft.";
