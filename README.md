# buoyant-esp32p4

A hardware-accelerated [Buoyant](https://github.com/riley-williams/buoyant) render target for the **ESP32-P4**, dispatching to the chip's PPA (Pixel Processing Accelerator) for the operations it can do — solid rectangle fills, image blits, alpha blends — and falling back to software for everything else.

## Why this exists

Buoyant is an ergonomic, SwiftUI-style declarative UI framework for embedded Rust. The shipping render path is `embedded-graphics::DrawTarget`, which is a pixel-level abstraction — it works wonderfully on small SPI panels (320×240, 480×320) but leaves any vendor 2D accelerator unused.

On ESP32-P4 (the chip in the M5Stack Tab5, Seeed reTerminal D1001, and others) that means the PPA sits idle while the CPU pushes 1280×720 pixels through software. This crate plugs in at Buoyant's `RenderTarget` trait — the right layer — to let the framework dispatch to PPA without giving up the rest of the framework.

The crate is **board-agnostic**: it targets the ESP32-P4 SoC. Per-board panel + touch + power init lives in separate BSP crates (e.g. a future `bsp-tab5`).

## Status

Phase 2 complete: `PpaRenderTarget` is validated end-to-end on real ESP32-P4 silicon (M5Stack Tab5) via `rlvgl-starter/firmware-tab5`, which depends on this crate by path and routes Buoyant rendering through the wrapper. The counter UI renders and accepts touch input identically to the direct-`EmbeddedGraphicsRenderTarget` path. PPA dispatch arrives in subsequent commits (Phase 3+).

See [`ROADMAP.md`](ROADMAP.md) for the phased plan and acceptance criteria per phase.

## Architecture

```
                ┌────────────────────────────────────────┐
                │  ui crate (your declarative views)     │
                └──────────────┬─────────────────────────┘
                               │ Buoyant traits
                               ▼
                ┌────────────────────────────────────────┐
                │  buoyant-esp32p4   (this crate)        │
                │  impl RenderTarget for PpaRenderTarget │
                │   ├─ fill(rect+solid)   → PPA fill     │
                │   ├─ fill(image brush)  → PPA blit     │
                │   ├─ clear()            → PPA fill     │
                │   ├─ with_layer alpha   → PPA blend    │
                │   └─ everything else    → software     │
                └──────────────┬─────────────────────────┘
                               │ esp-idf-sys FFI
                               ▼
                ┌────────────────────────────────────────┐
                │  ESP-IDF PPA driver, esp_lcd MIPI-DSI  │
                └────────────────────────────────────────┘
```

## Targets

Primary targets validated against:

- **M5Stack Tab5** — 5″ 1280×720 IPS, MIPI-DSI, ESP32-P4
- **Seeed reTerminal D1001** — 8″ 1280×800 IPS, MIPI-DSI, ESP32-P4

Both share the SoC, so this crate is unchanged across them; only the BSP differs.

The crate also builds on other ESP32 chips (S3, C6, etc.) with the `accel-ppa` feature off — the software fallback path is just the existing `embedded-graphics` route. This is useful during initial development on cheaper non-P4 boards.

## License

MIT. See [`LICENSE`](LICENSE).
