# Nekopet

A desktop pet cat for Hyprland (and other wlr-based Wayland compositors). Sits as a transparent overlay on your screen, reacts to your keyboard, and can be dragged anywhere.

<img src="assets/demo.gif" width="300" />

## Features

- **Idle** — cat sits in the corner with eyes that follow your cursor
- **Typing** — detects keyboard input and plays a typing animation; the cat tints orange → red based on how fast you type
- **Drag** — click and hold to pick up the cat and drop it anywhere on screen
- **Stretch** — on a timer, the cat grows to half-screen size, plays a stretch animation, then shrinks back

## Requirements

- A wlr-layer-shell compositor (Hyprland, Sway, river, etc.)
- Rust toolchain (`rustup.rs`)

## Build & Run

```bash
git clone https://github.com/dhruvkumar1805/nekopet
cd nekopet
cargo run --release
```

The binary expects to be run from the project root so it can find `assets/own.png`.

## Configuration

On first run, a config file is created at `~/.config/nekopet/config.toml`:

```toml
scale               = 3       # sprite size multiplier (3 = 96×96px from 32×32 source)
anim_ms             = 120     # milliseconds per animation frame
corner              = "bottom-right"  # starting corner: bottom-right, bottom-left, top-right, top-left
stretch_every_secs  = 1800    # how often the stretch animation fires (seconds); 0 to disable
stretch_anim_ms     = 400     # frame speed for the stretch animation
stretch_hold_ms     = 1500    # how long the last stretch frame holds before zooming out
bounce_every_secs   = 60      # how often the bounce animation fires (seconds); 0 to disable
lean_every_secs     = 45      # how often the lean animation fires (seconds); 0 to disable
```

## Custom Sprites

All animations come from a single sprite sheet at `assets/own.png`. Each row is 32×32 pixels per frame, left to right.

| Row | Animation | Frames                               |
| --- | --------- | ------------------------------------ |
| 0   | Idle      | 4                                    |
| 1   | Typing    | 4                                    |
| 2   | Stretch   | auto-detected (any non-empty frames) |
| 3   | Drag      | auto-detected (any non-empty frames) |
| 4   | Bounce    | auto-detected (any non-empty frames) |
| 5   | Lean      | auto-detected (any non-empty frames) |

The sheet is scaled up using nearest-neighbor interpolation, so pixel art looks sharp at any scale.

To replace the cat with your own character, edit `assets/own.png` keeping the same row layout. Frames are read left-to-right and stop at the first empty (fully transparent) cell.

## Eye Tracking

During idle and drag states, the cat's pupils shift toward the cursor while it's hovering over the cat — Wayland doesn't let clients query the cursor position outside their own surface, so tracking only works within the cat's bounds. The pupil positions are hardcoded for the default sprite at source pixels (12, 9) and (18, 9). If you draw a different character with eyes in different positions, update these constants in `src/main.rs`:

```rust
let lx = 12 * s;  // left pupil x in source pixels
let rx = 18 * s;  // right pupil x in source pixels
// py = 9 * s     // pupil y in source pixels
```

## Autostart

Add to your Hyprland config:

```
exec-once = /path/to/nekopet/target/release/nekopet
```
