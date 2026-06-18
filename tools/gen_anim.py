#!/usr/bin/env python3
"""
Procedurally generate pixel-art animation frames from a single still pose.

Most desktop-pet animations (bounce, lean, stretch, tail wag, idle breathing)
are whole-sprite geometric transforms, not redrawn poses. This lets you take
any 32x32 base pose -- your own art or a sprite you found online -- and
produce an animated frame row without drawing each frame by hand.

Usage:
    pip install pillow
    python3 tools/gen_anim.py BASE_FRAME.png --type bounce --frames 8 \
        --sheet assets/own.png --row 4

    # preview only, no sheet write:
    python3 tools/gen_anim.py BASE_FRAME.png --type wag --frames 6 --preview out.png

BASE_FRAME.png must be a 32x32 RGBA image (crop it from a sheet first if needed).
"""
import argparse
import math
from pathlib import Path

from PIL import Image

FRAME = 32


def squash_stretch(frame, scale_x, scale_y, anchor="bottom"):
    w, h = frame.size
    nw, nh = max(1, round(w * scale_x)), max(1, round(h * scale_y))
    scaled = frame.resize((nw, nh), Image.NEAREST)
    canvas = Image.new("RGBA", (FRAME, FRAME), (0, 0, 0, 0))
    x = (FRAME - nw) // 2
    y = FRAME - nh if anchor == "bottom" else (FRAME - nh) // 2
    canvas.paste(scaled, (x, y), scaled)
    return canvas


def skew_x(frame, shift_top, pivot="bottom"):
    w, h = frame.size
    canvas = Image.new("RGBA", (FRAME, FRAME), (0, 0, 0, 0))
    px = frame.load()
    out = canvas.load()
    for y in range(h):
        t = (1.0 - y / h) if pivot == "bottom" else (y / h)
        off = round(shift_top * t)
        for x in range(w):
            nx = x + off
            if 0 <= nx < FRAME:
                out[nx, y] = px[x, y]
    return canvas


def shift_region(frame, x_from, dx, dy):
    """Shift the columns at and right of x_from by (dx, dy). Used for tail/ear wiggle."""
    w, h = frame.size
    canvas = frame.copy()
    region = frame.crop((x_from, 0, w, h))
    canvas.paste(Image.new("RGBA", region.size, (0, 0, 0, 0)), (x_from, 0))
    canvas.paste(region, (x_from + dx, dy), region)
    return canvas


def gen_bounce(base, n):
    frames = []
    for i in range(n):
        t = i / (n - 1)
        s = math.sin(t * math.pi)
        scale_y = 1.0 - 0.25 * s if t < 0.5 else 1.0 + 0.15 * math.sin((t - 0.5) * 2 * math.pi)
        scale_x = 1.0 + (1.0 - scale_y) * 0.5
        frames.append(squash_stretch(base, scale_x, max(0.5, scale_y)))
    return frames


def gen_lean(base, n, amplitude=5):
    frames = []
    for i in range(n):
        t = i / n
        shift = math.sin(t * 2 * math.pi) * amplitude
        frames.append(skew_x(base, shift))
    return frames


def gen_elongate(base, n):
    """Stretch tall and thin, then settle -- a 'getting up' or curiosity stretch."""
    frames = []
    for i in range(n):
        t = i / (n - 1)
        s = math.sin(t * math.pi)
        scale_y = 1.0 + 0.3 * s
        scale_x = 1.0 - 0.15 * s
        frames.append(squash_stretch(base, scale_x, scale_y))
    return frames


def gen_breathe(base, n, amplitude=0.05):
    frames = []
    for i in range(n):
        t = i / n
        scale_y = 1.0 + amplitude * math.sin(t * 2 * math.pi)
        frames.append(squash_stretch(base, 1.0, scale_y))
    return frames


def gen_wag(base, n, tail_x=22, amplitude=2):
    frames = []
    for i in range(n):
        t = i / n
        dy = round(math.sin(t * 2 * math.pi) * amplitude)
        frames.append(shift_region(base, tail_x, 0, dy))
    return frames


GENERATORS = {
    "bounce": gen_bounce,
    "lean": gen_lean,
    "elongate": gen_elongate,
    "breathe": gen_breathe,
    "wag": gen_wag,
}


def write_preview(frames, path, scale=4):
    w = FRAME * len(frames)
    out = Image.new("RGBA", (w, FRAME), (0, 0, 0, 0))
    for i, f in enumerate(frames):
        out.paste(f, (i * FRAME, 0), f)
    out.resize((w * scale, FRAME * scale), Image.NEAREST).save(path)


def write_to_sheet(sheet_path, row, frames):
    sheet_path = Path(sheet_path)
    needed_w = FRAME * len(frames)
    needed_h = FRAME * (row + 1)
    if sheet_path.exists():
        sheet = Image.open(sheet_path).convert("RGBA")
    else:
        sheet = Image.new("RGBA", (needed_w, needed_h), (0, 0, 0, 0))

    w = max(sheet.width, needed_w)
    h = max(sheet.height, needed_h)
    if (w, h) != sheet.size:
        canvas = Image.new("RGBA", (w, h), (0, 0, 0, 0))
        canvas.paste(sheet, (0, 0))
        sheet = canvas

    # clear the target row first so a shorter new animation doesn't leave stale frames
    sheet.paste(Image.new("RGBA", (sheet.width, FRAME), (0, 0, 0, 0)), (0, row * FRAME))
    for i, f in enumerate(frames):
        sheet.paste(f, (i * FRAME, row * FRAME), f)
    sheet.save(sheet_path)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("base_frame", help="32x32 RGBA source pose")
    ap.add_argument("--type", choices=GENERATORS.keys(), required=True)
    ap.add_argument("--frames", type=int, default=6)
    ap.add_argument("--sheet", help="sprite sheet to write the row into, e.g. assets/own.png")
    ap.add_argument("--row", type=int, help="row index in --sheet to write")
    ap.add_argument("--preview", help="write a standalone preview strip PNG here instead of/as well as --sheet")
    args = ap.parse_args()

    base = Image.open(args.base_frame).convert("RGBA")
    if base.size != (FRAME, FRAME):
        raise SystemExit(f"base frame must be {FRAME}x{FRAME}, got {base.size}")

    frames = GENERATORS[args.type](base, args.frames)

    if args.preview:
        write_preview(frames, args.preview)
        print(f"wrote preview: {args.preview}")

    if args.sheet:
        if args.row is None:
            raise SystemExit("--row is required when writing to --sheet")
        write_to_sheet(args.sheet, args.row, frames)
        print(f"wrote {len(frames)} frames to row {args.row} of {args.sheet}")


if __name__ == "__main__":
    main()
