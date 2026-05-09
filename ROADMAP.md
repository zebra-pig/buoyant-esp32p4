# buoyant-esp32p4 roadmap

This document is the **persistent plan** for the project. Each phase has explicit deliverables and acceptance criteria so any future session — human or AI — can pick up where the last one stopped without re-deriving the architecture.

## Why this project exists

Buoyant ships with `EmbeddedGraphicsRenderTarget` as its only realistic backend. That backend lowers all Buoyant render operations into `embedded-graphics::DrawTarget` calls, which are pixel-level — by the time pixels are reaching the DrawTarget, the rasterization decisions are made and a 2D accelerator can't intercept them. On chips with hardware blit/blend (PPA on ESP32-P4, DMA2D on STM32, Chrom-ART on i.MX RT, etc.) the accelerator sits idle while the CPU rasterizes.

The fix isn't in `embedded-graphics` — that crate is intentionally a thin pixel abstraction. The fix is at Buoyant's `RenderTarget` trait, which already exposes high-level operations (`fill(shape, brush, transform)`, `with_layer(...)`, `draw_glyphs(...)`, `clear(color)`). Implementing that trait directly — not via embedded-graphics — gives the accelerator something to dispatch on.

This crate is the ESP32-P4 implementation. Other vendor backends could follow the same pattern in their own crates.

## Architectural decisions

These were settled during initial scoping; future sessions should preserve them unless explicitly revisited.

1. **Separate crate, not a fork of buoyant.** Buoyant exposes `pub trait RenderTarget`; we implement it downstream. We only fork or PR upstream if a method signature blocks us.
2. **Software fallback path is a feature, not a bug.** With `accel-ppa` off the crate is a board-portable software renderer that works on any ESP32 chip. This lets us develop and test on a CoreS3 SE (ESP32-S3, no PPA) before P4 hardware is on the bench.
3. **Board-agnostic.** This crate is *SoC-specific* (ESP32-P4) but *board-independent*. Panel init, touch, and power management live in per-board BSP crates (`bsp-tab5`, `bsp-d1001`, …). Both M5Stack and Seeed publish ESP-IDF BSPs in C; the BSP crates are thin Rust wrappers, not new bring-up.
4. **Glyph rendering stays software.** The PPA can't rasterize bezier curves; LVGL doesn't try either. Text dominates render-tree complexity but not pixel volume; software is the right call.
5. **Tab5 is the development board.** Cheaper, more documentation, has a tripod mount and swappable battery. The D1001 is a porting target for Phase 7+, not the iteration vehicle.
6. **Framebuffer in PSRAM, 64-byte aligned.** P4 has 32 MB OPI PSRAM; a 1280×800 RGB565 framebuffer is ~2 MB, way over the 768 KB internal SRAM budget. PPA requires 64-byte (cache-line) alignment, so allocations go through `heap_caps_aligned_alloc(64, size, MALLOC_CAP_SPIRAM)`.
7. **Don't over-design v0.** Each phase ships a small, demonstrable improvement. We measure frame time at each phase boundary.

## Phases

### Phase 0 — Scaffold ✅

Goal: a buildable empty crate so subsequent phases have a place to land.

**Done when:**
- `Cargo.toml`, `LICENSE`, `README.md`, `ROADMAP.md`, `.gitignore`, `src/lib.rs` exist.
- `cargo check` resolves dependencies without network surprises.
- Initial commit on `main` branch.

### Phase 1 — Trait skeleton with software fallback ✅

Goal: a working `RenderTarget` impl that proves the wiring without any acceleration.

**Deliverables:**
- `pub struct PpaRenderTarget<'a, D: DrawTarget<Color = Rgb565> + OriginDimensions>` wrapping a `DrawTargetSurface<'a, D>`.
- `impl RenderTarget for PpaRenderTarget<...>` with every method implemented by delegating to a private `EmbeddedGraphicsRenderTarget` held inside, OR by calling through `raw_surface()` to the inner DrawTarget.
- Public constructor `PpaRenderTarget::new(display: &mut D)`.
- `cargo check` clean. `cargo build` clean (no esp-idf required at this stage; the wrapper compiles for the host).

**Acceptance:**
- A small integration test or doc test creates a `PpaRenderTarget` over a `MockDisplay` from embedded-graphics and renders a Rectangle. Output identical to going through `EmbeddedGraphicsRenderTarget` directly.

**Why this phase exists:** lock the trait wiring before adding hardware. If Buoyant's trait is too restrictive for what we need, we discover it here, not after writing PPA glue.

### Phase 2 — Run the wrapper on a real ESP32-S3 (CoreS3 SE)

Goal: validate the wrapper end-to-end on hardware we already have, with no PPA involvement.

**Deliverables:**
- Example `examples/cores3-counter/` (a small bin crate or path-style test rig) that re-uses the existing `rlvgl-starter`'s `ui` crate but routes rendering through `PpaRenderTarget` instead of `EmbeddedGraphicsRenderTarget`.
- Documented in README that this is for development convenience, not production.

**Acceptance:**
- Counter UI flashes onto the CoreS3 SE and renders identically to the rlvgl-starter baseline. Touch still works. No new visual regressions.

### Phase 3 — esp-idf-sys + PPA driver wrapper module

Goal: thin Rust wrappers over the ESP-IDF PPA driver. No Buoyant integration yet.

**Deliverables:**
- New `src/ppa.rs` module (gated behind `feature = "accel-ppa"`).
- Wrappers for: `ppa_register_client`, `ppa_unregister_client`, `ppa_do_fill`, `ppa_do_blend`, `ppa_do_scale_rotate_mirror` (those are the IDF call names; verify against current esp-idf-sys bindings before relying on them).
- Helper to allocate 64-byte-aligned framebuffers via `heap_caps_aligned_alloc(64, size, MALLOC_CAP_SPIRAM)`.
- Cache flush helpers (`esp_cache_msync`) for buffers handed to the PPA.

**Acceptance:**
- The `ppa` module compiles when targeting `xtensa-esp32s3-espidf` even though those targets don't have PPA — symbols may be missing; `cfg(target_chip = "esp32p4")` gates keep S3 builds clean.
- Builds cleanly when targeting `riscv32imac-esp-espidf` (the P4 target triple).

### Phase 4 — PPA-accelerated `clear`

Goal: the simplest possible PPA fast-path. `clear(color)` becomes a `ppa_fill` over the entire framebuffer.

**Deliverables:**
- `clear` impl checks `cfg(feature = "accel-ppa")` and dispatches to PPA fill; otherwise falls back to surface fill.
- A simple frame-time logger so we can measure before/after.

**Acceptance:**
- On real Tab5 hardware, `clear` time drops measurably vs. Phase 2 baseline. Target: at least 3× speedup for full-screen fill at 1280×720.
- No visual artifacts. No tearing on a single redraw.

### Phase 5 — PPA-accelerated rectangle fills

Goal: `fill(shape, brush, ...)` with `shape.as_rectangle().is_some()` and `brush.as_solid().is_some()` becomes a `ppa_fill`.

**Deliverables:**
- Pattern-match the shape and brush; dispatch to PPA on the fast-path; fall through to `raw_surface()` + `embedded-graphics` for everything else.
- Verify with the counter UI (which has solid rects in the +/- buttons after the design redo).

**Acceptance:**
- Frame time on the Tab5 counter UI drops measurably vs. Phase 4. Target: 1.5–2× speedup on a UI dominated by button backgrounds.

### Phase 6 — PPA-accelerated image blits

Goal: `Brush::as_image()` axis-aligned blits dispatch to `ppa_blend` (or `ppa_scale_rotate_mirror` for scaled).

**Deliverables:**
- Detection of image brushes in `fill`.
- Dispatch to scaled-blit when brush size != shape size.
- Fall back to software blit when source isn't in PSRAM or isn't aligned.

**Acceptance:**
- An image-heavy demo (sprites, icons) on Tab5 measurably faster than Phase 5.

### Phase 7 — Layer alpha via PPA blend

Goal: `with_layer(|l| l.opacity(alpha))` paths get composed via `ppa_blend` rather than per-pixel software blending.

**Deliverables:**
- Layer-stack management that allocates a temp PSRAM buffer for the layer's contents and composes via PPA blend on `with_layer` exit.
- Memory pressure check: don't over-allocate; reuse buffers across layers.

**Acceptance:**
- Smooth fade transitions on a modal-overlay demo at 1280×720 without the CPU spiking.

### Phase 8 — MIPI-DSI present pipeline

Goal: a presentation strategy that doesn't tear on Tab5 / D1001's MIPI-DSI panels.

**Deliverables:**
- Double-buffered front/back framebuffers in PSRAM.
- Buffer flip on vsync via `esp_lcd` panel APIs.
- Optional triple-buffering if the GPU outpaces the panel.

**Acceptance:**
- No tearing on a moving-element demo. Sustained 30 FPS minimum on 1280×720, 60 FPS as stretch goal.

### Phase 9 — `bsp-tab5` companion crate

Goal: thin Rust wrappers around M5Stack's ESP-IDF BSP for the Tab5 (panel init, GT911 touch, AXP power).

**Deliverables:**
- `bsp-tab5/` crate (sibling, separate `Cargo.toml`, possibly a workspace).
- `Tab5::take()` returns a tuple of (`PpaRenderTarget`, touch tracker, …) ready for use.

**Acceptance:**
- A Tab5 example boots, runs the counter UI, accepts touch input, no panel init code in the example.

### Phase 10 — `bsp-d1001` companion crate

Goal: same as Phase 9, for the Seeed reTerminal D1001.

**Deliverables:**
- `bsp-d1001/` crate built on Seeed's BSP repo.
- D1001 example mirroring the Tab5 example.

**Acceptance:**
- Exact same example code (less the BSP::take call) runs on both Tab5 and D1001 — proving the SoC-level abstraction.

### Phase 11 — Polish

- Alignment guarantees enforced by `Framebuffer` constructor.
- Cache flush sequencing verified against the IDF docs.
- Tearing edge cases (cf. lvgl/lvgl#9046 for known PPA pitfalls).
- Benchmarks in `bench/`.
- Crates.io publish: `0.1.0` once Phase 4–8 are landed.

## Performance targets

Frame time goals at each phase boundary, on Tab5 (1280×720) running a UI of similar complexity to the rlvgl-starter counter demo:

| Phase | Target frame time | Notes |
|-------|------------------:|-------|
| 2 (sw fallback) | < 200 ms | Acceptable for click-driven UI; baseline. |
| 4 (PPA clear) | < 120 ms | Clear was a big chunk. |
| 5 (PPA rect fill) | < 80 ms | Buttons get fast. |
| 6 (PPA blit) | < 50 ms | Icons/sprites cheap. |
| 7 (PPA blend layer) | < 35 ms | Composition cheap. |
| 8 (vsync present) | 33 ms = 30 FPS | Panel-rate sustained. |

For comparison: LVGL+PPA on the same hardware runs at roughly 60 FPS on similar UIs but with retained-mode widget caching that we don't have. Realistic ceiling for this crate is 60–80% of LVGL's frame-time efficiency; we accept that gap in exchange for the ergonomic Buoyant API.

## What's *out* of scope (for now)

- Vector graphics / SVG / arbitrary path rendering. PPA can't help; Buoyant doesn't have it.
- Retained-mode widget caching. That'd be an upstream Buoyant change.
- Dirty-rect / partial redraw. Same — upstream issue (riley-williams/buoyant#133).
- Wider chip support (STM32 DMA2D, NXP NemaGFX, etc.). Other vendors get their own crates following this pattern.

## Cross-session continuity checklist

When picking this up in a future session:

1. Read this `ROADMAP.md` first.
2. Check `git log` to see which phase is most recently shipped.
3. Read the most recent commit messages — they should reference the phase they completed.
4. Re-read the "Architectural decisions" section above before changing anything.
5. If a phase's acceptance criteria aren't met, complete that phase before starting the next.
6. If hardware iteration is needed and isn't available, mark the work in a `phase-N-stub` branch and pause.

## Related projects

- Upstream framework: <https://github.com/riley-williams/buoyant>
- Sibling project (current iteration vehicle): `~/GitHub/rlvgl-starter` (CoreS3 SE firmware running Buoyant)
- ESP-IDF PPA docs: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32p4/api-reference/peripherals/ppa.html>
- M5Stack Tab5 BSP: <https://components.espressif.com/components/espressif/m5stack_tab5>
- Seeed reTerminal D1001 BSP: <https://github.com/Seeed-Studio/reTerminal-D1001>
