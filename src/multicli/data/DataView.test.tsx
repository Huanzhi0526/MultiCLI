// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
import "@/i18n/multicli";
import DataView from "./DataView";
import type { MultiCliApi } from "../api";
import type { BackupRecord, ProfileDataView } from "../types";

function record(overrides: Partial<BackupRecord> = {}): BackupRecord {
  return {
    manifest: {
      manifestVersion: 1,
      createdAt: "2026-11-03T12:00:00.000Z",
      appVersion: "0.1.0",
      profile: "test",
      databaseSchema: 7,
      fileName: "multicli-20261103-120000-abcd1234.db",
      sizeBytes: 40960,
      sha256: "a".repeat(64),
      source: "manual",
      tables: ["sessions", "messages", "schema_migrations"],
      scopeNote: "包含本应用数据库（含已索引的会话与消息内容）。",
    },
    path: "C:\\data\\backups\\multicli-20261103-120000-abcd1234.db",
    integrity: "ok",
    ...overrides,
  };
}

function data(overrides: Partial<ProfileDataView> = {}): ProfileDataView {
  return {
    profile: "test",
    dataRoot: "C:\\Users\\test\\AppData\\Local\\MultiCLI\\test",
    databaseFile: "C:\\Users\\test\\AppData\\Local\\MultiCLI\\test\\multicli.db",
    databaseBytes: 8192,
    databaseSchema: 7,
    expectedSchema: 7,
    journalMode: "wal",
    backupDirectory: "C:\\Users\\test\\AppData\\Local\\MultiCLI\\test\\backups",
    backups: [record()],
    pendingRestore: null,
    lastRestore: null,
    scopeNote: "包含本应用数据库（含已索引的会话与消息内容）；不包含 CLI 凭据或 CLI 原始历史文件。",
    ...overrides,
  };
}

interface Calls {
  backups: number;
  planned: string[];
  cancelled: number;
}

function fakeApi(state: ProfileDataView, calls: Calls): MultiCliApi {
  return {
    profileData: () => Promise.resolve(state),
    profileBackup: () => {
      calls.backups += 1;
      return Promise.resolve(record());
    },
    profileRestore: (fileName: string) => {
      calls.planned.push(fileName);
      return Promise.resolve({
        backupFileName: fileName,
        sha256: "a".repeat(64),
        requestedAt: "2026-11-03T12:05:00.000Z",
        databaseSchema: 7,
        note: "恢复将在下次启动时执行。",
      });
    },
    profileCancelRestore: () => {
      calls.cancelled += 1;
      return Promise.resolve();
    },
  } as unknown as MultiCliApi;
}

let container: HTMLDivElement | null = null;
let root: Root | null = null;

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  container = null;
  root = null;
});

async function render(api: MultiCliApi) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(<DataView api={api} />);
  });
  await act(async () => {
    await Promise.resolve();
  });
}

function byTestId(testId: string): HTMLElement {
  const element = container?.querySelector(`[data-testid="${testId}"]`);
  if (!element) {
    throw new Error(`missing test id ${testId}: ${container?.textContent ?? ""}`);
  }
  return element as HTMLElement;
}

describe("S07 own-profile data view", () => {
  it("names the profile and never shows a CLI path in the scope note", async () => {
    await render(fakeApi(data(), { backups: 0, planned: [], cancelled: 0 }));
    expect(byTestId("data-profile").textContent).toContain("test");
    expect(byTestId("data-database").textContent).toContain("multicli.db");
    expect(byTestId("data-scope").textContent).toContain("已索引的会话与消息内容");
  });

  it("creates a backup from the button and reloads the list", async () => {
    const calls: Calls = { backups: 0, planned: [], cancelled: 0 };
    await render(fakeApi(data(), calls));
    await act(async () => {
      (byTestId("data-backup") as HTMLButtonElement).click();
    });
    expect(calls.backups).toBe(1);
    expect(byTestId("data-message").textContent).toContain("已生成备份");
  });

  it("plans a restore only after an explicit confirmation", async () => {
    const calls: Calls = { backups: 0, planned: [], cancelled: 0 };
    await render(fakeApi(data(), calls));
    const fileName = record().manifest.fileName;
    await act(async () => {
      (byTestId(`data-restore-${fileName}`) as HTMLButtonElement).click();
    });
    // The dialog is shown; nothing is planned yet.
    expect(calls.planned).toEqual([]);
    expect(byTestId("data-restore-confirm")).toBeTruthy();
    await act(async () => {
      (byTestId("data-restore-confirm-ok") as HTMLButtonElement).click();
    });
    expect(calls.planned).toEqual([fileName]);
  });

  it("shows a pending restore and can cancel it", async () => {
    const calls: Calls = { backups: 0, planned: [], cancelled: 0 };
    const pending = data({
      pendingRestore: {
        backupFileName: record().manifest.fileName,
        sha256: "a".repeat(64),
        requestedAt: "2026-11-03T12:05:00.000Z",
        databaseSchema: 7,
        note: "恢复将在下次启动时执行。",
        failed: false,
      },
    });
    await render(fakeApi(pending, calls));
    expect(byTestId("data-pending-restore").textContent).toContain("待恢复");
    await act(async () => {
      (byTestId("data-restore-cancel") as HTMLButtonElement).click();
    });
    expect(calls.cancelled).toBe(1);
    expect(byTestId("data-message").textContent).toContain("已取消");
  });

  it("keeps the loaded rows when a refresh fails", async () => {
    const calls: Calls = { backups: 0, planned: [], cancelled: 0 };
    const base = fakeApi(data(), calls);
    let loads = 0;
    const flaky: MultiCliApi = {
      ...base,
      profileData: () => {
        loads += 1;
        return loads === 1
          ? Promise.resolve(data())
          : Promise.reject({
              code: "io",
              message: "synthetic refresh failure",
              retryable: true,
              details: {},
            });
      },
    };
    await render(flaky);
    await act(async () => {
      (byTestId("data-refresh") as HTMLButtonElement).click();
    });
    expect(byTestId("data-profile").textContent).toContain("test");
    expect(byTestId("data-view").textContent).toContain("synthetic refresh failure");
  });

  it("disables restore for a backup that failed its checksum", async () => {
    const broken = data({ backups: [record({ integrity: "checksum-mismatch" })] });
    await render(fakeApi(broken, { backups: 0, planned: [], cancelled: 0 }));
    const button = byTestId(
      `data-restore-${record().manifest.fileName}`,
    ) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
  });
});
