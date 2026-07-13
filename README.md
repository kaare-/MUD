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

| Input | Action |
|---|---|
| Right-drag | Orbit camera |
| Middle-drag | Pan camera |
| Scroll | Zoom |
| Left-drag | Press (carve with magic-clay displacement) |
| Shift + Left-drag | Pull (add material) |
| Hold `Q` / `E` | Turntable left / right |
| `[` / `]` | Shrink / grow brush radius |
| `M` | Toggle magic-clay displacement (A/B against pure CSG) |
| `Ctrl+Z` | Undo last stroke |
| `Ctrl+Y` or `Ctrl+Shift+Z` | Redo |
| `Esc` | Quit |

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
