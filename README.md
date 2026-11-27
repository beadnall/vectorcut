# VectorCut

Cut SVG designs on a Silhouette cutter (Cameo, Portrait) from macOS, without Silhouette Studio.

VectorCut reads an SVG, groups its lines by stroke colour, lets you place the design on the mat and
choose a tool and settings for each colour, then drives the machine over USB. It is written in
Rust. The interface is built on the ArtCraft framework, the shared UI layer of the Craft apps
([`storytold/craft-libs`](https://github.com/storytold/craft-libs)): window shell, controls, panels
and themes. The GP-GL encoding of each machine generation comes from that repository's
`craft-plotter` crate.

> **Status: first cut, not yet tried on a machine.** Everything up to the USB write is tested,
> against a simulated Cameo. Make a test cut on scrap before trusting it with good material.

## Layout

| Path | What it is |
|---|---|
| `crates/vectorcut-core` | The engine, no I/O: SVG to polylines in mm (`design`), cut order (`order`), machine table and material presets (`machine`), plan and GP-GL encoding (`job`) |
| `crates/vectorcut-usb` | The machine link: USB discovery and transfers through `nusb`, the paced sender (`Cutter`), a simulated machine |
| `crates/vectorcut-cli` | The `vectorcut` command: `devices`, `models`, `plan`, `cut`, `test-cut`, `status` |
| `app` | The desktop app (`VectorCut`), on the ArtCraft shell |
| `macos`, `scripts` | The bundle's `Info.plist` and `bundle-macos.sh` |
| `samples` | Example designs |

## Build and run

Needs Rust 1.95 or later.

```sh
cargo test                                   # engine and link tests
cargo run -p vectorcut-app                   # the app
cargo run -p vectorcut-app -- --simulate     # the app with a simulated Cameo
scripts/bundle-macos.sh                      # dist/VectorCut.app
```

The command line:

```sh
cargo run -p vectorcut-cli -- plan samples/tag.svg --offset 10,10 --transcript
cargo run -p vectorcut-cli -- cut samples/tag.svg --simulate -y
cargo run -p vectorcut-cli -- devices
cargo run -p vectorcut-cli -- test-cut --preset cardstock
```

## How a design is cut

- **Lines.** Every drawable element is a cut line, stroked or filled. Text has to be converted to
  paths first. Clip paths, transforms, `<use>` and CSS are applied.
- **Groups.** Lines are grouped by stroke colour. A drawing with Inkscape layers and no stroke
  colours is grouped by layer. Each group gets its own tool, holder, force, speed, blade depth,
  passes and overcut, or can be left out.
- **Order.** Pens draw before blades cut. Within a group, holes are cut before the outline round
  them, and the next line is the nearest one, entered at its nearest point.
- **Placement.** The design's page sits at an offset from the mat's top-left corner (drag it on
  the mat or type it). A design that runs off the cutting area is refused.
- **Sending.** The program goes out in pieces of at most 1 KB, each ending on a command, and the
  machine is polled until it is ready after each piece. *Stop* stops between pieces.

## Machines

The model table (`crates/vectorcut-core/src/machine.rs`) lists the Cameo 1 to 5 (and Plus/Pro),
the Cameo Pro MK II and the Portrait 1 to 4, with USB ids, dialect generation, width, tool holders
and setting ranges as the community drivers (inkscape-silhouette) publish them. A machine that is
not listed is still found by its vendor id and identified from its firmware reply.

Platform notes for the USB link:

- **macOS**: no driver should hold the interface, so it opens directly (not yet confirmed on a
  machine). Quit Silhouette Studio first; it keeps the machine open.
- **Linux** (later): needs a udev rule giving your user access to vendor `0b4d`.
- **Windows** (later): the machine is bound to the printer driver, which `nusb` cannot open. It
  needs WinUSB (via Zadig) until a native backend exists.

## Not done yet

- Print and cut (registration marks). `craft-plotter` encodes them; the app does not offer them yet.
- Saving settings between runs, and per-material presets of your own.
- Windows and Linux builds and packaging; an app icon; signing and notarization.
- Checks on real hardware for each model.

## Provenance

The model table (USB ids, widths, setting ranges), the material presets, the endpoint addresses
and the send pacing (1 KB pieces, poll until ready) were taken from reading the source of
inkscape-silhouette (`silhouette/Graphtec.py`, GPL-2.0). Only facts were taken, no code, which
suits a GPL project. But ArtCraft's repositories forbid contributors who have read GPL source
(craft-contrib `standards/app-agents.md` rule 23, `standards/clean-room.md`). This code therefore
cannot be contributed to them as it stands. A contribution would have to be written again, clean,
by someone who has seen neither that source nor this code.

## Licence

GPL-3.0-or-later (see `LICENSE`). The ArtCraft libraries it builds on are MIT OR Apache-2.0.
