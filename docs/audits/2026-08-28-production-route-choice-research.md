# Production route choice research

Date: 2026-08-28

## Decision

Production should ask one route question before it shows any task list, and each route should own its own task list.

The current page shows a single fixed task list called "Prepare this work package" with four tasks: Prepare Plates, Export for slicing, Add sliced file, Send or start. See `web/apps/web/src/lib/workPackageTasks.ts:12-17` and `web/apps/web/src/pages/ExportPage.tsx:555-560`. Every Build goes down that path, including Builds where the user only wants the unit files and Builds where the print already happened on a printer PrintPartner cannot see.

Three routes:

| Route | What the user gets | Ends at |
| --- | --- | --- |
| Make Plates for my printers | Chosen printers, Required units grouped by a criterion the user picks, arranged Plates, files for the slicer | A printer job and Checkoff |
| Download the unit files | The unit files for chosen Required units, no Plates, no printers | A download |
| Record a print made elsewhere | A print already on a linked printer, or an uploaded G-code, binary G-code or 3MF file, attributed to Required units | Checkoff |

The routes are not three equal paths. The first is the long one and the common one. The second is short. The third is not production work at all, it is data entry after the fact, and it needs to sit visibly on the same question rather than hide in a menu.

Nothing in the GOV.UK Design System sanctions a task list that changes shape based on an earlier answer, and USWDS says outright to avoid a step indicator when the number of steps can change from user input. So the branch goes above the task list, not inside it. The route question is a radio question with the odd route separated by an "or" divider, no pre-selected option, and the chosen route stays changeable with an explicit warning about what a switch destroys.

This does not reopen [ADR 0001](../adr/0001-model-build-workflow-as-preparation-and-making.md). Production and Checkoff still repeat, and the route choice lives inside one Production work package rather than becoming a fifth top-level stage.

## Scope and method

This review answers six questions with primary sources only: the GOV.UK Design System and Service Manual, USWDS, W3C WCAG 2.2 and the ARIA Authoring Practices Guide, and Nielsen Norman Group's own heuristic articles. Blog summaries and third-party pattern libraries were not used. Where a source does not answer the question, this document says so and marks the recommendation as judgement.

It also reads the current code so the recommendation is concrete: `web/apps/web/src/lib/workPackageTasks.ts`, `web/apps/web/src/components/layout/TaskList.tsx`, and the Checkoff unattributed-print path in `web/apps/web/src/components/checkoff/UnattributedPrintCard.tsx`.

It builds on the 2026-08-27 workflow UX and printer files research (both in git history). The second of those already proposed three entry points for a Production artifact, so this document is largely about how to present that choice rather than whether to have one.

## 1. Presenting a branch in a task-based flow

GDS owns the task list pattern this repo follows, and its guidance is narrower than the repo currently treats it.

Use a task list only where there is evidence that users do not want to or cannot finish everything in one sitting, and need to choose their own order. [GOV.UK task list](https://design-system.service.gov.uk/components/task-list/) The same page says not to use a task list for a long service that must be completed in a specific order, and to try to simplify the service first. Production passes this test: the operator leaves to slice and comes back later, which is exactly why the current code resumes at the first unfinished task.

Two rules constrain a branch:

- Users should be able to complete tasks in whatever order they like, and can only move on from the list when every task shows Completed. [GOV.UK task list](https://design-system.service.gov.uk/components/task-list/)
- Where a user cannot start a task yet because another task must be finished first, use the "Cannot start yet" status, grey with no background colour, and do not link the row. [GOV.UK complete multiple tasks](https://design-system.service.gov.uk/patterns/complete-multiple-tasks/)

That is dependency, not branching. "Cannot start yet" says "later", not "never for you". Neither the component page nor the pattern page says anything about tasks that do not apply to a given user, and the published research does not raise it either. The research on the component covers linking the whole task row, sentence-case status wording, and the status colour palette, and its known gaps are limited to accidental clicking, status contrast, and status wording. [GOV.UK task list, research on this component](https://design-system.service.gov.uk/components/task-list/#research-on-this-component) The pattern page points back to that same research. [GOV.UK complete multiple tasks, research on this pattern](https://design-system.service.gov.uk/patterns/complete-multiple-tasks/#research-on-this-pattern)

So the answer to "may a task list change shape based on an earlier answer" is that GDS does not say, and the gap is real rather than something I failed to find.

What GDS does own is branching before the list. The Service Manual says to use branching questions so people only have to answer questions that are relevant to them, and to design for the most common scenarios first, deciding which user group to prioritise and knowing the relative size of each group. [GOV.UK structuring forms](https://www.gov.uk/service-manual/design/form-structure) It also says one of the reasons to start with one thing per page is that it helps you handle branching questions and loops.

USWDS is blunter about the shape-changing case. Consider another approach if the number of steps might change due to user input, and a step indicator is not appropriate for nonlinear work that might be completed in any order. [USWDS step indicator](https://designsystem.digital.gov/components/step-indicator/) Three routes of different lengths is precisely a step count that changes from user input, which rules out any progress indicator across the route boundary and reinforces the earlier decision to drop "Stage n of 4" language.

For tasks that belong to another route, remove them rather than disable them. The closest owned statement is from the same design system, about tabs: do not disable elements, because disabling is normally confusing, so either remove the element or explain why there is no content. [GOV.UK tabs](https://design-system.service.gov.uk/components/tabs/) Applied here: a Build on the unit-files route should not show a greyed "Send or start" row at all.

Long transactions can be grouped into steps that represent stages in the process, each with a short heading, and tasks within a group should still be completable in any order. [GOV.UK complete multiple tasks](https://design-system.service.gov.uk/patterns/complete-multiple-tasks/) That is the sanctioned way to give the printer route its own headed group without inventing a numbered sequence.

## 2. How to ask the route question itself

Radios are the right control. Use radios when users can only select one option from a list, and do not use them if users might need more than one. [GOV.UK radios](https://design-system.service.gov.uk/components/radios/) USWDS agrees: radios are for a single selection from a set of mutually exclusive choices. [USWDS radio buttons](https://designsystem.digital.gov/components/radio-buttons/)

Specific rules that shape this screen:

- Do not pre-select a radio option, because users are then more likely to miss the question or submit the wrong answer. [GOV.UK radios](https://design-system.service.gov.uk/components/radios/)
- Order options alphabetically by default. Ordering most to least common is sometimes helpful but should be done with extreme caution because it can reinforce bias. [GOV.UK radios](https://design-system.service.gov.uk/components/radios/) USWDS is more permissive and asks only for a meaningful order, alphabetical or most frequent to least frequent. [USWDS radio buttons](https://designsystem.digital.gov/components/radio-buttons/)
- If one or more options is phrased differently from the others, separate them with a text divider, usually the word "or". [GOV.UK radios](https://design-system.service.gov.uk/components/radios/)
- Group the options in a `fieldset` with a `legend` that describes them, and if this is the only question on the page, make the legend the page heading so screen reader users hear it once. [GOV.UK radios](https://design-system.service.gov.uk/components/radios/)
- Hints on individual radio items are allowed, but do not put links in hint text, because screen readers usually do not announce that the text is a link. [GOV.UK radios](https://design-system.service.gov.uk/components/radios/)
- Conditional reveal is only for revealing a related question, and only questions. Do not show or hide anything that is not a question. GDS also records a known accessibility issue: users are not always notified when a conditionally revealed question appears or disappears, which fails WCAG 2.2 SC 4.1.2 Name, Role, Value. [GOV.UK radios](https://design-system.service.gov.uk/components/radios/) So the route radios must not conditionally reveal a whole task list.

On cards, neither system has a card control for a single choice. GOV.UK ships no card component. USWDS's closest sanctioned equivalent is the tile variant of radio buttons, which gives larger touch targets and room for a description line, with two constraints: do not mix default and tile variants, because mixed tiles look like a recommendation, and set default values with caution, only with data to back them up. [USWDS radio buttons](https://designsystem.digital.gov/components/radio-buttons/) That makes tile radios the defensible way to give each route a short description and a shop-floor-sized target without inventing a card pattern.

On one thing per page: start by splitting a form so each page holds one decision or question, and let research tell you when to merge pages. [GOV.UK structuring forms](https://www.gov.uk/service-manual/design/form-structure) The question pages pattern repeats this and requires a back link, a page heading, and a continue button on every question page. [GOV.UK question pages](https://design-system.service.gov.uk/patterns/question-pages/) There is an explicit carve-out for internal tools: a user of an admin system may need to repeat and switch between tasks quickly, or see everything about one thing in one place, which may make one thing per page inappropriate on some pages. [GOV.UK services for government users](https://www.gov.uk/service-manual/design/services-for-government-users) The same page warns not to assume an existing working process is a user need.

On tabs, both systems point away from what PrintPartner used to do. GOV.UK says tabs are for related sections of content shown one at a time, not to be used as page navigation, and not to be used if users need to read through content in order to understand a step-by-step process or compare information across tabs. [GOV.UK tabs](https://design-system.service.gov.uk/components/tabs/) USWDS has no tabs component at all: its component list has 47 entries and tabs is not one of them. [USWDS components](https://designsystem.digital.gov/components/overview/) Three mutually exclusive routes, each containing a multi-step process, fail the GOV.UK test twice. Tabs are wrong here, which is consistent with the earlier decision to remove the four numbered Production tabs.

On whether the route stays changeable: the design systems answer yes, in three places. Radios cannot be returned to an unselected state without refreshing the browser, so include a "None of the above" or "I do not know" option if those are valid answers. [GOV.UK radios](https://design-system.service.gov.uk/components/radios/) Always allow users to go back into a task to change their answer. [GOV.UK complete multiple tasks](https://design-system.service.gov.uk/patterns/complete-multiple-tasks/) Always include a back link on question pages, and do not break the browser back button, with one exception for actions the user should only perform once, where the back button should still work but show a sensible message rather than let the action repeat. [GOV.UK question pages](https://design-system.service.gov.uk/patterns/question-pages/) A sent printer job is exactly that kind of once-only action.

## 3. Reversibility and the cost of a wrong choice

Nielsen's third heuristic states: "Users often choose system functions by mistake and will need a clearly marked 'emergency exit' to leave the unwanted state without having to go through an extended dialogue. Support undo and redo." [NN/g, User Control and Freedom](https://www.nngroup.com/articles/user-control-and-freedom/) The same article says users should be able to quickly correct mistakes or backtrack on choices, that the ability to get out of trouble encourages exploration, and that a cancel option should be easy to find and quick to execute even where a back link exists. It also warns against a back control that silently behaves as cancel, using Delta's app as the example: selecting a flight and then pressing back throws the user to the start of the search, which makes people hesitant to explore.

That is the failure mode to avoid on this screen. Switching from the printer route to the unit-files route must not silently discard chosen printers, unit grouping, and a Plate revision under a control labelled "Back".

WCAG turns part of this into a requirement. SC 3.3.4 Error Prevention (Legal, Financial, Data), Level AA, applies to pages that modify or delete user-controllable data in a data storage system, and requires at least one of: submissions are reversible, data is checked for input errors with a chance to correct, or a mechanism is available for reviewing, confirming, and correcting information before finalising. The intent explicitly covers unintentional modification or deletion of stored data the user will later need, while noting it is not meant to require confirmation for every save. [W3C, Understanding SC 3.3.4](https://www.w3.org/WAI/WCAG22/Understanding/error-prevention-legal-financial-data.html) A Plate revision and printer assignment are durable Build state that the operator created deliberately, so a route switch that destroys them needs one of those three. The listed sufficient technique for the deletion case is G168, requesting confirmation to continue with the selected action.

SC 3.3.7 Redundant Entry, Level A, says information the user already entered in the same process, if required again, must be auto-populated or available for the user to select, unless re-entry is essential, security-related, or the earlier information is no longer valid. [W3C, Understanding SC 3.3.7](https://www.w3.org/WAI/WCAG22/Understanding/redundant-entry.html) Required-unit selection is the same information in all three routes, so it carries across a switch. Printer assignment and Plate layout do not exist in the other two routes and are therefore genuinely no longer valid, which is the exception rather than a licence to lose them quietly.

The change-link mechanics are already settled by GDS. Provide a Change link next to each section, with hidden text describing what it changes. Pre-populate what the user already entered. Show pages the way the user last saw them. Return the user to the check answers page when they finish rather than making them walk the rest of the transaction again. If a change means you need to ask more questions, ask them before returning. [GOV.UK check answers](https://design-system.service.gov.uk/patterns/check-answers/)

Applied here, "Change route" behaves like a Change link with a confirmation step in front of it when the switch is lossy, and like a plain Change link when it is not. Switching away from the printer route after a Plate revision exists is lossy. Switching before any Plate work is not.

## 4. Progressive disclosure versus showing all routes

Progressive disclosure means showing only a few of the most important options initially and offering a larger set on request, so most users never meet the extra complexity. Nielsen names two things you must get right: the split between initial and secondary features, disclosing everything users frequently need up front so they progress only on rare occasions, and making it obvious how to progress from the primary to the secondary level. He also warns that it is rarely a good idea to offer multiple ways to reach the secondary options. [NN/g, Progressive Disclosure](https://www.nngroup.com/articles/progressive-disclosure/)

The same article is why the route choice must not be a sequence of screens the user walks once. Staged disclosure, whose classic form NN/g calls a wizard, is useful when a task divides into distinct steps with little interaction between them, and problematic when the steps are interdependent and users must alternate between them. [NN/g, Progressive Disclosure](https://www.nngroup.com/articles/progressive-disclosure/) Production's steps are interdependent and the operator alternates: arrange Plates, export, leave for the slicer, come back, adjust, send. This supports the existing resumable task list and, along with the USWDS step indicator guidance above, keeps the earlier decision to remove step semantics.

On defaults, the two systems disagree in emphasis and I am not going to average them. GOV.UK says flatly not to pre-select radio options. [GOV.UK radios](https://design-system.service.gov.uk/components/radios/) USWDS says to set defaults with caution and only with data to back them up. [USWDS radio buttons](https://designsystem.digital.gov/components/radio-buttons/) The GOV.UK position is the stricter one and the repo already follows GOV.UK for forms, so no route is pre-selected. Recommending a route by wording and order is still allowed, and GOV.UK's own advice to design for the most common scenarios first supports it. [GOV.UK structuring forms](https://www.gov.uk/service-manual/design/form-structure)

The tension in the assignment is real and the sources do not resolve it. Progressive disclosure is about deferring rarely used features to a secondary screen; the route question is a single mutually exclusive choice whose options are the product's model of what Production is. Nielsen's own caution about splits cuts against hiding routes: if a hidden route is one a user occasionally needs, they now progress to a secondary level on a normal occasion rather than a rare one. Nielsen also notes that showing something on the initial display tells users it is important, and that research does not support the fear that a small initial set gives users a limiting mental model. [NN/g, Progressive Disclosure](https://www.nngroup.com/articles/progressive-disclosure/) That argument is about features, not about the branches of a single question, so it does not settle this case either way.

Author's judgement, not a finding: show all three routes on the one question. Two of them are rare, but a Build has exactly one route and the user cannot pick a route they cannot see. Apply progressive disclosure inside each route instead, where it is the right tool: the grouping criterion, the Plate layout details, and the unmonitored-destination fields are the rarely used options that belong one level down.

## 5. The "record something that happened elsewhere" route

No primary source I found names this case. Neither GOV.UK nor USWDS has guidance on distinguishing "do the work here" from "tell us what you already did" in one interface. Everything below is either adjacent owned guidance or marked as judgement.

Adjacent guidance that does apply:

- Where one option is phrased differently from the others, separate it with an "or" divider. [GOV.UK radios](https://design-system.service.gov.uk/components/radios/) "Record a print made elsewhere" is phrased differently from the other two, because it is a record rather than a piece of work, so it belongs after an "or".
- Use the details component to reveal help text that is only relevant to a subset of users, and do not use help text to explain the interface, because needing to do that means the service is too complicated. [GOV.UK designing good questions](https://www.gov.uk/service-manual/design/designing-good-questions)
- Accelerators are secondary ways of doing the same task, faster but less obvious, that do not get in the way of a new user. The trick is making them discoverable but unobtrusive. The same article warns against duplication: positioning the same feature in several places in the UI means users must learn what the difference between the duplicated commands is. [NN/g, Flexibility and Efficiency of Use](https://www.nngroup.com/articles/flexibility-efficiency-heuristic/)
- Avoid asking questions the user will need to provide again when using your service. [GOV.UK check a service is suitable](https://design-system.service.gov.uk/patterns/check-a-service-is-suitable/)
- Do not assume an existing working process is a user need; investigate whether it is necessary or can be improved. [GOV.UK services for government users](https://www.gov.uk/service-manual/design/services-for-government-users)

Author's judgement: the escape hatch belongs on the route question, third, after the "or" divider, and nowhere else in Production. The duplication warning in heuristic 7 is the reason. Checkoff already has a claim path for prints the system noticed by itself, the unattributed print card at `web/apps/web/src/components/checkoff/UnattributedPrintCard.tsx`, which starts as a compact "Unclaimed print detected" flair and expands. If Production grows a second attribution entry point in a menu or a secondary panel, there are then three places to attribute a print and the operator has to learn which one applies. One deliberate route in Production for prints PrintPartner never saw, and one recovery path in Checkoff for prints it did see, is the smallest set that covers both cases.

This also matches the earlier decision that attribution should be possible before a print starts, with completed unattributed activity as a recovery path rather than the normal path. That decision came from the 2026-08-27 printer files research, now in git history.

## 6. Accessibility requirements, checked against WCAG

The prior brief lists seven commitments. Four map to WCAG success criteria, one maps to a Level AAA criterion rather than the AA line, and two are house style with partial standards backing.

| Commitment | Backing | Level |
| --- | --- | --- |
| Never colour alone for status | SC 1.4.1 Use of Color, plus technique G14 | A |
| `role="status"` or a polite live region for background progress | SC 4.1.3 Status Messages | AA |
| `role="alert"` only for urgent problems | Not a success criterion. ARIA APG says alerts must not affect keyboard focus, that you should avoid alerts that disappear automatically, and that frequent interruption inhibits usability | Guidance, plus SC 2.2.4 Interruptions at AAA |
| Persistent inline errors rather than toasts | SC 3.3.1 Error Identification requires the item in error to be identified and the problem described in text. APG adds that an alert which disappears too quickly can fail SC 2.2.3. The Retry control itself is house style | 3.3.1 at A, 2.2.3 at AAA |
| Any drag interaction needs a non-drag alternative | SC 2.5.7 Dragging Movements | AA |
| Shop-floor touch targets at least 44 by 44 CSS px | SC 2.5.5 Target Size (Enhanced) requires 44 by 44. The AA line is SC 2.5.8 Target Size (Minimum) at 24 by 24 | 2.5.5 at AAA, 2.5.8 at AA |
| Phone-first single column | Not a success criterion. SC 1.4.10 Reflow requires content to work without two-dimensional scrolling at a 320 CSS px equivalent width. Single column phone-first is house style that satisfies it | 1.4.10 at AA |

Criterion numbers and levels come from the W3C list of WCAG 2.2 understanding documents. [W3C, All WCAG 2.2 Understanding Docs](https://www.w3.org/WAI/WCAG22/Understanding/)

Two details worth keeping:

The 44 by 44 target is not just a house preference borrowed from Apple. SC 2.5.5's intent recommends going larger than the minimum specifically when the control is used frequently, when the result of the interaction cannot easily be undone, or when the control is part of a sequential task. [W3C, Understanding SC 2.5.5](https://www.w3.org/WAI/WCAG22/Understanding/target-size-enhanced.html) The route radios and a "Send or start" control hit all three. It is worth saying in the code comment that the repo targets AAA here deliberately, not that 44 is the AA requirement.

The toast rule has firmer grounding than "toasts are weak". The APG alert pattern says it is important to avoid designing alerts that disappear automatically, that an alert which disappears too quickly can lead to failure to meet SC 2.2.3, and that alerts must not affect keyboard focus, with the alert dialog pattern reserved for cases where interrupting the workflow is necessary. [W3C, ARIA APG alert pattern](https://www.w3.org/WAI/ARIA/apg/patterns/alert/) A failed export that only ever announced itself in a toast is both an accessibility problem and a workflow one.

One commitment has no WCAG backing and should stop being described as an accessibility requirement: "every pending state names its owner and gives an action or an exact route". It is good design and it is partly design-system-backed, because GOV.UK requires a status on every task and a "Cannot start yet" status with an unlinked row where a task cannot be started yet. [GOV.UK complete multiple tasks](https://design-system.service.gov.uk/patterns/complete-multiple-tasks/) Naming the owner is this repo's own rule.

## Recommended shape

### The route question

Production opens a work package with one question, as a radio group in a `fieldset` whose `legend` is the page heading, using the USWDS tile treatment so each route carries a one-line description and a shop-floor-sized target. No option is pre-selected.

```text
How do you want to make these units?

( ) Make Plates for my printers
    Choose printers, group the Required units, arrange Plates, and export for slicing

( ) Download the unit files
    Get the files for the units you choose. No Plates, no printers

    or

( ) Record a print made elsewhere
    The print already exists. Pick a file from a linked printer, or upload G-code,
    binary G-code or a 3MF file

                                                              [Continue]
```

Order is most common first, with the record route separated by "or" because it is phrased differently from the other two. If the group order is later tuned by observed use, the GOV.UK caution about frequency ordering reinforcing bias applies and the change needs evidence.

Error message when nothing is selected, following the GOV.UK wording rule for more than two options: "Select how you want to make these units".

### Each route gets its own task list

The route becomes durable state on the Production work package, alongside the Accepted Plan revision and the selected Required units. `ProductionTaskId` stops being one flat list of four and becomes per-route. Tasks from other routes are absent, not disabled.

| Route | Tasks |
| --- | --- |
| Make Plates for my printers | Choose Required units, Choose printers and grouping, Arrange Plates, Export for slicing, Add sliced file, Send or start |
| Download the unit files | Choose Required units, Download the files |
| Record a print made elsewhere | Choose the file, Attribute it to Required units, Confirm the record |

The printer route is long enough for the grouped presentation GDS recommends: a "Prepare" group for units, printers and Plates, and a "Hand over" group for export, sliced file and send, each with a short heading. The other two routes are short enough to stay as one group.

Statuses stay as they are now, since the existing set already matches the GDS advice to start small and add only what research needs: Needs your decision, Waiting for your slicer, Complete, Failed retry available, and Unavailable with a reason. Existing behaviour to keep: only genuinely blocked tasks are unavailable, completed tasks stay reviewable, and the page reopens at the first unfinished task.

The grouping criterion the user picks for the printer route, currently Source layer, directory, colour, role, or part, stays one level down inside "Choose printers and grouping" rather than on the route question. That is the right use of progressive disclosure here.

### Changing route later

The Production work package header shows the chosen route with a Change link, following the check answers mechanics: hidden text on the link, pre-populated answers when the user returns, and a return to the work package rather than a walk through the rest of the flow.

Two cases:

Nothing lossy yet, meaning no Plate revision, no export artifact, no printer send, no attribution record. Change the route immediately. Carry the Required-unit selection across, which SC 3.3.7 requires anyway.

Lossy. Show a confirmation that names exactly what will go, then require an explicit action. This satisfies SC 3.3.4 through the "confirmed" option, and technique G168 is the sanctioned form.

```text
Change route from "Make Plates for my printers" to "Download the unit files"?

This work package will lose:
  Plate revision 3, 4 Plates
  Printer assignments for 12 Required units
  1 exported file set

It will keep:
  Your 12 chosen Required units

Verified units in Checkoff are not affected.

                              [Change route and discard Plate work]  [Keep this route]
```

Where a printer send already happened, the route does not change. Say so plainly and point at the existing recovery, because the printed result is now physical and belongs to Checkoff. The route control becomes read-only text with a link to Checkoff rather than a disabled control, following the same design system's advice against disabling elements.

### Presentation rules carried forward

- No progress indicator and no step count anywhere across or inside a route, because the step count now depends on an answer.
- No tabs for the routes.
- No conditional reveal of a task list from the radios.
- The route question is a single column on a phone, with 44 by 44 targets on the tiles.
- Route status and change confirmations use text, not colour alone.
- A failed route switch keeps the user's answers and offers Retry inline, not in a toast.

## Open questions

The sources do not settle these. They need operator research or a product decision.

- Whether the route question belongs on its own page or at the top of the Production page. GOV.UK's default is one thing per page, and its own carve-out for internal tools where users switch tasks quickly points the other way. Both are defensible and only testing separates them.
- Whether operators think of these as three routes at all, or as one route with two exits. The route names above are my labels and have not been tested.
- Whether "Download the unit files" is the right name. GLOSSARY.md reserves Plate for a printable artifact and warns against calling a Plate an export, so this route must not be called an export, but "unit files" is not yet product vocabulary.
- Whether the record route should also accept a print from a printer PrintPartner monitors, which overlaps the Checkoff claim path, or whether it should be limited to files PrintPartner never saw. The duplication argument above is judgement, not evidence.
- Whether a Build should remember its last route across work packages. That is a default by another name, and GOV.UK's rule against pre-selection was written about first-time answers, not about a repeat operator making the same choice for the twentieth time. USWDS would allow it with data. There is no data yet.
- How often a route switch actually happens after Plate work exists. If it is rare, the confirmation above is enough. If it is common, the printer route is being offered too early and the ordering needs revisiting.
- Whether the unit-files route needs its own Checkoff story. Handing over files ends PrintPartner's involvement, so the Required units stay unverified forever unless the operator records something later.

## Primary sources

- [GOV.UK Design System: Task list](https://design-system.service.gov.uk/components/task-list/)
- [GOV.UK Design System: Complete multiple tasks](https://design-system.service.gov.uk/patterns/complete-multiple-tasks/)
- [GOV.UK Design System: Radios](https://design-system.service.gov.uk/components/radios/)
- [GOV.UK Design System: Question pages](https://design-system.service.gov.uk/patterns/question-pages/)
- [GOV.UK Design System: Check answers](https://design-system.service.gov.uk/patterns/check-answers/)
- [GOV.UK Design System: Check a service is suitable](https://design-system.service.gov.uk/patterns/check-a-service-is-suitable/)
- [GOV.UK Design System: Tabs](https://design-system.service.gov.uk/components/tabs/)
- [GOV.UK Service Manual: Structuring forms](https://www.gov.uk/service-manual/design/form-structure)
- [GOV.UK Service Manual: Designing good questions](https://www.gov.uk/service-manual/design/designing-good-questions)
- [GOV.UK Service Manual: Services for government users](https://www.gov.uk/service-manual/design/services-for-government-users)
- [USWDS: Radio buttons](https://designsystem.digital.gov/components/radio-buttons/)
- [USWDS: Step indicator](https://designsystem.digital.gov/components/step-indicator/)
- [USWDS: Components](https://designsystem.digital.gov/components/overview/)
- [W3C: All WCAG 2.2 Understanding Docs](https://www.w3.org/WAI/WCAG22/Understanding/)
- [W3C: Understanding SC 2.5.5 Target Size (Enhanced)](https://www.w3.org/WAI/WCAG22/Understanding/target-size-enhanced.html)
- [W3C: Understanding SC 3.3.4 Error Prevention (Legal, Financial, Data)](https://www.w3.org/WAI/WCAG22/Understanding/error-prevention-legal-financial-data.html)
- [W3C: Understanding SC 3.3.7 Redundant Entry](https://www.w3.org/WAI/WCAG22/Understanding/redundant-entry.html)
- [W3C: ARIA Authoring Practices, alert pattern](https://www.w3.org/WAI/ARIA/apg/patterns/alert/)
- [Nielsen Norman Group: User Control and Freedom (Usability Heuristic 3)](https://www.nngroup.com/articles/user-control-and-freedom/)
- [Nielsen Norman Group: Flexibility and Efficiency of Use (Usability Heuristic 7)](https://www.nngroup.com/articles/flexibility-efficiency-heuristic/)
- [Nielsen Norman Group: Progressive Disclosure](https://www.nngroup.com/articles/progressive-disclosure/)
