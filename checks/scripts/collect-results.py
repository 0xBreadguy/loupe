#!/usr/bin/env python3
"""Curate completed experiment protocols; preserve trials, hashes and error counts.

No builds, GUI or measurements. Run after all named protocols finish. Raw samples
stay local; this report keeps every timed trial/key without repetitive process maps.
"""
import hashlib
import json
import math
from pathlib import Path
import statistics

ROOT = Path(__file__).resolve().parents[1]
NAMES = ["canvas-demo", "canvas-large", "model-demo", "model-large", "gui-idle",
         "gui-updates", "startup", "presentation-demo", "presentation-large",
         "presentation-streaming-control", "presentation-confirm-demo",
         "presentation-confirm-large", "tuning"]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def paired_analysis(report):
    groups = {}
    for trial in report["trials"]:
        groups.setdefault(trial["trial"], {})[trial["side"]] = statistics.median(
            op["delivered_to_present_ms"] for op in trial["result"]["operations"])
    differences = [g["typescript_immediate_input"] - g["native"] for g in groups.values()]
    positives = sum(d > 0 for d in differences)
    negatives = sum(d < 0 for d in differences)
    n = positives + negatives
    p = min(1., 2 * sum(math.comb(n, k) for k in range(min(positives, negatives) + 1)) / 2 ** n)
    return {"unit": "fresh-process group, each containing four scripted keys",
            "trial_medians_ms": groups,
            "immediate_ts_minus_native_median_ms": differences,
            "native_lower_groups": positives, "native_higher_groups": negatives,
            "exact_two_sided_paired_sign_test_p": p,
            "limits": "Keys within a process are not treated as independent trials. This conditional test does not remove shared-host drift, instrumentation or platform limitations."}


def main():
    protocols, raw_hashes = {}, {}
    for name in NAMES:
        path = ROOT / "measurements" / ("deep-" + name + ".json")
        report = json.loads(path.read_text())
        assert report["errors"] == 0, name
        assert report.get("completed_protocol", True), name
        raw_hashes[path.name] = digest(path)
        if name.startswith("gui-"):
            for trial in report["trials"]:
                result = trial["result"]
                assert result["errors"] == 0 and result["missing_memory_samples"] == 0
                result["sample_count"] = len(result.pop("samples"))
                if name == "gui-updates":
                    replay = result["completed_replay"]
                    assert replay["updates_applied"] == 29 and replay["updates_skipped"] == 0
        elif name.startswith("presentation-"):
            assert report["reportable"]
            expected_groups = report["conditions"]["trials_per_side"]
            assert len(report["trials"]) == 3 * expected_groups
            assert report["verified_keys"] == 12 * expected_groups
            report["paired_analysis"] = paired_analysis(report)
            report["shared_oracle"] = report["trials"][0]["result"]["oracle"]
            report["limitations"] = [
                f"{4 * expected_groups} keys/side in {expected_groups} fresh processes; p95 remains a limited nearest-rank observation."
                if item.startswith("Twenty keys/side") else item for item in report["limitations"]]
            report["curation_note"] = "Corrected the raw generator's fixed twenty-key wording for ten-group cohorts. Counts/timings and raw hashes are unchanged."
            for trial in report["trials"]:
                result = trial["result"]
                assert result.pop("oracle") == report["shared_oracle"]
                assert result["errors"] == 0 and result["verified_keys"] == 4
                assert all(result["counts"].get(key, 0) == 0 for key in (
                    "discarded", "additional_feedback_pending_at_capture", "trace_records_dropped"))
                for op in result["operations"]:
                    assert op["errors"] == 0 and op["semantics"]["equal"]
                    assert op["clock_id"] == 1
                    assert op["flags"] == 1 and op["refresh_ns"] == 0
                    rows = op["semantics"].pop("rows")
                    op["semantics"]["row_count"] = len(rows)
                    op["semantics"]["row_order_sha256"] = hashlib.sha256(json.dumps(rows, separators=(",", ":")).encode()).hexdigest()
                    op.pop("key_command", None)  # IPC duration is explicitly outside the metric.
                    # Absolute endpoint times, phases, frame/state hashes, errors,
                    # attribution IDs and clock/flags remain. Intermediate absolute
                    # stamps are redundant with the retained phase durations.
                    for field in ("ack_mono_ns", "frame_mono_ns", "prepared_mono_ns", "commit_mono_ns", "feedback_mono_ns", "tag_mono_ns", "encoded_output_mono_ns", "core_key_time_ms", "core_key_vs_callback_offset_ms_unverified_domain"):
                        op.pop(field, None)
                initial_rows = result["initial_semantics"].pop("rows")
                result["initial_semantics"]["row_count"] = len(initial_rows)
                result["initial_semantics"]["row_order_sha256"] = hashlib.sha256(json.dumps(initial_rows, separators=(",", ":")).encode()).hexdigest()
                for field in ("launch", "stop", "windows_after_stop", "capture_command"):
                    result.pop(field, None)
        elif name == "startup":
            for trial in report["trials"]:
                result = trial["result"]
                assert result["errors"] == 0
                trial["result"] = {key: result[key] for key in ("errors", "geometry", "settle_proxy_ms")}
                trial["result"]["launch_returncode"] = result["launch"]["returncode"]
                trial["result"]["stop_returncode"] = result["stop"]["returncode"]
        protocols[name] = report
    binary_hash = protocols["canvas-demo"]["native_sha256"]
    for name, report in protocols.items():
        if name != "tuning":
            assert report["native_sha256"] == binary_hash, name
    before_path, after_path = (ROOT / "measurements" / name for name in ("deep-raster-before.json", "deep-raster-final.json"))
    raster = {"scope": "Optional diagnostic of matched calm captures; not pixel identity or aesthetic certification."}
    if before_path.is_file() and after_path.is_file():
        before, after = (json.loads(path.read_text()) for path in (before_path, after_path))
        assert before["frame_sha256"] == after["frame_sha256"]
        raster.update(before=before, after=after)
    else:
        raster["unavailable"] = "Matching optional raster-audit reports were not supplied; no raster claim."
    result = {"schema": 1, "implementation_revision": protocols["canvas-demo"]["experiment_revision"],
              "native_sha256": binary_hash, "selected_profile": "release",
              "claim": "Tested pinned-baseline cell/behavior parity with lower Canvas cost, PSS and instrumented delivered-input-to-headless-compositor medians; no release, physical display or general language-wide claim.",
              "raw_report_sha256": raw_hashes, "protocols": protocols,
              "raster": raster,
              "counts": {"protocols": len(protocols), "protocol_errors": 0,
                         "canvas_complete_timed_operations": sum(protocols[n]["verified_operations"] for n in ("canvas-demo", "canvas-large")),
                         "tuning_complete_timed_operations": protocols["tuning"]["verified_operations"],
                         "presentation_verified_keys": sum(r["verified_keys"] for n, r in protocols.items() if n.startswith("presentation-"))},
              "curator_sha256": digest(Path(__file__))}
    output = ROOT / "results/deep-measurements.json"
    output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"output": str(output), **result["counts"]}, indent=2))


if __name__ == "__main__":
    main()
