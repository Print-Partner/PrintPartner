import { Fragment, type MouseEvent } from "react";
import { NavLink } from "react-router-dom";
import { ClipboardList, Import, ListChecks, PackageCheck } from "lucide-react";
import { cn } from "@/lib/utils";
import {
  type WorkflowStage,
  type WorkflowStageId,
} from "../lib/workflowStages";

const STAGE_ICONS: Record<WorkflowStageId, typeof Import> = {
  sources: Import,
  plan: ClipboardList,
  production: PackageCheck,
  checkoff: ListChecks,
};

type Props = {
  stages: WorkflowStage[];
  activeId: WorkflowStageId | null;
  variant?: "rail" | "mobile";
  collapsed?: boolean;
  onNavigate?: (to: string, event: MouseEvent<HTMLAnchorElement>) => void;
  className?: string;
};

function isAttention(stage: WorkflowStage): boolean {
  return stage.status.kind === "needs_attention"
    || stage.status.kind === "stale"
    || stage.status.kind === "error";
}

function statusDotClass(stage: WorkflowStage, active: boolean): string {
  if (active) return "bg-primary ring-primary/30";
  switch (stage.status.kind) {
    case "complete":
      return "bg-success";
    case "needs_attention":
    case "stale":
      return "bg-warning ring-warning/30";
    case "error":
      return "bg-destructive ring-destructive/30";
    case "in_progress":
    case "ready":
      return "bg-primary/70 ring-primary/20";
    case "not_started":
      return "bg-border";
  }
}

/** Outstanding task count for a stage, used as the mobile badge number. */
function stageTaskCount(stage: WorkflowStage): number {
  return "task_count" in stage.status ? (stage.status.task_count ?? 0) : 0;
}

function stageAriaLabel(stage: WorkflowStage): string {
  return `${stage.label}. ${stage.status.summary}`;
}

export default function WorkflowProgress({
  stages,
  activeId,
  variant = "rail",
  collapsed = false,
  onNavigate,
  className,
}: Props) {
  if (variant === "mobile") {
    return (
      <nav
        className={cn(
          "flex shrink-0 gap-1 border-t border-border bg-card/95 px-2 py-1.5 backdrop-blur-sm print:hidden",
          className,
        )}
        aria-label="Build Workflow"
      >
        {stages.map((stage, index) => {
          const active = stage.id === activeId;
          const startsMake = index > 0 && stages[index - 1]?.group !== stage.group;
          const Icon = STAGE_ICONS[stage.id];
          const attention = isAttention(stage);
          return (
            <NavLink
              key={stage.id}
              to={stage.to}
              onClick={(event) => onNavigate?.(stage.to, event)}
              aria-label={stageAriaLabel(stage)}
              aria-current={active ? "page" : undefined}
              className={cn(
                // WCAG 2.2 target size: each destination keeps a 44px tall target.
                "relative flex min-h-11 min-w-0 flex-1 flex-col items-center justify-center gap-0.5 rounded-md px-1 py-1 text-center transition-colors",
                startsMake && "border-l border-border",
                active
                  ? "desk-stage-active text-primary"
                  : "text-muted-foreground hover:bg-accent/70 hover:text-foreground",
              )}
            >
              <span className="relative">
                <Icon className="h-5 w-5" aria-hidden />
                {/* A count, not a bare colour: the number carries the meaning and
                    the tone only reinforces it (WCAG G14). */}
                {attention ? (
                  <span
                    className={cn(
                      "absolute -right-2 -top-1 min-w-4 rounded-full px-1 text-micro font-semibold leading-4 ring-2 ring-card",
                      stage.status.kind === "error"
                        ? "bg-destructive text-destructive-foreground"
                        : "bg-warning text-warning-foreground",
                    )}
                    aria-hidden
                  >
                    {stageTaskCount(stage)}
                  </span>
                ) : null}
              </span>
              <span
                className={cn(
                  "w-full truncate text-micro font-medium",
                  active && "font-semibold",
                )}
              >
                {stage.label}
              </span>
            </NavLink>
          );
        })}
      </nav>
    );
  }

  if (collapsed) {
    return (
      <nav
        className={cn("relative flex flex-col gap-1", className)}
        aria-label="Build Workflow"
      >
        {stages.map((stage, index) => {
          const active = stage.id === activeId;
          const Icon = STAGE_ICONS[stage.id];
          const startsMake = index > 0 && stages[index - 1]?.group !== stage.group;
          return (
            <Fragment key={stage.id}>
              {startsMake ? <div className="mx-1 my-1 border-t border-border" aria-hidden /> : null}
              <NavLink
                to={stage.to}
                onClick={(event) => onNavigate?.(stage.to, event)}
                title={stageAriaLabel(stage)}
                aria-label={stageAriaLabel(stage)}
                className={cn(
                  "relative flex items-center justify-center rounded-md p-2.5 transition-colors",
                  active
                    ? "desk-stage-active text-primary"
                    : "text-muted-foreground hover:bg-accent/70 hover:text-foreground",
                  stage.status.kind === "not_started" && !active && "opacity-60",
                )}
              >
                <Icon className="h-4 w-4" />
                {isAttention(stage) ? (
                  <span
                    className={cn(
                      "absolute right-1 top-1 h-1.5 w-1.5 rotate-45",
                      stage.status.kind === "error" ? "bg-destructive" : "bg-warning",
                    )}
                    aria-hidden
                  />
                ) : stage.status.kind === "complete" ? (
                  <span
                    className="absolute right-1 top-1 h-1.5 w-1.5 rounded-full bg-success ring-2 ring-card"
                    aria-hidden
                  />
                ) : stage.status.kind === "ready" || stage.status.kind === "in_progress" ? (
                  <span
                    className="absolute right-1 top-1 h-1.5 w-1.5 rounded-full bg-primary ring-2 ring-card"
                    aria-hidden
                  />
                ) : null}
              </NavLink>
            </Fragment>
          );
        })}
      </nav>
    );
  }

  return (
    <nav
      className={cn("relative flex flex-col gap-1", className)}
      aria-label="Build Workflow"
    >
      {stages.map((stage, index) => {
        const active = stage.id === activeId;
        const startsGroup = index === 0 || stages[index - 1]?.group !== stage.group;
        return (
          <Fragment key={stage.id}>
            {startsGroup ? (
              <p
                className={cn(
                  "px-2 pt-1 font-mono text-micro font-semibold uppercase tracking-[0.14em] text-muted-foreground",
                  index > 0 && "mt-2 border-t border-border pt-3",
                )}
              >
                {stage.group === "prepare" ? "Prepare" : "Make"}
              </p>
            ) : null}
            <NavLink
              to={stage.to}
              onClick={(event) => onNavigate?.(stage.to, event)}
              title={stage.status.summary}
              aria-label={stageAriaLabel(stage)}
              className={cn(
                "relative flex items-start gap-2.5 rounded-md px-2.5 py-2 transition-colors",
                active ? "desk-stage-active" : "hover:bg-accent/70",
              )}
            >
              <span
                className={cn(
                  "relative z-[1] mt-1 h-2.5 w-2.5 shrink-0 rounded-full ring-[3px] ring-card",
                  statusDotClass(stage, active),
                )}
                aria-hidden
              />
              <span className="min-w-0 flex-1">
                <span
                  className={cn(
                    "block text-sm font-medium",
                    active
                      ? "font-semibold text-primary"
                      : stage.status.kind === "not_started"
                        ? "text-muted-foreground/70"
                        : "text-foreground",
                  )}
                >
                  {stage.label}
                </span>
                <span className="mt-0.5 block text-xs leading-snug text-muted-foreground">
                  {stage.status.summary}
                </span>
              </span>
            </NavLink>
          </Fragment>
        );
      })}
    </nav>
  );
}
