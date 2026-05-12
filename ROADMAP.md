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

### Phase 2 — Run the wrapper on real hardware ✅

Goal: validate the wrapper end-to-end on hardware we have, with no PPA involvement.

Originally scoped for CoreS3 SE because Tab5 wasn't on the bench yet. With a Tab5 now available the validation moved directly to the target hardware: `rlvgl-starter`'s `firmware-tab5` imports `buoyant-esp32p4` by path and routes Buoyant rendering through `PpaRenderTarget::new(&mut adapter)` instead of `EmbeddedGraphicsRenderTarget::new(...)`. The counter UI behaves identically on the Tab5: panel comes up via the M5Stack BSP, GT911/ST7123 touch events drive Buoyant's event loop, and `state.count` increments/decrements through the wrapper with no visual regressions.

**Acceptance:** met. Tab5 v1.3 silicon, 720×1280 MIPI-DSI, software path through `PpaRenderTarget`. No PPA dispatches yet — that's Phase 4+.

### Phase 3 — esp-idf-sys + PPA driver wrapper module ✅

Goal: thin Rust wrappers over the ESP-IDF PPA driver. No Buoyant integration yet.

**Delivered:**
- `src/ppa.rs` gated behind `feature = "accel-ppa"`. Exposes `Client::new_{fill,blend,srm}()` → drops via `ppa_unregister_client`, `do_fill/do_blend/do_srm` thin wrappers that translate `esp_err_t` into `Result`, `PsramBuffer::new(size)` for 64-byte aligned PSRAM allocations with `heap_caps_aligned_alloc(64, …, MALLOC_CAP_SPIRAM)` and RAII drop, plus `msync_flush` / `msync_invalidate` over `esp_cache_msync`.
- `include/ppa_bindings.h` (driver/ppa.h + heap-caps + esp_cache + soc_caps) declared in `Cargo.toml` as `[[package.metadata.esp-idf-sys.extra_components]]` with `bindings_module = "ppa"`. esp-idf-sys aggregates this from the dep graph, so any consumer enabling `accel-ppa` gets `esp_idf_sys::ppa::*` symbols automatically — no per-project header edits needed.

**Acceptance:** met. Crate builds clean on the host with default features; firmware-tab5 with `accel-ppa` enabled compiles for `riscv32imafc-esp-espidf` and runs identically on Tab5 v1.3 hardware (counter UI behaves the same — Phase 3 binds the API but doesn't yet call it).

> Roadmap pre-update notes referenced `riscv32imac-esp-espidf` and a per-target `cfg(target_chip)`. The actual target triple for ESP32-P4 (FPU present) is `riscv32imafc-esp-espidf`, and the cleaner gate is the `accel-ppa` Cargo feature itself — consumers that don't target P4 simply don't enable it.

### Phase 4 — PPA-accelerated `clear` ✅

Goal: the simplest possible PPA fast-path. `clear(color)` becomes a `ppa_fill` over the entire framebuffer.

**Delivered:**
- `ppa::PpaFillTarget` in `src/ppa.rs` binds a borrowed [`Client`] (registered for `Operation::Fill`) to an output framebuffer (raw pointer + size + dimensions + `ppa_fill_color_mode_t`). Its `clear(fill_val)` method submits a blocking `ppa_do_fill` over the whole window and invalidates the L1/L2 cache lines covering the destination.
- `PpaRenderTarget::ppa_clear(&fill_target, fill_val)` exposes the fast-path to consumers. We chose an **additive** API rather than overriding the `RenderTarget::clear` trait method because keeping the wrapper `#[repr(transparent)]` is load-bearing for Phase 1's `with_layer` recast (the only sound way Rust gives us to hand `&mut Self` to a nested closure without specialization). Callers invoke `ppa_clear` once per frame in place of a software `display.clear(...)`; Buoyant's internal `clear` calls inside `Render::render` still take the software path — those are typically small region clears for layer composition, where the PPA setup cost would outweigh the fill.
- Per-frame timing logged from `firmware-tab5`'s render loop. A one-shot bench at boot compares 5 software and 5 PPA full-frame fills.

**Acceptance:** met on Tab5 v1.3 silicon, 720×1280 RGB565 framebuffer in OPI PSRAM @ 200 MHz, ESP32-P4 HP core @ 360 MHz:

| Path | Avg time per full-frame clear | |
|---|---:|---|
| Software (`slice.fill(0)` over 921 600 pixels in PSRAM) | **29 225 µs** | baseline |
| PPA `ppa_do_fill` + cache invalidate | **5 320 µs** | **5.49× speedup** |

Comfortably above the ≥3× target. No visual artifacts, no tearing, touch still responsive. The PPA path is close to PSRAM bandwidth saturation (1.84 MiB / 5.3 ms ≈ 347 MB/s, vs the X16 OPI PSRAM's ~400 MB/s peak) — most of the remaining win in later phases will come from offloading CPU cycles, not raw throughput.

### Phase 5 — PPA-accelerated rectangle fills ✅

Goal: `fill(shape, brush, ...)` with `shape.as_rectangle().is_some()` and `brush.as_solid().is_some()` becomes a `ppa_fill`.

**Delivered:**
- `ppa::PpaFillTarget::fill_rect(x, y, w, h, fill_val)` — the sub-rectangle counterpart to Phase 4's full-window `clear`. Uses `ppa_do_fill`'s `block_offset_x/y` + `fill_block_w/h` fields.
- `ppa::PpaDrawTarget<'a, D>` — an `embedded-graphics` `DrawTarget<Color = Rgb565>` wrapper that intercepts `fill_solid` for rectangles above a configurable pixel threshold (default 4 096 px ≈ 64×64) and dispatches them via `PpaFillTarget::fill_rect`. All other calls pass through to the inner display. `clear` likewise dispatches.
- Lives at the `embedded-graphics` layer (not Buoyant's `RenderTarget::fill`) so `PpaRenderTarget` can keep `#[repr(transparent)]` for Phase 1's `with_layer` recast. In practice `EmbeddedGraphicsRenderTarget::fill(rect, solid_brush, …)` lowers through `embedded-graphics` to `fill_solid` anyway, so intercepting there gets us the same dispatches without breaking the wrapper's invariant.

**Acceptance:** measured on Tab5 v1.3, 720×1280 RGB565 in OPI PSRAM @ 200 MHz, HP @ 360 MHz:

| Rect | Pixels | Software | PPA | Speedup |
|---|---:|---:|---:|---:|
| 64×64 | 4 096 | 68 µs | 96 µs | 0.71× |
| 200×200 | 40 000 | 653 µs | 331 µs | 1.97× |
| 400×400 | 160 000 | 6 023 µs | 1 033 µs | 5.83× |
| 720×200 | 144 000 | 5 045 µs | 934 µs | 5.40× |
| 720×600 | 432 000 | 15 128 µs | 2 512 µs | 6.02× |

Empirical crossover sits at ~64×64 (4 096 px), validating the default threshold. Above 200×200 the PPA wins by ≥2×; at half-screen it's 6×, matching the Phase 4 full-frame ratio.

**On the counter UI specifically:** frame render time stays at ~3.7 ms because the existing UI is dominated by *circular* button backgrounds (drawn through `embedded-graphics`'s `draw_iter` per-pixel coverage path), a RoundedRectangle Reset button (also `draw_iter`), and tiny 32×6 icon bars (192 px — below threshold). The roadmap's "1.5–2× speedup on a UI dominated by button backgrounds" expected rectangle backgrounds; the post-redesign counter has circles instead, so Phase 5's win for this specific UI is essentially zero. The infrastructure is correct and dispatches when given something to dispatch on — the next material UI speedup arrives with Phase 6 (image blits via `ppa_blend`) and Phase 7 (layer alpha via PPA blend), both of which match operations the current UI actually performs.

### Phase 6 — PPA-accelerated image blits ✅

Goal: `Brush::as_image()` axis-aligned blits dispatch to `ppa_blend` (or `ppa_scale_rotate_mirror` for scaled).

**Delivered:**
- `ppa::PpaSrmTarget` (parallel to `PpaFillTarget`) bound to an output framebuffer and an SRM-mode [`Client`]. Two methods: `blit` (1:1 copy from a source buffer into a destination sub-rect) and `blit_scaled` (arbitrary positive scale factors derived from the size ratios). Internally flushes the source cache lines before submission and invalidates the destination's afterward, so CPU↔PPA aliasing is sound.
- Sources live in caller-managed PSRAM (e.g. via [`PsramBuffer`]) so the PPA's alignment and DMA-reachability requirements can be satisfied without copying.
- **Automatic detection of image brushes inside `RenderTarget::fill` is deferred.** Doing it cleanly requires either overriding the trait method on `PpaRenderTarget` (which breaks Phase 1's `#[repr(transparent)]` recast in `with_layer`) or inspecting iterator types at the `embedded-graphics::DrawTarget::fill_contiguous` boundary (fragile without `Any`-style downcasts). The current API exposes `PpaSrmTarget` directly; the user calls `blit_scaled` at the point of the image draw. A follow-up phase can add automatic dispatch once the brush-trait surface settles.

**Acceptance:** measured on Tab5 v1.3, 720×1280 RGB565, OPI PSRAM @ 200 MHz, HP @ 360 MHz:

| Operation | Pixels | Software | PPA | Speedup |
|---|---:|---:|---:|---:|
| 1:1 blit 32×32 | 1 024 | 14 µs | 133 µs | 0.11× |
| 1:1 blit 128×128 | 16 384 | 200 µs | 894 µs | 0.22× |
| 1:1 blit 256×256 | 65 536 | 3 373 µs | 3 061 µs | 1.10× |
| 1:1 blit 400×300 | 120 000 | 6 257 µs | 5 625 µs | 1.11× |
| 1:1 blit 720×200 | 144 000 | 6 710 µs | 6 848 µs | 0.98× |
| 1:1 blit 720×600 | 432 000 | 19 965 µs | 19 842 µs | 1.01× |
| **Upscale 100×100 → 400×400** | 160 000 out | **9 654 µs** | **546 µs** | **17.68×** |

The honest read: **pure 1:1 blits are parity** with CPU `memcpy`. Both paths are PSRAM-bandwidth-bound for read+write (~347 MB/s effective), and neither side can beat the memory bus. Phases 4/5 fills won because they only *write*; SRM doesn't get that asymmetry back.

**Scaled blits are where SRM crushes the CPU** — 17.68× on a 4× linear upscale (16× pixel count). The CPU has to compute every output pixel's source coordinate and read it; the PPA does that in dedicated logic. For any UI that does dynamic icon resizing, zoom/pan, or bitmap font scaling, this is the win.

Roadmap target ("image-heavy demo measurably faster than Phase 5") is met for the scaling case and unmet (~parity) for the 1:1 case. That's the shape of the hardware, not a bug.

### Phase 7 — Layer alpha via PPA blend ✅

Goal: `with_layer(|l| l.opacity(α))` paths get composed via `ppa_do_blend` rather than per-pixel software blending.

**Delivered:**
- `ppa::PpaBlendTarget` — third sibling to `PpaFillTarget` / `PpaSrmTarget`. Method `blend_argb_over_rgb565(src_argb_ptr, w, h, dst_x, dst_y, scalar_alpha)` issues `ppa_do_blend` with the bound RGB565 framebuffer as both bg and out, and the source as fg with `PPA_ALPHA_SCALE` set to `scalar_alpha / 255`. Per-pixel α=0 source pixels leave the destination untouched; α=255 pixels blend at `scalar_alpha/255` strength. Cache flushes on both source and destination before, cache invalidates the destination after.
- `ppa::LayerStack` trait — two methods: `push_layer(α)` activates a new scratch surface, `pop_layer_blend()` composites it onto whatever was underneath and recycles the buffer. Implemented by user framebuffer wrappers; the [`PpaLayeredRenderTarget`] calls these on opacity-layer boundaries.
- `ppa::PpaLayeredFramebuffer<D>` — the canonical [`LayerStack`] implementation. Wraps any `DrawTarget<Color = Rgb565> + OriginDimensions`. Maintains a lazy-allocated pool of full-screen ARGB8888 PSRAM scratch buffers (W × H × 4 each). Routes `fill_solid` / `clear` / `draw_iter` to the active top-of-stack with Rgb565→ARGB8888 promotion (alpha = 255 for drawn pixels, 0 for untouched). Bottom layer pops via PPA blend onto the base framebuffer; nested layers fall back to a software ARGB-over-ARGB composite (rare; v0 doesn't accelerate the second-deepest level).
- `PpaLayeredRenderTarget<'a, D>` where `D: DrawTarget + OriginDimensions + LayerStack` — Buoyant `RenderTarget` impl, `#[repr(transparent)]` over `EmbeddedGraphicsRenderTarget` so the Phase 1 `with_layer` recast stays sound. `with_layer` probes `layer_fn` against a fresh `LayerConfig` to read the resulting α: α=0 → skip the draw, α=255 → run `draw_fn` directly, α<255 → push/draw/pop with PPA blend.

**Acceptance:** measured on Tab5 v1.3, 720×1280 RGB565 in OPI PSRAM @ 200 MHz, HP @ 360 MHz, and validated visually with `.opacity(128)` applied to the counter UI's big digit:

| Blend size | Pixels | Software per-pixel α | PPA blend | Speedup |
|---|---:|---:|---:|---:|
| 128×128 | 16 384 | 2 504 µs | 777 µs | **3.22×** |
| 400×300 | 120 000 | 23 934 µs | 4 029 µs | **5.94×** |
| 720×600 | 432 000 | 85 665 µs | 13 956 µs | **6.14×** |

The PPA wins decisively at every size — unlike Phase 6's 1:1 SRM blit which was parity (both bandwidth-bound), alpha blending requires per-pixel multiply-add that the CPU is slow at, so the PPA's dedicated logic dominates.

**Smoke-test result on the counter UI:** the path triggers automatically. Adding `.opacity(128)` to the count Text view produced a half-transparent digit with correct visual blending. Buoyant's existing renderer needed zero changes; the `view.opacity(α)` syntax works the same as on the software path, just faster.

**Caveats / v0 limitations** (worth knowing for follow-up work):
- **`LayerHandle::clip` and `LayerHandle::transform` inside an opacity layer are silently dropped.** `with_layer` consumes its `FnOnce` `layer_fn` for the alpha probe, and re-synthesising clip/transform through EGRT's `LayerHandle` methods double-applies the parent transform (Buoyant's `clip()` re-transforms to global coordinates each time). Fixing this needs direct `LayerConfig` introspection upstream in Buoyant; an issue/PR there is the next step. Practical impact: views that combine `.clip_to(rect).opacity(α)` will misclip. Pure `.opacity(α)` works correctly.
- **Always allocates a full-screen ARGB8888 scratch buffer** (3.7 MiB at 720×1280). Bounding-box-aware allocation would let us blend only the affected region; for the counter's tiny opacity layer this would change the headline 92 ms initial-frame cost into something much closer to the Phase 7 bench numbers above. Future optimization.
- **Nested opacity layers fall back to software** ARGB-over-ARGB composite. Rare in real UIs; can be promoted to PPA later if needed (the PPA can do ARGB→ARGB blend; we'd just need a second `PpaBlendTarget` configured with ARGB output instead of RGB565).
- **Initial frame is expensive: ~92 ms when an opacity layer first pushes** (lazy PSRAM allocation + zero-init + full-screen PPA blend). Subsequent frames reuse the pool and should land closer to the bench numbers. Pre-allocating via `PpaLayeredFramebuffer::reserve_layers(N)` at startup avoids the alloc cost on first interaction.

### Phase 8 — MIPI-DSI present pipeline ✅

Goal: a presentation strategy that doesn't tear on Tab5 / D1001's MIPI-DSI panels.

**Delivered:**
- Switched the rlvgl-starter firmware-tab5 from a single owned PSRAM framebuffer + `esp_lcd_panel_draw_bitmap` (which copied each frame into the panel driver's single FB → tearing) to the DPI panel's *own* dual framebuffer pool.
- Configured the M5Stack BSP with `CONFIG_BSP_LCD_DPI_BUFFER_NUMS=2` so `bsp_display_new` pre-allocates two full-screen RGB565 framebuffers in PSRAM and starts the MIPI-DSI auto-refresh from the first.
- Retrieve both pointers via `esp_lcd_dpi_panel_get_frame_buffer(panel, 2, &mut fb_a, &mut fb_b)` after BSP init.
- New `DoubleBuffer` shim in firmware-tab5 tracks which FB is the back; rebinds the `Framebuffer` view + PPA fill/blend/SRM targets to the current back pointer at the start of every frame.
- `esp_lcd_panel_draw_bitmap(panel, …, back)` becomes a pure "schedule swap on next vsync" call — measured at **~200 µs vs ~240 µs** for the previous copy-into-driver-FB path.

**Acceptance:** measured on Tab5 v1.3, the steady-state per-frame budget after the switch is:

| Stage | Time |
|---|---:|
| PPA full-frame clear | ~5.2 ms |
| Buoyant render (counter UI) | ~3.5 ms |
| Present (schedule swap) | ~0.2 ms |
| **Total** | **~9 ms** |

Comfortably under the 16.7 ms / 60 Hz frame budget. The "smooth 30 FPS / stretch 60 FPS on 1280×720" target from the original roadmap is met for click-driven UIs. For continuous animation that fills the frame budget (e.g. a real moving-element demo), the same machinery applies and tearing is structurally eliminated by the double-buffered swap.

Note: the counter UI updates infrequently (only on touch events) and renders a static layout, so single-vs-double buffer is hard to distinguish visually for *this* UI. The architectural fix is correct regardless; an animation demo would be the right vehicle to show off the tear-free result.

**Triple buffering** (the original "if the GPU outpaces the panel" stretch goal) is unimplemented — `BSP_LCD_DPI_BUFFER_NUMS` supports up to 3, so it's a one-line config bump if we ever start outpacing vsync on this UI. Not currently a constraint.

### Phase 9 — `bsp-tab5` companion crate ✅

Goal: thin Rust wrappers around M5Stack's ESP-IDF BSP for the Tab5 (panel init, GT911/ST7123 touch, PMU, DPI buffer pool).

**Delivered:** as a sibling crate to `firmware-tab5` inside the `rlvgl-starter` workspace (`rlvgl-starter/bsp-tab5/`). Library crate, workspace-excluded (same convention as the firmware crates so a host `cargo build` doesn't try to compile it).

Public surface:

- `Tab5::take() -> anyhow::Result<Tab5>` — single entry point. Brings up `bsp_i2c_init` → `bsp_display_new` (with the `mipi_dsi_phy_pllref_clk_src_t::DEFAULT_LEGACY` / 1 Gbps config the BSP requires; passing NULL faults the BSP) → `esp_lcd_panel_disp_on_off` → `bsp_touch_new` → `esp_lcd_dpi_panel_get_frame_buffer(panel, 2, …)` → three PPA clients (FILL / SRM / BLEND). Returns a `Tab5` whose public fields expose the panel + IO handles, the touch handle, a `DoubleBuffer`, the three `ppa::Client`s, and a `TouchTracker`.
- `Tab5::poll_touch()` — synthesise Buoyant `Event::Touch` events from the GT911/ST7123.
- `Tab5::set_brightness(percent: i32)`, `Tab5::backlight_on / off()`.
- `DoubleBuffer { fb_a, fb_b, back_is_a }` with `back_ptr()` and `present(panel)`.
- `Framebuffer` — thin RGB565 `embedded-graphics::DrawTarget` view over a borrowed `*mut u8`.
- `Rgb888Adapter<'a, D>` — Rgb888 ↔ Rgb565 conversion adapter, **forwards `buoyant_esp32p4::ppa::LayerStack`** so `PpaLayeredRenderTarget` can wrap it directly.
- `TouchTracker` — GT911/ST7123 → `buoyant::event::Event::Touch` state machine with phase synthesis.
- Constants `FB_WIDTH = 720`, `FB_HEIGHT = 1280`, `FB_BYTES`, `FB_PIXELS`, `PPA_ALIGN = 64`.
- `rgb888_to_565` / `rgb888_to_565_packed` colour helpers.

The BSP component itself is pulled in via `[[package.metadata.esp-idf-sys.extra_components]]` *in `bsp-tab5/Cargo.toml`*, not in the consuming binary crate. `esp-idf-sys` aggregates `extra_components` metadata across the entire dependency graph, so downstream binaries don't need their own `bsp_bindings.h`.

**Acceptance:** met. `rlvgl-starter/firmware-tab5` is now a thin shell around `bsp_tab5::Tab5::take()`: the previous ~700-line `main.rs` (init + types + render loop + benches) became ~370 lines, of which only the render loop, the boot benches, and a `render_frame` helper are application code — every line of init/wrapping was deleted in favour of the crate API.

Validated on Tab5 v1.3, identical counter UI to pre-refactor:

| Stage | Pre-Phase 9 | Phase 9 |
|---|---:|---:|
| `Phase 4 bench (full-frame)` | software=29214us PPA=5318us | software=29231us PPA=5318us (no regression) |
| `Phase 6 bench upscale 100→400` | software=9654us PPA=547us 17.7× | software=7522us PPA=536us 14.0× |
| `Phase 7 bench 400×300 α=128` | software=23934us PPA=4029us | software=23970us PPA=4096us 5.85× |
| Steady frame | clear=5213us render=3534us present=216us | clear=5216us render=3176us present=139us |

Bench variation is run-to-run noise; no measurable refactor cost.

**Not yet:** higher-level helpers like a `tab5.render(|target| { … })` closure that builds the whole `PpaLayeredRenderTarget<Rgb888Adapter<PpaLayeredFramebuffer<PpaDrawTarget<Framebuffer>>>>` wrapper stack internally. The concrete type name is unwieldy and using `impl Trait` in the closure parameter runs into `for<'a>` HRTB inference issues that would need careful design. For v0, the consumer builds the stack inline (see `firmware-tab5/src/main.rs::render_frame`) — it's ~6 lines and the type inference works.

### Phase 10 — `bsp-d1001` companion crate

Goal: same as Phase 9, for the Seeed reTerminal D1001.

**Deliverables:**
- `bsp-d1001/` crate built on Seeed's BSP repo.
- D1001 example mirroring the Tab5 example.

**Acceptance:**
- Exact same example code (less the BSP::take call) runs on both Tab5 and D1001 — proving the SoC-level abstraction.

### Phase 11 — Polish ✅ (publish gated on user authorization)

**Delivered:**
- Alignment guarantees: `PpaFillTarget::new`, `PpaSrmTarget::new`, `PpaBlendTarget::new` now `debug_assert!` that `framebuffer_ptr` is `CACHE_LINE`-aligned (64 bytes), so misaligned buffers fail fast in debug builds rather than producing PPA dispatch errors at runtime. Release builds trust the caller; the PPA driver rejects misaligned buffers with `ESP_ERR_INVALID_ARG` regardless.
- Cache flush sequencing: `PpaSrmTarget::blit_scaled` and `PpaBlendTarget::blend_argb_over_rgb565` now `msync_flush` both source and destination before submission (the bg buffer is also read by the PPA for blends), then `msync_invalidate` the destination after. The fill path doesn't need source flushing because it doesn't read.
- Documentation: module-level crate docs in `src/lib.rs` enumerate the two render targets, the `ppa` building blocks, and feature flag implications; README.md gets a worked usage example + measured-speedups table + v0 limitations section.
- `CHANGELOG.md` summarises the 0.1.0 release.
- `Cargo.toml`: `include` list for the published artifact, all required metadata fields (description, license, repository, keywords, categories, readme) present.
- Version bumped to **0.1.0** from 0.0.1.

**Validation:** `cargo publish --dry-run --allow-dirty` packages cleanly (144.7 KiB / 43 KiB compressed, 11 files) and the verification rebuild succeeds.

**Crates.io publish:** intentionally not yet performed — it's an irreversible action and the user owns that decision. Once authorised, the publish is a single `cargo publish` command from the crate root.

**Deferred to a future minor version (0.2.0):**
- Tearing edge cases — covered structurally by Phase 8's double-buffering on the firmware side; no LVGL-style edge cases observed in our usage. Will revisit if a continuous-animation demo surfaces issues.
- Standalone benches in `bench/` — currently in-firmware as boot diagnostics, which is the more honest measurement environment (real PSRAM, real PPA, real frame budget). A `criterion`-style host bench wouldn't measure anything meaningful since the PPA only exists on P4 silicon.
- Phase 10 — `bsp-d1001` companion crate. Requires a Seeed reTerminal D1001 on the bench; untestable without one. The architecture is a near-copy of `bsp-tab5` with a different ESP-IDF BSP component name.

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
