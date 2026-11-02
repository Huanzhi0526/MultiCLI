import { useCallback, useEffect, useState } from "react";
import type { MultiCliApi } from "../api";
import { toAppError } from "../navigation";
import type { AppError, DiscoverySettings, ScanView } from "../types";

/**
 * Shared discovery scan state. `refresh` calls the manual refresh command,
 * which merges into the in-flight scan instead of starting another scanner.
 */
export function useDiscoveryScan(api: MultiCliApi) {
  const [scan, setScan] = useState<ScanView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<AppError | null>(null);
  const [settings, setSettings] = useState<DiscoverySettings | null>(null);
  const [settingsError, setSettingsError] = useState<AppError | null>(null);

  const refresh = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const next = await api.refreshDiscovery();
      setScan(next);
      return next;
    } catch (raw: unknown) {
      setError(toAppError(raw));
      return null;
    } finally {
      setBusy(false);
    }
  }, [api]);

  const changeInterval = useCallback(
    async (intervalMs: number) => {
      setSettingsError(null);
      try {
        const next = await api.updateDiscoverySettings(intervalMs);
        setSettings(next);
      } catch (raw: unknown) {
        setSettingsError(toAppError(raw));
      }
    },
    [api],
  );

  /** The list responses carry the live scan object; keep the status bar in
   * sync with the same data the rows came from. */
  const applyScan = useCallback((next: ScanView | null) => {
    if (next) {
      // A generation changes from scanning to completed without changing its
      // number; do not retain an in-flight/error/settings snapshot indefinitely.
      setScan((current) =>
        current && current.generation > next.generation ? current : next,
      );
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return {
    scan,
    busy,
    error,
    settings,
    settingsError,
    refresh,
    changeInterval,
    applyScan,
  };
}
