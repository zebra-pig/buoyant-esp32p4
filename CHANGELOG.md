# Changelog

All notable changes to **buoyant-esp32p4** are documented here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the project version is tracked with [SemVer](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — 2026-05-12

First public release. Phases 0–9 of the project [`ROADMAP.md`](ROADMAP.md) complete and validated on M5Stack Tab5 (ESP32-P4 v1.3) hardware.

### Added — render targets

- `PpaRenderTarget<'a, D>` — `#[repr(transparent)]` Buoyant `RenderTarget` wrapper over `EmbeddedGraphicsRenderTarget<DrawTargetSurface<'a, D>>`. Software-only path, suitable for any DrawTarget. Phase 1.
- `PpaLayeredRenderTarget<'a, D>` — same shape as `PpaRenderTarget` but with `D: ppa::LayerStack`. Overrides `with_layer` to dispatch `view.opacity(α)` through an ARGB8888 scratch buffer + PPA `ppa_do_blend` on layer exit. Phase 7.

### Added — `ppa` module (feature: `accel-ppa`)

- `ppa::Client` — RAII handle for one of `Operation::{Fill, Blend, Srm}`; unregisters on drop.
- `ppa::PpaFillTarget` — bound RGB565/RGB888/ARGB8888 destination + fill client. Methods `clear(fill_val)` (full window) and `fill_rect(x, y, w, h, fill_val)`. Phase 4 / 5.
- `ppa::PpaSrmTarget` — bound destination + SRM client. Methods `blit(src, src_w, src_h, dst_x, dst_y)` (1:1) and `blit_scaled(src, src_w, src_h, dst_x, dst_y, dst_w, dst_h)`. Phase 6.
- `ppa::PpaBlendTarget` — bound RGB565 destination + blend client. Method `blend_argb_over_rgb565(src_argb, w, h, dst_x, dst_y, scalar_alpha)`. Phase 7.
- `ppa::PpaDrawTarget<'a, D>` — embedded-graphics `DrawTarget<Color = Rgb565>` wrapper that intercepts `fill_solid` for rectangles above a configurable pixel threshold (default 4 096 px) and routes them through `PpaFillTarget::fill_rect`. Phase 5.
- `ppa::PpaLayeredFramebuffer<'a, D>` — `LayerStack`-implementing `DrawTarget` wrapper with a lazy-allocated pool of full-screen ARGB8888 PSRAM scratch buffers. Drawing routes to the top-of-stack while a layer is active; popping PPA-blends the layer onto whatever was underneath. Phase 7.
- `ppa::LayerStack` trait — `push_layer(α)` / `pop_layer_blend()`; what `PpaLayeredRenderTarget` calls on its inner DrawTarget.
- `ppa::PsramBuffer` — RAII-freed `heap_caps_aligned_alloc(64, …, MALLOC_CAP_SPIRAM)` allocation for source images.
- `ppa::msync_flush` / `ppa::msync_invalidate` — thin `esp_cache_msync` wrappers for CPU↔PPA cache coherency.

### Added — build integration

- esp-idf-sys `[[package.metadata.esp-idf-sys.extra_components]]` declaration pointing at `include/ppa_bindings.h`. Downstream binary crates that enable `accel-ppa` automatically get `esp_idf_sys::ppa::*` resolved — no per-project header edits.

### Measured performance (Tab5 v1.3, 720×1280, OPI PSRAM 200 MHz, HP 360 MHz)

| Operation | Software | PPA | Speedup |
|---|---:|---:|---:|
| Full-screen clear | 29.2 ms | 5.3 ms | 5.5× |
| 400×400 solid fill | 6.0 ms | 1.0 ms | 5.9× |
| 720×600 solid fill | 15.1 ms | 2.5 ms | 6.0× |
| 1:1 blit 256×256 | 3.4 ms | 3.1 ms | 1.1× |
| Upscale 100→400 | 9.7 ms | 0.55 ms | 17.7× |
| α=128 blend 400×300 | 23.9 ms | 4.0 ms | 5.9× |

### Known limitations

See [`README.md`'s "v0 limitations" section](README.md#v0-limitations). The main one: `LayerHandle::clip` and `LayerHandle::transform` are silently dropped inside opacity layers — pure `.opacity(α)` works correctly, but `.clip_to(rect).opacity(α)` misclips. Fix needs upstream Buoyant exposing direct `LayerConfig` introspection so we can probe alpha without consuming the `FnOnce` `layer_fn`.

[0.1.0]: https://github.com/zebra-pig/buoyant-esp32p4/releases/tag/v0.1.0
