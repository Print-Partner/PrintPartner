import { ExternalLink } from "lucide-react";
import type { AppUpdateCheckResponse } from "@print-partner/contracts";
import { useDateFormat } from "../../context/DateFormatContext";
import { Button } from "../ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "../ui/card";

type AboutUpdatesCardProps = {
  updateCheck: AppUpdateCheckResponse | null;
  onRefresh: () => void;
  refreshing?: boolean;
};

export default function AboutUpdatesCard({
  updateCheck,
  onRefresh,
  refreshing = false,
}: AboutUpdatesCardProps) {
  const releaseUrl = updateCheck?.release_url ?? updateCheck?.release_notes_url;
  const { formatDate } = useDateFormat();

  return (
    <Card id="about-updates" className="shadow-none">
      <CardHeader>
        <CardTitle className="text-base">About &amp; updates</CardTitle>
        <CardDescription>
          Version info and optional checks for new Print Partner releases.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3 text-sm">
        <p>
          Running{" "}
          <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">
            {updateCheck?.current_version ?? "…"}
          </code>
          {updateCheck?.deploy_mode && (
            <span className="text-muted-foreground"> ({updateCheck.deploy_mode})</span>
          )}
        </p>

        {!updateCheck?.enabled && (
          <p className="text-muted-foreground">
            Update checks are disabled on this server (
            <code className="font-mono text-xs">PRINT_PARTNER_UPDATE_CHECK=0</code>).
          </p>
        )}

        {updateCheck?.enabled && updateCheck.update_available && updateCheck.latest_version && (
          <div className="rounded-md border border-primary/40 bg-primary-soft px-3 py-2">
            <p className="font-medium">
              Update available: {updateCheck.latest_version}
            </p>
            {updateCheck.deploy_mode === "self-host" && (
              <p className="mt-1 text-muted-foreground">
                Self-host Docker:{" "}
                <code className="font-mono text-xs">
                  docker compose pull &amp;&amp; docker compose up --build
                </code>
              </p>
            )}
            {releaseUrl && (
              <Button variant="secondary" size="sm" className="mt-2" asChild>
                <a href={releaseUrl} target="_blank" rel="noopener noreferrer">
                  View release notes
                  <ExternalLink className="ml-1 h-3.5 w-3.5" />
                </a>
              </Button>
            )}
          </div>
        )}

        {updateCheck?.enabled && !updateCheck.update_available && updateCheck.latest_version && (
          <p className="text-muted-foreground">You are on the latest published release.</p>
        )}

        {updateCheck?.enabled && !updateCheck.latest_version && (
          <p className="text-muted-foreground">
            Could not reach GitHub to check for updates. You can try again or see{" "}
            <a
              href="https://github.com/Print-Partner/PrintPartner/releases"
              target="_blank"
              rel="noopener noreferrer"
              className="text-primary underline-offset-4 hover:underline"
            >
              releases on GitHub
            </a>
            .
          </p>
        )}

        {updateCheck?.checked_at && (
          <p className="text-xs text-muted-foreground">
            Last checked {formatDate(updateCheck.checked_at)}
          </p>
        )}

        <Button
          variant="secondary"
          size="sm"
          className="min-h-9"
          onClick={() => void onRefresh()}
          disabled={refreshing || !updateCheck?.enabled}
        >
          {refreshing ? "Checking…" : "Check for updates"}
        </Button>
      </CardContent>
    </Card>
  );
}
