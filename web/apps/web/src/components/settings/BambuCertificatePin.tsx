import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { fetchTlsPin, resetTlsPin } from "../../api/endpoints/integrations";
import { queryKeys } from "../../queries/keys";
import ConfirmDialog from "../ConfirmDialog";
import { Button } from "../ui/button";

type Props = {
  integrationId: string;
  disabled: boolean;
  onMessage: (message: string) => void;
  onError: (message: string) => void;
};

/**
 * The Bambu certificate pin for one printer host. Only Test connection records
 * a pin, so this row shows it and offers the deliberate reset a replaced or
 * reset printer needs. Rendered only while the desktop rebuild is on.
 */
export default function BambuCertificatePin({ integrationId, disabled, onMessage, onError }: Props) {
  const queryClient = useQueryClient();
  const pin = useQuery({
    queryKey: queryKeys.tlsPin(integrationId),
    queryFn: () => fetchTlsPin(integrationId),
  });
  const [resetting, setResetting] = useState(false);

  const onReset = async () => {
    setResetting(true);
    try {
      await resetTlsPin(integrationId);
      onMessage("Certificate reset. Run Test connection to pin the printer's current certificate.");
      await queryClient.invalidateQueries({ queryKey: queryKeys.tlsPin(integrationId) });
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setResetting(false);
    }
  };

  const data = pin.data;
  const label = !data
    ? pin.isError
      ? "Certificate state unavailable."
      : "Checking certificate…"
    : data.pinned
      ? `Certificate pinned · ${data.sha256_prefix} · ${new Date(data.pinned_at).toLocaleDateString()}`
      : "Not pinned. Test connection to pin.";

  return (
    <div className="flex flex-wrap items-center gap-2">
      <p className="font-mono text-xs text-muted-foreground">{label}</p>
      {data?.pinned ? (
        <ConfirmDialog
          trigger={
            <Button variant="outline" size="sm" disabled={disabled || resetting}>
              Reset certificate
            </Button>
          }
          title="Reset this printer's certificate?"
          description="Print Partner forgets the pinned certificate. The next Test connection trusts whatever certificate answers at this address, so run it only while you're sure the printer on your network is this same device."
          confirmLabel="Reset certificate"
          disabled={disabled || resetting}
          onConfirm={() => void onReset()}
        />
      ) : null}
    </div>
  );
}
