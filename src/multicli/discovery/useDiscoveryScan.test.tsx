// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it } from "vitest";
import { useDiscoveryScan } from "./useDiscoveryScan";
import type { MultiCliApi } from "../api";
import type { ScanView } from "../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

it("accepts completion within one generation while rejecting older generations", async () => {
  const initial: ScanView = { generation:2,status:"scanning",inFlight:true,lastAttemptAt:null,lastSuccessAt:null,error:null,installError:null,intervalMs:3000,scope:"local-windows",sampledProcesses:0,unreadableProcesses:0,trackedProcesses:0,note:"synthetic" };
  // Only refreshDiscovery is called by this hook in this scenario.
  const api = {refreshDiscovery: async () => initial} as MultiCliApi;
  let current: ReturnType<typeof useDiscoveryScan> | null = null;
  function Harness() { current=useDiscoveryScan(api); return <span>{current.scan?.status}</span>; }
  const container=document.createElement("div");const root=createRoot(container);
  try {
    await act(async()=>{root.render(<Harness/>);});
    expect(container.textContent).toBe("scanning");
    await act(async()=>{current!.applyScan({...initial,status:"ok",inFlight:false});});
    expect(container.textContent).toBe("ok");
    await act(async()=>{current!.applyScan({...initial,generation:1,status:"error"});});
    expect(container.textContent).toBe("ok");
  } finally { await act(async()=>root.unmount()); }
});
