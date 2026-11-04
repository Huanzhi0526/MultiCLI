// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
import i18n from "@/lib/i18n";
import "@/i18n/multicli";
import UsageView from "./UsageView";
import type { MultiCliApi } from "../api";
import type {
  PoolView,
  UsageOverviewView,
  UsageRefreshResult,
  UsageSample,
  UsageSnapshot,
} from "../types";

function sample(overrides: Partial<UsageSample> = {}): UsageSample {
  return {
    metric: "quota-window",
    unit: "percent",
    window: "short",
    label: "Codex · primary",
    usedPercent: 42,
    remainingPercent: 58,
    amount: null,
    totalTokens: null,
    inputTokens: null,
    cachedInputTokens: null,
    cacheCreationTokens: null,
    outputTokens: null,
    tokenSemantics: null,
    resetsAt: "2026-11-03T15:00:00.000Z",
    windowMinutes: 300,
    estimated: false,
    source: "codex app-server account/rateLimits/read",
    note: null,
    ...overrides,
  };
}

function snapshot(overrides: Partial<UsageSnapshot> = {}): UsageSnapshot {
  return {
    id: "snap-1",
    poolId: "pool-1",
    agentId: "codex",
    status: "ok",
    freshness: "fresh",
    sourceKind: "official-machine",
    source: "codex app-server account/rateLimits/read",
    capturedAt: "2026-11-03T10:00:00.000Z",
    expiresAt: "2026-11-03T10:05:00.000Z",
    error: null,
    retryAfterMs: null,
    nextAttemptAt: null,
    attempts: 0,
    samples: [sample()],
    note: null,
    unavailableFields: [],
    ...overrides,
  };
}

function pool(overrides: Partial<PoolView> = {}): PoolView {
  return {
    pool: {
      id: "pool-1",
      poolKey: "confirmed:codex:fingerprint",
      hostId: "local-windows",
      agentId: "codex",
      product: "Codex CLI",
      accountProfileId: "account-1",
      identityStatus: "confirmed",
      label: "Codex · a***@example.com · plus",
      installationCount: 2,
      createdAt: "2026-11-03T09:00:00.000Z",
      updatedAt: "2026-11-03T10:00:00.000Z",
    },
    snapshot: snapshot(),
    lastGood: null,
    installationIds: ["install-1", "install-2"],
    ...overrides,
  };
}

function overview(overrides: Partial<UsageOverviewView> = {}): UsageOverviewView {
  return {
    schemaVersion: 1,
    generatedAt: "2026-11-03T10:00:00.000Z",
    accounts: [
      {
        account: {
          id: "account-1",
          hostId: "local-windows",
          agentId: "codex",
          product: "Codex CLI",
          vendor: "OpenAI",
          label: "Codex · a***@example.com · plus",
          identityStatus: "confirmed",
          identityFingerprint: "fingerprint",
          plan: "plus",
          source: "codex app-server account/read",
          observedAt: "2026-11-03T10:00:00.000Z",
        },
        pools: [pool()],
      },
    ],
    unconfirmedPools: [],
    catalog: [
      {
        agentId: "codex",
        product: "Codex CLI",
        quotaAccess: "machine",
        tokenAccess: "machine",
        officialEntry: "codex app-server → account/rateLimits/read",
        status: "supported",
        installed: true,
        presence: "installed",
        evidence: "installed schema",
        limitation: "read-only",
      },
      {
        agentId: "codebuddy",
        product: "CodeBuddy Code",
        quotaAccess: "none",
        tokenAccess: "none",
        officialEntry: "/cost",
        status: "unverified",
        installed: true,
        presence: "installed",
        evidence: "no endpoint",
        limitation: "manual only",
      },
    ],
    catalogScan: {
      scannedRoots: 5,
      entriesExamined: 120,
      unreadableRoots: 0,
      truncated: false,
      conclusive: true,
      scannedAt: "2026-11-03T10:00:00.000Z",
    },
    lastAttemptAt: "2026-11-03T10:00:00.000Z",
    lastSuccessAt: "2026-11-03T10:00:00.000Z",
    note: "去重说明",
    ...overrides,
  };
}

function fakeApi(
  result: UsageOverviewView,
  refresh?: UsageRefreshResult | "reject",
): MultiCliApi {
  return {
    usageOverview: () => Promise.resolve(result),
    usageRefresh: () =>
      refresh === "reject"
        ? Promise.reject({
            code: "database",
            message: "usage database is unavailable",
            retryable: true,
            details: {},
          })
        : Promise.resolve(refresh ?? { overview: result, outcomes: [] }),
  } as unknown as MultiCliApi;
}

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function render(api: MultiCliApi) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(<UsageView api={api} />);
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

beforeAll(() => {
  void i18n.changeLanguage("zh");
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  container = null;
  root = null;
});

describe("usage centre", () => {
  it("shows one shared pool for a confirmed account and never sums installations", async () => {
    await render(fakeApi(overview()));
    const card = byTestId("usage-pool-pool-1");
    expect(card.textContent).toContain("2 个安装共用此池");
    expect(card.textContent).toContain("42.0%");
    expect(card.textContent).toContain("58.0%");
    expect(card.textContent).toContain("Codex · primary");
  });

  it("renders an unsupported reading as a state, not as zero", async () => {
    const unsupported = overview({
      accounts: [],
      unconfirmedPools: [
        pool({
          pool: {
            ...pool().pool,
            id: "pool-2",
            poolKey: "unconfirmed:codebuddy:install-9",
            agentId: "codebuddy",
            accountProfileId: null,
            identityStatus: "unconfirmed",
            label: "CodeBuddy Code",
            installationCount: 1,
          },
          snapshot: snapshot({
            agentId: "codebuddy",
            status: "unsupported",
            freshness: "unknown",
            samples: [],
            sourceKind: "unavailable",
            note: "额度不可机器读取",
            unavailableFields: ["quota", "tokens"],
          }),
        }),
      ],
    });
    await render(fakeApi(unsupported));
    const card = byTestId("usage-pool-pool-2");
    expect(card.textContent).toContain("不支持机器读取");
    expect(card.textContent).toContain("额度不可机器读取");
    expect(byTestId("usage-pool-pool-2-no-numbers").textContent).toContain("不显示为 0");
    expect(card.textContent).not.toContain("0.0%");
  });

  it("keeps a failed refresh explicit and exposes the last good reading separately", async () => {
    const failed = overview({
      accounts: [
        {
          account: overview().accounts[0].account,
          pools: [
            pool({
              snapshot: snapshot({
                status: "rate-limited",
                samples: [],
                error: {
                  code: "unsupported",
                  message: "rate limited",
                  retryable: true,
                  details: {},
                },
                retryAfterMs: 120_000,
                note: "本次未取得数值",
              }),
              lastGood: snapshot({ id: "snap-0", capturedAt: "2026-11-03T09:00:00.000Z" }),
            }),
          ],
        },
      ],
    });
    await render(fakeApi(failed));
    const card = byTestId("usage-pool-pool-1");
    expect(card.textContent).toContain("被限流");
    expect(card.textContent).toContain("120 秒后可重试");
    expect(card.textContent).toContain("上次成功读数");
    // The stale 42% only appears inside the collapsed last-good block, never as
    // the current reading.
    expect(byTestId("usage-pool-pool-1-no-numbers")).toBeTruthy();
  });

  it("labels estimated cost separately from a bill", async () => {
    const estimated = overview({
      accounts: [
        {
          account: overview().accounts[0].account,
          pools: [
            pool({
              snapshot: snapshot({
                samples: [
                  sample({
                    metric: "cost-estimate",
                    unit: "usd-micros",
                    label: "估算成本",
                    usedPercent: null,
                    remainingPercent: null,
                    amount: 1234,
                    estimated: true,
                  }),
                ],
              }),
            }),
          ],
        },
      ],
    });
    await render(fakeApi(estimated));
    expect(byTestId("usage-pool-pool-1").textContent).toContain("估算");
  });

  it("lists the provider catalogue with manual entries and limitations", async () => {
    await render(fakeApi(overview()));
    const codex = byTestId("usage-catalog-codex");
    expect(codex.textContent).toContain("machine");
    expect(codex.textContent).toContain("supported");
    const buddy = byTestId("usage-catalog-codebuddy");
    expect(buddy.textContent).toContain("/cost");
    expect(buddy.textContent).toContain("manual only");
  });

  it("never paints a stale but ok reading as healthy", async () => {
    const stale = overview({
      accounts: [
        {
          account: overview().accounts[0].account,
          pools: [
            pool({
              snapshot: snapshot({ status: "ok", freshness: "stale" }),
            }),
          ],
        },
      ],
    });
    await render(fakeApi(stale));
    const card = byTestId("usage-pool-pool-1");
    expect(card.textContent).toContain("读数已陈旧");
    expect(card.textContent).toContain("已过期，不代表当前状态");
  });

  it("keeps the previous readings and shows a failed refresh", async () => {
    await render(fakeApi(overview(), "reject"));
    await act(async () => {
      (byTestId("usage-refresh") as HTMLButtonElement).click();
    });
    const panel = container?.querySelector('[data-testid="usage-refresh-error"]');
    expect(panel).not.toBeNull();
    expect(panel?.textContent).toContain("usage database is unavailable");
    // The old pool is still on screen.
    expect(byTestId("usage-pool-pool-1")).toBeTruthy();
  });

  it("expires a visible reading while the window stays open", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-11-03T10:00:00.000Z"));
    try {
      await render(fakeApi(overview()));
      const card = byTestId("usage-pool-pool-1");
      expect(card.textContent).toContain("读数新鲜");
      expect(card.textContent).not.toContain("已过期");
      // Six minutes later the five-minute TTL has passed; the page is still
      // open and no new read happened.
      await act(async () => {
        vi.advanceTimersByTime(6 * 60 * 1000);
      });
      const expired = byTestId("usage-pool-pool-1");
      expect(expired.textContent).toContain("已过期");
      expect(expired.textContent).toContain("读数已陈旧");
      expect(expired.textContent).not.toContain("读数新鲜");
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the normal refresh on the backend backoff and forces only on demand", async () => {
    const calls: boolean[] = [];
    const api = {
      usageOverview: async () => overview(),
      usageRefresh: async (_agent: unknown, force: unknown) => {
        calls.push(Boolean(force));
        return { overview: overview(), outcomes: [] };
      },
    } as unknown as MultiCliApi;
    await render(api);
    await act(async () => {
      byTestId("usage-refresh").click();
    });
    await act(async () => {
      byTestId("usage-refresh-force").click();
    });
    expect(calls).toEqual([false, true]);
  });

  it("retries a failed refresh without forcing past the backoff", async () => {
    const calls: boolean[] = [];
    let fail = true;
    const api = {
      usageOverview: async () => overview(),
      usageRefresh: async (_agent: unknown, force: unknown) => {
        calls.push(Boolean(force));
        if (fail) {
          fail = false;
          throw { code: "database", message: "unavailable", retryable: true, details: {} };
        }
        return { overview: overview(), outcomes: [] };
      },
    } as unknown as MultiCliApi;
    await render(api);
    await act(async () => {
      byTestId("usage-refresh").click();
    });
    const panel = container?.querySelector('[data-testid="usage-refresh-error"]');
    expect(panel).not.toBeNull();
    const retry = panel?.querySelector("button") as HTMLButtonElement | null;
    expect(retry).not.toBeNull();
    await act(async () => {
      retry?.click();
    });
    expect(calls).toEqual([false, false]);
  });

  it("shows skipped providers with their backoff reason", async () => {
    const base = overview();
    await render(
      fakeApi(base, {
        overview: base,
        outcomes: [
          {
            agentId: "codex",
            status: "rate-limited",
            skipped: true,
            reasonCode: "backoff",
            reason: "退避中",
            retryAfterMs: 60_000,
          },
        ],
      }),
    );
    const refresh = byTestId("usage-refresh") as HTMLButtonElement;
    await act(async () => {
      refresh.click();
    });
    expect(byTestId("usage-outcomes").textContent).toContain("已跳过");
    expect(byTestId("usage-outcomes").textContent).toContain("退避中，本次未重新读取");
  });

  it("keeps an unconfirmed install apart from a confirmed absence", async () => {
    const mixed = overview({
      catalog: [
        {
          ...overview().catalog[0],
          agentId: "gemini",
          product: "Gemini CLI",
          status: "unverified",
          installed: false,
          presence: "undiscovered",
        },
        {
          ...overview().catalog[0],
          agentId: "aider",
          product: "Aider",
          installed: false,
          presence: "not-installed",
        },
      ],
    });
    await render(fakeApi(mixed));
    expect(byTestId("usage-presence-gemini").textContent).toContain("尚未确认");
    expect(byTestId("usage-presence-aider").textContent).toContain("已确认未安装");
    // The old copy conflated both states into one word.
    expect(byTestId("usage-presence-gemini").textContent).not.toContain("已确认未安装");
  });

  it("explains an inconclusive install scan and points at the scan page", async () => {
    const inconclusive = overview({
      catalogScan: {
        scannedRoots: 0,
        entriesExamined: 0,
        unreadableRoots: 0,
        truncated: false,
        conclusive: false,
        scannedAt: "2026-11-03T10:00:00.000Z",
      },
    });
    let opened = 0;
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    await act(async () => {
      root?.render(
        <UsageView api={fakeApi(inconclusive)} onOpenInstallations={() => (opened += 1)} />,
      );
    });
    await act(async () => {
      await Promise.resolve();
    });
    const banner = byTestId("usage-catalog-inconclusive");
    expect(banner.textContent).toContain("安装扫描不完整");
    expect(banner.textContent).toContain("不代表产品未安装");
    (byTestId("usage-catalog-open-installations") as HTMLButtonElement).click();
    expect(opened).toBe(1);
  });
});
