import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import {
  BookOpen,
  ClipboardCheck,
  FileArchive,
  FolderGit2,
  FolderOpen,
  Hammer,
  Scale,
  Workflow,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { externalApiAccessEnabled, HOSTED_PLANNING_COMPOSE_NOTE, isHostedPlanning } from "@print-partner/contracts";
import { fetchLegalDocument, fetchWorkflowGuide } from "../api/endpoints/help";
import { fetchManifestRegistry, type ManifestRegistryEntry } from "../api/endpoints/planManifests";
import { engineBaseUrl } from "../api/endpoints/runtime";
import SupportCta from "../components/SupportCta";
import PageHeader from "../components/layout/PageHeader";
import PageShell from "../components/layout/PageShell";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "../components/ui/tabs";
import { useProfileSelection } from "../context/ProfileContext";
import { useEngineHealth } from "../hooks/useEngineHealth";
import {
  LEGAL_TABS,
  WORKFLOW_STEPS,
  renderMarkdownLite,
  workflowStepPaths,
  type LegalTab,
} from "../lib/helpPageModel";
import { boardRoute } from "../lib/routes";
import { resolveEngineState } from "../lib/workflowState";
import { useExternalAccessSettingsQuery } from "../queries/externalAccess";

const WORKFLOW_STEP_ICONS: LucideIcon[] = [FolderGit2, Hammer, FileArchive, ClipboardCheck];

const WORKFLOW_GROUPS = [
  {
    id: "prepare",
    label: "Prepare",
    description: "Choose the files and parts to make.",
  },
  {
    id: "make",
    label: "Make",
    description: "Repeat Production and Checkoff until the Build is complete.",
  },
] as const;

function HelpLoadingSkeleton() {
  return (
    <div className="space-y-2">
      <Skeleton className="h-5 w-3/4" />
      <Skeleton className="h-4 w-full" />
      <Skeleton className="h-4 w-full" />
      <Skeleton className="h-4 w-5/6" />
    </div>
  );
}

export default function HelpPage() {
  const { health, error: engineError, loading: healthLoading } = useEngineHealth();
  const { selectedProfileId } = useProfileSelection();
  const engineState = resolveEngineState({
    health,
    loading: healthLoading,
    error: engineError,
  });
  const engineReady = engineState === "ready";
  const hostedPlanning = isHostedPlanning(health);
  const externalAccessQuery = useExternalAccessSettingsQuery(engineReady);
  const showApiDetails = externalAccessQuery.data
    ? externalApiAccessEnabled(externalAccessQuery.data.mode)
    : false;
  const [legalTab, setLegalTab] = useState<LegalTab>("summary");
  const [legalText, setLegalText] = useState("");
  const [workflowText, setWorkflowText] = useState("");
  const [workflowLoading, setWorkflowLoading] = useState(false);
  const [legalLoading, setLegalLoading] = useState(false);
  const [registryLoading, setRegistryLoading] = useState(false);
  const [workflowError, setWorkflowError] = useState<string | null>(null);
  const [legalError, setLegalError] = useState<string | null>(null);
  const [engineUrl, setEngineUrl] = useState<string | null>(null);
  const [registryEntries, setRegistryEntries] = useState<ManifestRegistryEntry[]>([]);
  const [registryError, setRegistryError] = useState<string | null>(null);

  const stepPaths = workflowStepPaths(selectedProfileId);

  const dataDir = engineReady ? health?.data_dir : null;

  useEffect(() => {
    if (!engineReady) return;
    void engineBaseUrl().then(setEngineUrl);
  }, [engineReady]);

  const loadWorkflow = useCallback(async () => {
    setWorkflowError(null);
    setWorkflowLoading(true);
    try {
      const text = await fetchWorkflowGuide();
      setWorkflowText(text);
    } catch (e) {
      setWorkflowError(e instanceof Error ? e.message : String(e));
      setWorkflowText("");
    } finally {
      setWorkflowLoading(false);
    }
  }, []);

  const loadLegal = useCallback(async (tab: LegalTab) => {
    setLegalError(null);
    setLegalLoading(true);
    try {
      const text = await fetchLegalDocument(tab);
      setLegalText(text);
    } catch (e) {
      setLegalError(e instanceof Error ? e.message : String(e));
      setLegalText("");
    } finally {
      setLegalLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!engineReady) return;
    void loadWorkflow();
  }, [engineReady, loadWorkflow]);

  useEffect(() => {
    if (!engineReady) return;
    void loadLegal(legalTab);
  }, [engineReady, legalTab, loadLegal]);

  useEffect(() => {
    if (!engineReady) return;
    setRegistryError(null);
    setRegistryLoading(true);
    void fetchManifestRegistry()
      .then(setRegistryEntries)
      .catch((e) => {
        setRegistryError(e instanceof Error ? e.message : String(e));
        setRegistryEntries([]);
      })
      .finally(() => setRegistryLoading(false));
  }, [engineReady]);

  return (
    <PageShell width="reading">
      <PageHeader
        icon={BookOpen}
        accent
        title="Help"
        description="Workflow guide, data folders, and license information."
        actions={<SupportCta />}
      />

      {hostedPlanning ? (
        <div className="space-y-2 text-sm text-muted-foreground">
          <p>{HOSTED_PLANNING_COMPOSE_NOTE}</p>
          <p>
            Sharing is the{" "}
            <Link className="font-medium underline underline-offset-2" to={boardRoute()}>
              Board
            </Link>
            . Invitees post a frozen references-only Build recipe and leave comments. Kit copies stay off this host.
          </p>
        </div>
      ) : null}

      <Card>
        <CardHeader accent>
          <div className="flex items-start gap-3">
            <span className="desk-well h-9 w-9 shrink-0">
              <Workflow className="h-4 w-4" aria-hidden />
            </span>
            <div>
              <CardTitle className="text-base">Workflow</CardTitle>
              <CardDescription>Sources → Plan → (Production ↔ Checkoff)</CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <div className="grid gap-4 md:grid-cols-2">
            {WORKFLOW_GROUPS.map((group) => (
              <section key={group.id} className="desk-nameplate p-3">
                <h3 className="font-medium">{group.label}</h3>
                <p className="mt-1 text-xs text-muted-foreground">{group.description}</p>
                <ol className="mt-3 grid gap-2">
                  {WORKFLOW_STEPS.map((step, index) => {
                    if (step.group !== group.id) return null;
                    const StepIcon = WORKFLOW_STEP_ICONS[index];
                    return (
                      <li key={step.id}>
                        <Link
                          to={stepPaths[index]}
                          className="flex h-full items-start gap-3 rounded-lg p-3 transition-colors hover:bg-accent/70"
                        >
                          <span className="desk-well h-7 w-7 shrink-0">
                            <StepIcon className="h-3.5 w-3.5" aria-hidden />
                          </span>
                          <span>
                            <span className="block font-medium">{step.label}</span>
                            <span className="mt-1 block text-xs text-muted-foreground">
                              {hostedPlanning && step.id === "production"
                                ? "Prepare plates and export files. Live send is Compose on the shop LAN."
                                : step.description}
                            </span>
                          </span>
                        </Link>
                      </li>
                    );
                  })}
                </ol>
              </section>
            ))}
          </div>
        </CardContent>
      </Card>

      <Card id="kit-variants">
        <CardHeader accent>
          <div className="flex items-start gap-3">
            <span className="desk-well h-9 w-9 shrink-0">
              <Hammer className="h-4 w-4" aria-hidden />
            </span>
            <div>
              <CardTitle className="text-base">Kit variants</CardTitle>
              <CardDescription>
                Optional per-repo manifests that define one-choice and multi-choice groups on Plan.
              </CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-3 text-sm text-muted-foreground">
          <ol className="list-decimal space-y-2 pl-5">
            <li>
              On <strong className="font-medium text-foreground">Sources</strong>, attach the
              Library source as the main source for the Build, then sync it.
            </li>
            <li>
              On <strong className="font-medium text-foreground">Plan</strong>, expand{" "}
              <strong className="font-medium text-foreground">Kit variants</strong> and choose
              the options required by each group.
            </li>
            <li>
              Wait for <strong className="font-medium text-foreground">Saved</strong>. Print
              Partner refreshes the Plan parts automatically.
            </li>
          </ol>
          <ul className="list-disc space-y-2 pl-5">
            <li>
              Add a repo-root{" "}
              <code className="font-mono text-xs">print-partner.manifest.yaml</code> with{" "}
              <code className="font-mono text-xs">pick_one</code>,{" "}
              <code className="font-mono text-xs">pick_any</code>, or{" "}
              <code className="font-mono text-xs">pick_n</code> option groups, then sync the source.
            </li>
            <li>
              Each Build has one main source and can have additional sources. Attach them on
              Sources before choosing the variants on Plan.
            </li>
          </ul>
          <p className="text-xs">
            Maintainer docs:{" "}
            <a
              href="https://github.com/Print-Partner/PrintPartner/blob/main/docs/playbooks/kit-studio-build.md"
              target="_blank"
              rel="noreferrer"
              className="text-primary hover:underline"
            >
              kit-studio-build playbook
            </a>
          </p>
        </CardContent>
      </Card>

      <Card id="creating-a-manifest">
        <CardHeader accent>
          <div className="flex items-start gap-3">
            <span className="desk-well h-9 w-9 shrink-0">
              <BookOpen className="h-4 w-4" aria-hidden />
            </span>
            <div>
              <CardTitle className="text-base">Workflow guide</CardTitle>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          {workflowError && <p className="text-sm text-destructive">{workflowError}</p>}
          {!engineReady && (
            <p className="text-sm text-muted-foreground">
              {engineState === "offline"
                ? "Start the engine to load the workflow guide."
                : "Connecting to the engine…"}
            </p>
          )}
          {workflowLoading && engineReady && !workflowText && <HelpLoadingSkeleton />}
          {workflowText ? (
            <div
              className="help-prose text-sm leading-relaxed [&_h2]:mb-2 [&_h2]:mt-4 [&_h2]:text-base [&_h2]:font-semibold [&_h3]:mb-2 [&_h3]:mt-3 [&_h3]:text-sm [&_h3]:font-semibold [&_li]:ml-4 [&_p]:mb-2 [&_ul]:list-disc [&_ul]:space-y-1"
              dangerouslySetInnerHTML={{ __html: renderMarkdownLite(workflowText) }}
            />
          ) : null}
        </CardContent>
      </Card>

      <Card>
        <CardHeader accent>
          <div className="flex items-start gap-3">
            <span className="desk-well h-9 w-9 shrink-0">
              <FolderGit2 className="h-4 w-4" aria-hidden />
            </span>
            <div>
              <CardTitle className="text-base">Community manifest registry</CardTitle>
              <CardDescription>
                Approved manifests from the Print Partner repo. Link a slug on a source or use a
                repo-root <code className="font-mono text-xs">print-partner.manifest.yaml</code> after
                sync.
              </CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          {registryError && <p className="text-sm text-destructive">{registryError}</p>}
          {!engineReady && (
            <p className="text-sm text-muted-foreground">
              {engineState === "offline"
                ? "Start the engine to browse approved manifests."
                : "Connecting to the engine…"}
            </p>
          )}
          {registryLoading && engineReady && (
            <div className="space-y-2">
              <Skeleton className="h-4 w-full" />
              <Skeleton className="h-4 w-3/4" />
            </div>
          )}
          {engineReady && registryEntries.length === 0 && !registryError && !registryLoading && (
            <p className="text-sm text-muted-foreground">No approved community manifests yet.</p>
          )}
          {registryEntries.length > 0 && (
            <ul className="space-y-3">
              {registryEntries.map((entry) => (
                <li
                  key={entry.slug}
                  className="flex flex-col gap-1 rounded-md border border-border p-3 text-sm"
                >
                  <strong>{entry.title ?? entry.slug}</strong>
                  <span className="font-mono text-xs text-muted-foreground">{entry.slug}</span>
                  {entry.target_repo ? (
                    <a
                      href={entry.target_repo}
                      target="_blank"
                      rel="noreferrer"
                      className="text-xs text-primary hover:underline"
                    >
                      {entry.target_repo}
                    </a>
                  ) : (
                    <span className="text-xs text-muted-foreground">No repo URL</span>
                  )}
                </li>
              ))}
            </ul>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader accent>
          <div className="flex items-start gap-3">
            <span className="desk-well h-9 w-9 shrink-0">
              <FolderOpen className="h-4 w-4" aria-hidden />
            </span>
            <div>
              <CardTitle className="text-base">Folders</CardTitle>
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          <p className="text-sm text-muted-foreground">
            {dataDir ? (
              <>
                Data directory (inside the server):{" "}
                <code className="font-mono text-xs">{dataDir}</code>
              </>
            ) : (
              "Start the engine to see the data directory path."
            )}
          </p>
          <p className="text-xs text-muted-foreground">
            With Docker, bind a host volume to this path (default compose volume:{" "}
            <code className="font-mono">print-partner-data</code> → <code className="font-mono">/data</code>
            ).
          </p>
          {engineUrl && showApiDetails ? (
            <p className="text-xs text-muted-foreground">
              Engine API: <code className="font-mono">{engineUrl}</code> · OpenAPI:{" "}
              <code className="font-mono">{engineUrl}/api/v1/openapi.json</code>
            </p>
          ) : null}
        </CardContent>
      </Card>

      <Card>
        <CardHeader accent>
          <div className="flex items-start gap-3">
            <span className="desk-well h-9 w-9 shrink-0">
              <Scale className="h-4 w-4" aria-hidden />
            </span>
            <div>
              <CardTitle className="text-base">Legal</CardTitle>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <Tabs value={legalTab} onValueChange={(v) => setLegalTab(v as LegalTab)}>
            <TabsList className="flex h-auto w-full flex-wrap gap-1 sm:flex-nowrap">
              {LEGAL_TABS.map((t) => (
                <TabsTrigger key={t.id} value={t.id} className="min-h-9 flex-1 text-xs sm:flex-none sm:text-sm">
                  {t.label}
                </TabsTrigger>
              ))}
            </TabsList>
            {LEGAL_TABS.map((t) => (
              <TabsContent key={t.id} value={t.id}>
                {legalError && <p className="text-sm text-destructive">{legalError}</p>}
                {!engineReady && (
                  <p className="text-sm text-muted-foreground">
                    {engineState === "offline"
                      ? "Start the engine to load license text."
                      : "Connecting to the engine…"}
                  </p>
                )}
                {legalLoading && engineReady && !legalText ? (
                  <HelpLoadingSkeleton />
                ) : (
                  <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-md border border-border bg-muted/20 p-3 text-xs leading-relaxed">
                    {legalText}
                  </pre>
                )}
              </TabsContent>
            ))}
          </Tabs>
        </CardContent>
      </Card>
    </PageShell>
  );
}
