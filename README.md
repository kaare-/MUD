# MUD

A small text adventure built with [Bevy](https://bevyengine.org/) 0.15.

## Requirements

Linux system libraries for winit/wgpu (already installed in the Cloud Agent environment):

- `libxkbcommon-dev` `libxkbcommon-x11-0` `libwayland-dev`
- `libasound2-dev` `libudev-dev`
- `libxcursor-dev` `libxi-dev` `libxrandr-dev` `libgl1-mesa-dev`
- For headless runs: `xvfb` `mesa-vulkan-drivers`

Rust **stable** (1.85+, edition2024-capable) is required.

## Run

```bash
cargo run
```

Headless demo (scripted commands, then exit):

```bash
xvfb-run -a env WGPU_BACKEND=vulkan cargo run -- --demo
```

## Play

Type commands in the window:

| Command | Action |
| --- | --- |
| `look` / `l` | Describe the current room |
| `go <dir>` or `n`/`s`/`e`/`w` | Move |
| `help` | List commands |
| `quit` | Leave the keep |

Starting room: **Entrance Hall**, with exits to the library, garden, and cellar.
