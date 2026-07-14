# MUD

A digital clay modeller. See [`DESIGN.md`](DESIGN.md) for the design
notes; this file is just how to build and run.

## Requirements

- **Rust** stable, currently 1.85+ (managed by `rust-toolchain.toml`).
- **Linux desktop dependencies** for Bevy's X11 backend:

```bash
sudo apt install libxkbcommon-dev libxkbcommon-x11-0 libwayland-dev \
                 libasound2-dev libudev-dev libxcursor-dev libxi-dev \
                 libxrandr-dev libgl1-mesa-dev
```

`libxkbcommon-x11-0` is the one people miss; without it the app
panics at startup with `Library libxkbcommon-x11.so could not be
loaded`.

macOS and Windows have no extra system deps beyond the usual toolchain.

## Build and run

```bash
cargo run -p sculpt-app --release
```

The first release build takes a few minutes (Bevy is fat). Subsequent
iterations are fast because dev profile compiles deps at `opt-level = 3`
(see the workspace `Cargo.toml`).

## Controls

**Camera and turntable**

| Input | Action |
|---|---|
| Right-drag | Orbit camera |
| Middle-drag | Pan camera |
| Scroll | Zoom |
| Hold `Q` / `E` | Turntable left / right |

**Sculpting**

| Input | Action |
|---|---|
| Left-drag | Add/Remove tool: remove material |
| Shift + Left-drag | Add/Remove tool: add material (sideways hold + Q/E can draw a ring; over empty bench = deposit a blob) |
| `[` / `]` or `-` / `=` | Shrink / grow the active tool (keyboard) |
| Shift + scroll wheel | Shrink / grow the active tool (mouse / trackpad) |
| `M` | Toggle magic-clay (Add/Remove only) |
| `S` | Toggle mirror symmetry (piece-local X = 0) |
| `Ctrl+N` | Clear the worktable (empty grid, clears undo) |
| `Shift+N` | Insert Primitive… (sphere / cube / cylinder / torus) |
| `Ctrl+Z` | Undo last stroke |
| `Ctrl+Y` or `Ctrl+Shift+Z` | Redo |

**Tool palette**

| Key | Tool |
|---|---|
| `1` | Add/Remove (default) |
| `2` | Cookie cutter — circle |
| `3` | Cookie cutter — square |
| `4` | Cookie cutter — hexagon |
| `5` | Cookie cutter — star |
| `6` | Wire cutter (LMB-drag slices along the drag line) |
| `7` | Smooth (hold LMB to polish high-frequency detail) |
| `8` | Paddle (hold LMB to press a flat) |
| `9` | Select (LMB picks a connected piece; HUD shows its size) |
| `Delete` / `Backspace` | Remove the selected piece |
| `A` | Toggle **active-only** sculpting (only affects the selected piece) |
| `Ctrl+G` | Rest every floating piece on the workbench (rigid drop, undoable) |
| `Ctrl+E` | Export the current piece to a binary STL file (Z-up, print-ready) |
| `Ctrl+Shift+E` | Export STL As… — dialog with a filename you choose |
| `Ctrl+S` | Save the current piece as a native `.mudclay` project file (auto-timestamped) |
| `Ctrl+Shift+S` | Save As… — dialog with a filename you choose |
| `Ctrl+O` | Open… — picker showing every `.mudclay` in the working directory |
| `Ctrl+Shift+O` | Quick-reopen the most recently modified project |

All of the above are also reachable from the on-screen UI: the top
**File** menu (Save / Load / Export STL / Quit), the left **Tools**
palette (click a tool to pick it up), and the bottom status strip
(click the *Symmetry* / *Magic clay* readouts to toggle them).

`Esc` to quit.

## Layout

```
Cargo.toml              # workspace
rust-toolchain.toml
crates/
  sculpt-core/          # Bevy-independent geometry engine
    src/
      grid.rs           # SDF grid + sampling + ray march
      brush.rs          # spherical brush ops (Stage-0 CSG + Stage-1 magic clay)
      mesh.rs           # chunked surface-nets extraction
  sculpt-app/           # Bevy 0.15 shell
    src/
      main.rs           # window, lighting, plugin registration
      camera.rs         # orbit camera controller
      turntable.rs      # Q/E turntable state
      workpiece.rs      # SDF resource + chunk-based re-meshing
      sculpt.rs         # mouse → SDF ray march → brush stamp
      undo.rs           # per-stroke journal, Ctrl+Z / Ctrl+Y
```
