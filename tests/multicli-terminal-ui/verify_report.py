"""Validate the fixed 20 x 60s S04 run, then print resource curves for review."""
import json
import math
import sys
from pathlib import Path


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def p95(values):
    values = sorted(values)
    return values[math.ceil(len(values) * .95) - 1]


def main():
    report = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    rounds = report["rounds"]
    check(len(rounds) == 20, "requires all 20 rounds in one report")
    check(not report["failures"], "global fixture failures")
    samples = []
    curve = []
    source_durations = []
    for index, round_ in enumerate(rounds):
        label = f"round {index + 1}"
        check(round_["round"] == index, f"{label}: nonconsecutive round")
        check(round_["requestedSeconds"] == 60, f"{label}: duration changed")
        check(round_["flooders"] == 8, f"{label}: terminal count changed")
        check(round_["activeTerminalsAfterOpen"] == 8, f"{label}: open count")
        check(round_["sampleCount"] == 40, f"{label}: incomplete samples")
        check(round_["sampleMisses"] == 0, f"{label}: missed ACK")
        check(not round_["failures"], f"{label}: fixture errors")
        check(round_["activeTerminalsAfterClose"] == 0, f"{label}: active terminals")
        check(round_["unfinalizedRunsAfterClose"] == 0, f"{label}: durable Runs")
        check(not round_["syntheticProcessSurvivors"], f"{label}: live/unverified children")
        check(len(round_["syntheticProcessChecks"]) == 8, f"{label}: missing exit checks")
        for child in round_["syntheticProcessChecks"]:
            check(child["status"] in ("gone", "exited_with_code_0"), f"{label}: {child}")
        check(len(round_["perTerminal"]) == 8, f"{label}: incomplete source records")
        for terminal in round_["perTerminal"]:
            source = terminal["sourceFlood"]
            check(source["bytes"] == 100 * 1024 * 60, f"{label}: source bytes")
            check(source["elapsedMs"] >= 60000, f"{label}: source ended early")
            source_durations.append(source["elapsedMs"])
            check(terminal["droppedBytes"] == 0, f"{label}: output lost")
        latencies = [sample["latencyMs"] for sample in round_["samples"]]
        check(len(latencies) == 40, f"{label}: sample data count")
        check(p95(latencies) <= 200, f"{label}: p95 exceeds 200ms")
        samples.extend(latencies)
        resources = round_["processTreeResourcesAfter"]
        check(resources["unverifiedHandleCounts"] == 0, f"{label}: unknown handles")
        check(resources["unverifiedParentLinks"] == 0, f"{label}: unknown process ancestry")
        curve.append({"round": index + 1, "p95Ms": round(p95(latencies), 2),
                      "nativeHandles": round_["handlesAfter"],
                      "treeHandles": resources["totalHandles"],
                      "workingSetMiB": round(resources["workingSetBytes"] / 1048576, 1)})
    print(json.dumps({"checksPassed": True, "roundCount": len(rounds),
                      "sampleCount": len(samples), "p95Ms": p95(samples),
                      "maxMs": max(samples),
                      "sourceDurationMs": [min(source_durations), max(source_durations)],
                      "resourceCurve": curve}, indent=2))


if __name__ == "__main__":
    main()
