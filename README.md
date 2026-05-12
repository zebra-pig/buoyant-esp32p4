# buoyant-esp32p4

A hardware-accelerated [Buoyant](https://github.com/riley-williams/buoyant) render target for the **ESP32-P4**, dispatching to the chip's PPA (Pixel Processing Accelerator) for solid rectangle fills, image blits, and alpha blends — and falling back to software for everything else.

## Why this exists

Buoyant is an ergonomic, SwiftUI-style declarative UI framework for embedded Rust. The shipping render path is `embedded-graphics::DrawTarget`, a pixel-level abstraction — it works wonderfully on small SPI panels (320×240, 480×320) but leaves any vendor 2D accelerator unused.

On ESP32-P4 (the chip in the M5Stack Tab5, Seeed reTerminal D1001, and others) that means the PPA sits idle while the CPU pushes 1280×720 pixels through software. This crate plugs in at Buoyant's `RenderTarget` trait to let the framework dispatch to PPA without giving up the rest of the framework.

The crate is **board-agnostic**: it targets the ESP32-P4 SoC. Per-board panel + touch + power init lives in separate BSP crates (e.g. `bsp-tab5`).

## Status

Phases 0–9 of the roadmap have shipped. The crate provides:

- `PpaRenderTarget` — Buoyant `RenderTarget` wrapper, software-only path
- `PpaLayeredRenderTarget` — same but with hardware-accelerated `view.opacity(α)` via the PPA blend engine
- `ppa::PpaFillTarget` — full-screen and sub-rect solid fills (`ppa_do_fill`)
- `ppa::PpaSrmTarget` — 1:1 and scaled image blits (`ppa_do_scale_rotate_mirror`)
- `ppa::PpaBlendTarget` — alpha composition (`ppa_do_blend`)
- `ppa::PpaDrawTarget` — embedded-graphics adapter that intercepts `fill_solid` above a configurable size threshold
- `ppa::PpaLayeredFramebuffer` — DrawTarget with an internal ARGB8888 scratch-buffer stack for `LayerStack`-driven opacity composition
- `ppa::Client`, `ppa::PsramBuffer`, `ppa::msync_flush` / `msync_invalidate` building blocks

See [`ROADMAP.md`](ROADMAP.md) for the phased plan and per-phase acceptance criteria with measurements.

## Measured speedups (Tab5 v1.3, 720×1280, OPI PSRAM 200 MHz, HP 360 MHz)

| Operation | Software | PPA | Speedup |
|---|---:|---:|---:|
| Full-screen clear | 29.2 ms | 5.3 ms | **5.5×** |
| 400×400 solid rect fill | 6.0 ms | 1.0 ms | **5.9×** |
| 720×600 solid rect fill | 15.1 ms | 2.5 ms | **6.0×** |
| 1:1 blit 256×256 | 3.4 ms | 3.1 ms | 1.1× (PSRAM bandwidth-bound) |
| Upscale 100→400 | 9.7 ms | 0.55 ms | **17.7×** |
| α=128 blend 400×300 | 23.9 ms | 4.0 ms | **5.9×** |

## Usage

The most common shape: use `bsp-tab5` for the board bring-up, this crate for the render target.

```rust
use bsp_tab5::{Tab5, Framebuffer, Rgb888Adapter, FB_BYTES, FB_WIDTH, FB_HEIGHT, rgb888_to_565_packed};
use buoyant_esp32p4::{ppa, PpaLayeredRenderTarget};
use buoyant::render::Render;
use embedded_graphics::pixelcolor::Rgb888;

let mut tab5 = Tab5::take()?;

// ... build your Buoyant view tree ...

loop {
    if let Some(event) = tab5.poll_touch() { /* dispatch */ }
    if redraw {
        let back = tab5.double_buffer.back_ptr();
        let fill_target = unsafe {
            ppa::PpaFillTarget::new(
                &tab5.fill_client, back, FB_BYTES,
                FB_WIDTH as u32, FB_HEIGHT as u32,
                ppa::ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_RGB565,
            )
        };
        let blend_target = unsafe {
            ppa::PpaBlendTarget::new(
                &tab5.blend_client, back, FB_BYTES,
                FB_WIDTH as u32, FB_HEIGHT as u32,
            )
        };
        fill_target.clear(rgb888_to_565_packed(Rgb888::new(0, 0, 0)) as u32)?;
        {
            let mut fb = unsafe { Framebuffer::from_raw(back) };
            let mut pfb = ppa::PpaDrawTarget::new(&mut fb, &fill_target);
            let mut lfb = ppa::PpaLayeredFramebuffer::new(&mut pfb, &blend_target);
            let mut adapter = Rgb888Adapter::new(&mut lfb);
            let mut target = PpaLayeredRenderTarget::new(&mut adapter);
            Render::render(&tree, &mut target, &Rgb888::new(0xFF, 0xFF, 0xFF));
        }
        tab5.double_buffer.present(tab5.panel)?;
    }
}
```

Your Buoyant view code uses standard idioms — `.opacity(α)`, `Rectangle::new(...).fill(...)`, etc. The acceleration is transparent.

## Architecture

```
                ┌────────────────────────────────────────┐
                │  ui crate (your declarative views)     │
                └──────────────┬─────────────────────────┘
                               │ Buoyant traits
                               ▼
                ┌────────────────────────────────────────┐
                │  buoyant-esp32p4   (this crate)        │
                │  impl RenderTarget for                 │
                │    PpaLayeredRenderTarget              │
                │   ├─ clear()            → PpaFillTarget│
                │   ├─ fill_solid()       → PpaDrawTarget│
                │   │                       → PpaFillTarget│
                │   ├─ with_layer(opacity)→ PpaLayered- │
                │   │                       Framebuffer  │
                │   │                       → PpaBlendTarget│
                │   ├─ (image blit, scale) → PpaSrmTarget│
                │   └─ everything else    → software     │
                └──────────────┬─────────────────────────┘
                               │ esp-idf-sys FFI (driver/ppa.h)
                               ▼
                ┌────────────────────────────────────────┐
                │  ESP-IDF PPA driver, esp_lcd MIPI-DSI  │
                └────────────────────────────────────────┘
```

## Features

- `default` (off): host-buildable, no esp-idf-sys, no PPA — just the `PpaRenderTarget` trait skeleton over `EmbeddedGraphicsRenderTarget`.
- `std`: enables Buoyant's `std` features; required when targeting esp-idf.
- `esp-idf`: pulls in `esp-idf-sys`. Implies `std`.
- `accel-ppa`: enables the PPA fast-paths and the `ppa` module. Implies `esp-idf`. **Requires** ESP-IDF v5.5+ and the `riscv32imafc-esp-espidf` target (ESP32-P4). On other ESP32 chips the PPA peripheral doesn't exist and the symbols will fail to link.

## v0 limitations

The crate is at 0.1.0 — usable, with the following acknowledged constraints:

- **`with_layer` clip and transform are silently dropped inside opacity layers.** `LayerHandle::clip(rect).opacity(α)` will misclip. Fix needs upstream Buoyant exposing direct `LayerConfig` introspection so we can probe alpha without consuming the `FnOnce` layer_fn. Pure `.opacity(α)` works correctly.
- **`PpaLayeredFramebuffer` always allocates a full-screen ARGB8888 scratch buffer** (3.7 MiB on 720×1280) regardless of the layer's bounding box. Bounding-box-aware allocation is a future optimisation.
- **First opacity-layer push costs ~92 ms** on a 720×1280 panel: PSRAM scratch alloc + zero-init + full-screen PPA blend. Use `PpaLayeredFramebuffer::reserve_layers(N)` at startup to amortise.
- **Nested opacity layers** software-composite instead of PPA-blending. The PPA could do ARGB→ARGB blend but our `PpaBlendTarget` is configured for RGB565 output; nested case is rare in practice.
- **1:1 SRM blits are at parity** with CPU `memcpy` (both PSRAM-bandwidth-bound). PPA SRM wins decisively only when scaling/rotating. For straight copies use whichever is more ergonomic.
- **Only RGB565 panels supported** at the integration level. The lower-level `ppa::PpaFillTarget` etc. accept any `ppa_*_color_mode_t`, but `PpaLayeredFramebuffer` and the `Rgb888Adapter` chain assume an RGB565 destination.

## Targets

Primary targets validated against:

- **M5Stack Tab5** — 5″ 720×1280 IPS portrait, MIPI-DSI, ESP32-P4
- **Seeed reTerminal D1001** — 8″ 1280×800 IPS, MIPI-DSI, ESP32-P4 (per the SoC architecture; a `bsp-d1001` crate is on the roadmap)

Both share the SoC, so this crate is unchanged across them; only the BSP differs.

The crate also builds on other ESP32 chips (S3, C6, etc.) with the `accel-ppa` feature off — the software fallback path is just the existing `embedded-graphics` route. This is useful during initial development on cheaper non-P4 boards.

## License

MIT. See [`LICENSE`](LICENSE).
