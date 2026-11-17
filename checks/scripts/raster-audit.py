#!/usr/bin/env python3
"""Compare actual calm captures at a verified shared terminal grid.

This diagnoses raster differences, not semantic parity or aesthetic approval.
It cannot pass by drawing less: every nonblank reference cell is included.
"""
import argparse
import hashlib
import json
from pathlib import Path
from PIL import Image


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def audit(reference, candidate, frame, cell=(8, 18), inset=(14, 14)):
    if reference.size != candidate.size:
        raise ValueError("capture dimensions differ")
    w, h = frame["w"], frame["h"]
    cw, ch = cell
    ox, oy = inset
    if len(frame["ch"]) != w * h or ox + w * cw > reference.width or oy + h * ch > reference.height:
        raise ValueError("complete reference grid does not fit the actual captures")
    groups = {}
    for index, cp in enumerate(frame["ch"]):
        if cp in (0, 32, 160):
            continue
        category = "ascii" if 32 < cp < 127 else "graphics" if 0x2500 <= cp <= 0x259f or 0x2800 <= cp <= 0x28ff else "other_unicode"
        counts = groups.setdefault(category, dict(cells=0, pixels=0, changed_pixels=0, absolute_rgb_error=0, max_channel_error=0))
        counts["cells"] += 1
        x, y = ox + index % w * cw, oy + index // w * ch
        for row in range(y, y + ch):
            for column in range(x, x + cw):
                a, b = reference.getpixel((column, row)), candidate.getpixel((column, row))
                errors = [abs(a[k] - b[k]) for k in range(3)]
                counts["pixels"] += 1
                counts["changed_pixels"] += any(errors)
                counts["absolute_rgb_error"] += sum(errors)
                counts["max_channel_error"] = max(counts["max_channel_error"], *errors)
    for counts in groups.values():
        counts["mean_absolute_channel_error_0_255"] = counts["absolute_rgb_error"] / (3 * counts["pixels"])
        counts["changed_pixel_fraction"] = counts["changed_pixels"] / counts["pixels"]
    return groups


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--native", type=Path, required=True)
    parser.add_argument("--frame", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    frame = json.loads(args.frame.read_text())
    baseline = Image.open(args.baseline).convert("RGB")
    native = Image.open(args.native).convert("RGB")
    result = {"baseline_sha256": sha(args.baseline), "native_sha256": sha(args.native),
              "frame_sha256": sha(args.frame), "window_pixels": baseline.size,
              "grid": [frame["w"], frame["h"]], "cell_pixels": [8, 18], "inset_pixels": [14, 14],
              "groups": audit(baseline, native, frame),
              "limitations": ["A diagnostic difference metric, not visual certification.",
                              "Requires independently verified equal data, calm time, colors and cell geometry.",
                              "Does not prove animation, input handling, presentation latency or accessibility."]}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result["groups"], indent=2))


if __name__ == "__main__":
    main()
