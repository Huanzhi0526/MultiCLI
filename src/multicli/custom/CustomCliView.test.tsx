// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
import i18n from "@/lib/i18n";
import "@/i18n/multicli";
import CustomCliView, { parseArgs } from "./CustomCliView";
import type { MultiCliApi } from "../api";
import type { CustomCliInput, CustomCliView as CustomCliEntry } from "../types";

function entry(overrides: Partial<CustomCliEntry> = {}): CustomCliEntry {
  return {
    id: "custom-1",
    label: "我的工具",
    executable: {
      hostId: "local-windows",
      displayPath: "D:\\Tools\\my tool.exe",
      canonicalPath: "d:\\tools\\my tool.exe",
    },
    args: ["--version", "中文 参数"],
    cwd: "D:\\Work",
    productHint: "custom",
    launchChain: "exe",
    historyAccess: "unsupported",
    quotaAccess: "unsupported",
    verification: "user-local-import",
    createdAt: "2026-11-03T10:00:00.000Z",
    ...overrides,
  };
}

interface Recorder {
  created: CustomCliInput[];
  deleted: string[];
}

function fakeApi(items: CustomCliEntry[], recorder: Recorder): MultiCliApi {
  return {
    customCliList: () => Promise.resolve(items),
    customCliCreate: (input: CustomCliInput) => {
      recorder.created.push(input);
      return Promise.resolve(entry({ label: input.label }));
    },
    customCliDelete: (id: string) => {
      recorder.deleted.push(id);
      return Promise.resolve();
    },
  } as unknown as MultiCliApi;
}

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function render(api: MultiCliApi) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(<CustomCliView api={api} />);
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

function type(element: HTMLElement, value: string) {
  const target = element as HTMLInputElement | HTMLTextAreaElement;
  const setter = Object.getOwnPropertyDescriptor(
    target instanceof HTMLTextAreaElement
      ? HTMLTextAreaElement.prototype
      : HTMLInputElement.prototype,
    "value",
  )?.set;
  act(() => {
    setter?.call(target, value);
    target.dispatchEvent(new Event("input", { bubbles: true }));
  });
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

describe("custom CLI presets", () => {
  it("keeps one argument per line verbatim, including Chinese and quotes", () => {
    expect(parseArgs("--version\n中文 参数\n\"a b\"\n\n")).toEqual([
      "--version",
      "中文 参数",
      '"a b"',
    ]);
    // Leading spaces are part of the argument; only blank lines are dropped.
    expect(parseArgs("  two spaces\n\n\t")).toEqual(["  two spaces"]);
    expect(parseArgs("")).toEqual([]);
  });

  it("shows stored presets with an explicit unsupported history/quota claim", async () => {
    await render(fakeApi([entry()], { created: [], deleted: [] }));
    const row = byTestId("custom-cli-row-custom-1");
    expect(row.textContent).toContain("我的工具");
    expect(row.textContent).toContain("本地添加");
    expect(row.textContent).toContain("暂不支持 / 暂不支持");
    expect(row.textContent).toContain("--version · 中文 参数");
  });

  it("submits the argument array, never a joined command line", async () => {
    const recorder: Recorder = { created: [], deleted: [] };
    await render(fakeApi([], recorder));
    type(byTestId("custom-cli-label"), "我的工具");
    type(byTestId("custom-cli-executable"), "D:\\Tools\\my tool.exe");
    type(byTestId("custom-cli-cwd"), "D:\\Work");
    type(byTestId("custom-cli-args"), "--version\n中文 参数");
    await act(async () => {
      (byTestId("custom-cli-submit") as HTMLButtonElement).click();
    });
    expect(recorder.created).toHaveLength(1);
    expect(recorder.created[0].args).toEqual(["--version", "中文 参数"]);
    expect(recorder.created[0].cwd).toBe("D:\\Work");
  });

  it("needs one confirmation click before deleting and never guesses", async () => {
    const recorder: Recorder = { created: [], deleted: [] };
    await render(fakeApi([entry()], recorder));
    const button = () => byTestId("custom-cli-delete-custom-1") as HTMLButtonElement;
    await act(async () => {
      button().click();
    });
    expect(recorder.deleted).toEqual([]);
    expect(button().textContent).toContain("确认删除");
    await act(async () => {
      button().click();
    });
    expect(recorder.deleted).toEqual(["custom-1"]);
  });
});
