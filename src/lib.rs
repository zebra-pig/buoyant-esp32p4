//! Hardware-accelerated Buoyant render target for ESP32-P4.
//!
//! This crate plugs into Buoyant by implementing
//! [`buoyant::render_target::RenderTarget`]. Future phases dispatch
//! qualifying operations to the ESP32-P4 PPA (Pixel Processing
//! Accelerator); the current phase delegates everything to
//! [`EmbeddedGraphicsRenderTarget`] so the trait wiring is verifiable on
//! any chip while the PPA path is built up. See `ROADMAP.md` for the
//! phased plan.

#![cfg_attr(not(feature = "std"), no_std)]

/// Thin Rust wrappers over the ESP-IDF PPA driver, PSRAM-aligned heap
/// allocation, and cache-flush helpers. Only available when the
/// `accel-ppa` feature is enabled (which implies `esp-idf`). The Buoyant
/// trait methods on [`PpaRenderTarget`] start dispatching through these
/// in Phase 4 of the roadmap.
#[cfg(feature = "accel-ppa")]
pub mod ppa;

use buoyant::color::AlphaColor;
use buoyant::font::FontRender;
use buoyant::primitives::geometry::{Rectangle, Shape};
use buoyant::primitives::transform::LinearTransform;
use buoyant::primitives::{Interpolate, Point, Size};
use buoyant::render_target::surface::DrawTargetSurface;
use buoyant::render_target::{
    Brush, EmbeddedGraphicsRenderTarget, Glyph, LayerHandle, RenderTarget, Stroke, Surface,
};
#[cfg(feature = "accel-ppa")]
use buoyant::render_target::LayerConfig;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::OriginDimensions;
use embedded_graphics::pixelcolor::PixelColor;

/// Buoyant render target that will dispatch qualifying operations to the
/// ESP32-P4 PPA. In Phase 1 this is a transparent wrapper over
/// [`EmbeddedGraphicsRenderTarget`]; later phases override individual
/// methods (`clear`, `fill` on rectangles, image-brush blits, layer
/// blends) to call into the PPA driver, keeping the rest on the software
/// path.
///
/// The wrapper is `repr(transparent)` over its inner field so
/// [`RenderTarget::with_layer`] can recast its `&mut inner` callback
/// argument back to a `&mut Self` for the user's draw closure — this is
/// the standard newtype-delegation pattern. The cast is sound because
/// the layout is guaranteed identical to the inner type.
#[repr(transparent)]
pub struct PpaRenderTarget<'a, D>
where
    D: DrawTarget + OriginDimensions,
    D::Color: PixelColor + Interpolate + AlphaColor,
{
    inner: EmbeddedGraphicsRenderTarget<DrawTargetSurface<'a, D>>,
}

impl<'a, D> PpaRenderTarget<'a, D>
where
    D: DrawTarget + OriginDimensions,
    D::Color: PixelColor + Interpolate + AlphaColor,
{
    /// Wrap an `embedded-graphics` display so Buoyant can render through it.
    pub fn new(display: &'a mut D) -> Self {
        Self {
            inner: EmbeddedGraphicsRenderTarget::new(display),
        }
    }

    /// Like [`Self::new`], but seeds the root layer with a background-color
    /// hint. The hint enables glyph antialiasing in Buoyant's font renderer
    /// (otherwise text falls through to a 1-bit coverage threshold).
    pub fn new_hinted(display: &'a mut D, background_hint: D::Color) -> Self {
        Self {
            inner: EmbeddedGraphicsRenderTarget::new_hinted(display, background_hint),
        }
    }

    /// Borrow the underlying display, e.g. to flip a back buffer.
    pub fn display(&self) -> &D {
        self.inner.display()
    }

    /// Mutable counterpart of [`Self::display`].
    pub fn display_mut(&mut self) -> &mut D {
        self.inner.display_mut()
    }

    /// Phase-4 fast-path: clear the whole framebuffer via the PPA fill
    /// engine instead of looping per-pixel through `embedded-graphics`.
    /// Call this once per frame before invoking
    /// `buoyant::render::Render::render`, in place of a manual
    /// `display.clear(...)` on the underlying [`DrawTarget`].
    ///
    /// `fill_val` is the raw word the buffer's pixel format expects —
    /// e.g. a packed Rgb565 in the low 16 bits when [`fill_target`] was
    /// configured with [`ppa::ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_RGB565`].
    /// Returns `Err` if PPA dispatch fails; callers should fall back to
    /// the software clear via [`buoyant::render_target::RenderTarget::clear`]
    /// when that happens.
    ///
    /// Buoyant's internal `clear` calls inside `Render::render` still go
    /// through the software path; those tend to be small region clears
    /// for layer composition, where the PPA setup cost can outweigh the
    /// fill itself. Phase 5+ adds inline PPA dispatch for the broader
    /// shape/brush fast-paths.
    ///
    /// [`fill_target`]: ppa::PpaFillTarget
    #[cfg(feature = "accel-ppa")]
    pub fn ppa_clear(
        &self,
        fill_target: &ppa::PpaFillTarget<'_>,
        fill_val: u32,
    ) -> Result<(), esp_idf_sys::esp_err_t> {
        fill_target.clear(fill_val)
    }
}

impl<'a, D> RenderTarget for PpaRenderTarget<'a, D>
where
    D: DrawTarget + OriginDimensions,
    D::Color: PixelColor + Interpolate + AlphaColor,
{
    type ColorFormat = D::Color;

    fn size(&self) -> Size {
        self.inner.size()
    }

    fn clear(&mut self, color: Self::ColorFormat) {
        self.inner.clear(color)
    }

    fn clip_rect(&self) -> Rectangle {
        self.inner.clip_rect()
    }

    fn with_layer<LayerFn, DrawFn>(&mut self, layer_fn: LayerFn, draw_fn: DrawFn)
    where
        LayerFn: FnOnce(LayerHandle<Self::ColorFormat>) -> LayerHandle<Self::ColorFormat>,
        DrawFn: FnOnce(&mut Self),
    {
        self.inner.with_layer(layer_fn, |inner| {
            // SAFETY: `Self` is `#[repr(transparent)]` over `inner`'s type,
            // so the two have identical layout and a `&mut inner` is a
            // valid `&mut Self`. The user's closure is the only consumer
            // of this borrow within this scope, so no aliasing occurs.
            let self_ref: &mut Self = unsafe { &mut *(inner as *mut _ as *mut Self) };
            draw_fn(self_ref);
        });
    }

    fn alpha(&self) -> u8 {
        self.inner.alpha()
    }

    fn report_active_animation(&mut self) {
        self.inner.report_active_animation()
    }

    fn clear_animation_status(&mut self) -> bool {
        self.inner.clear_animation_status()
    }

    fn fill<C: Into<Self::ColorFormat>>(
        &mut self,
        transform: impl Into<LinearTransform>,
        brush: &impl Brush<ColorFormat = C>,
        brush_offset: Option<Point>,
        shape: &impl Shape,
    ) {
        // Phase 4–5 intercept here for solid-color rectangle fills →
        // PPA fill. Phase 6 intercepts image brushes → PPA blit.
        self.inner.fill(transform, brush, brush_offset, shape)
    }

    fn stroke<C: Into<Self::ColorFormat>>(
        &mut self,
        stroke: &Stroke,
        transform: impl Into<LinearTransform>,
        brush: &impl Brush<ColorFormat = C>,
        brush_offset: Option<Point>,
        shape: &impl Shape,
    ) {
        self.inner
            .stroke(stroke, transform, brush, brush_offset, shape)
    }

    fn draw_glyphs<C: Into<Self::ColorFormat>, F: FontRender<Self::ColorFormat>>(
        &mut self,
        offset: Point,
        brush: &impl Brush<ColorFormat = C>,
        glyphs: impl Iterator<Item = Glyph>,
        font: &F,
        font_attributes: &F::Attributes,
        conservative_bounds: &Rectangle,
    ) {
        // Glyph rasterization stays software in every phase: the PPA can
        // fill rectangles and blit images but cannot rasterize bezier
        // curves, so there is no fast-path for fonts.
        self.inner.draw_glyphs(
            offset,
            brush,
            glyphs,
            font,
            font_attributes,
            conservative_bounds,
        )
    }

    fn raw_surface(&mut self) -> impl Surface<Color = Self::ColorFormat> + '_ {
        self.inner.raw_surface()
    }
}

/// Phase 7 render target: like [`PpaRenderTarget`] but with hardware
/// acceleration for Buoyant's `view.opacity(α)` modifier. Requires the
/// inner DrawTarget to implement [`ppa::LayerStack`] (use
/// [`ppa::PpaLayeredFramebuffer`]) so the wrapper can hand off opacity
/// regions to the PPA blend engine on layer exit.
///
/// `#[repr(transparent)]` over the same `EmbeddedGraphicsRenderTarget`
/// shell as [`PpaRenderTarget`], so the nested `with_layer` recast
/// trick from Phase 1 stays sound. The opacity path is the only
/// override; every other trait method delegates to the inner target.
///
/// **Limitation (v0):** `LayerHandle::clip` and `LayerHandle::transform`
/// applied alongside an alpha change are not propagated to the inner
/// draw — the PPA path uses the LayerHandle's alpha and ignores the
/// rest. Mixing clip with opacity falls back silently; document this
/// for users.
#[cfg(feature = "accel-ppa")]
#[repr(transparent)]
pub struct PpaLayeredRenderTarget<'a, D>
where
    D: DrawTarget + OriginDimensions + ppa::LayerStack,
    D::Color: PixelColor + Interpolate + AlphaColor,
{
    inner: EmbeddedGraphicsRenderTarget<DrawTargetSurface<'a, D>>,
}

#[cfg(feature = "accel-ppa")]
impl<'a, D> PpaLayeredRenderTarget<'a, D>
where
    D: DrawTarget + OriginDimensions + ppa::LayerStack,
    D::Color: PixelColor + Interpolate + AlphaColor,
{
    pub fn new(display: &'a mut D) -> Self {
        Self {
            inner: EmbeddedGraphicsRenderTarget::new(display),
        }
    }

    pub fn new_hinted(display: &'a mut D, background_hint: D::Color) -> Self {
        Self {
            inner: EmbeddedGraphicsRenderTarget::new_hinted(display, background_hint),
        }
    }

    pub fn display(&self) -> &D {
        self.inner.display()
    }

    pub fn display_mut(&mut self) -> &mut D {
        self.inner.display_mut()
    }
}

#[cfg(feature = "accel-ppa")]
impl<'a, D> RenderTarget for PpaLayeredRenderTarget<'a, D>
where
    D: DrawTarget + OriginDimensions + ppa::LayerStack,
    D::Color: PixelColor + Interpolate + AlphaColor,
{
    type ColorFormat = D::Color;

    fn size(&self) -> Size {
        self.inner.size()
    }

    fn clear(&mut self, color: Self::ColorFormat) {
        self.inner.clear(color)
    }

    fn clip_rect(&self) -> Rectangle {
        self.inner.clip_rect()
    }

    fn with_layer<LayerFn, DrawFn>(&mut self, layer_fn: LayerFn, draw_fn: DrawFn)
    where
        LayerFn: FnOnce(LayerHandle<Self::ColorFormat>) -> LayerHandle<Self::ColorFormat>,
        DrawFn: FnOnce(&mut Self),
    {
        // Probe layer_fn on a fresh LayerConfig to read the resulting
        // alpha. LayerFn is `FnOnce`, so the probe consumes it — for
        // the α = 255 path we can't replay the layer_fn against EGRT.
        // Synthesising an equivalent layer_fn is not safe in the
        // general case (probed clip/transform live in global
        // coordinates relative to identity; re-applying them through
        // EGRT's LayerHandle methods would double-transform when EGRT
        // already has a non-identity transform from a parent layer).
        //
        // **v0 limitation**: clip and transform from a `LayerHandle`
        // are silently dropped at the boundary of every `with_layer`
        // invocation. The PPA path applies the layer's alpha cleanly,
        // but a `view.clip_to(rect).opacity(α)` view will fail to
        // clip. The counter UI doesn't use clip or transform inside
        // opacity layers; UIs that do should fall back to
        // `PpaRenderTarget` and pay the software cost until upstream
        // Buoyant exposes direct `LayerConfig` introspection (then
        // we can recover clip/transform correctly).
        let mut probe = LayerConfig::new_sized(self.size());
        let _ = layer_fn(LayerHandle::new(&mut probe));
        let alpha = probe.alpha;

        if alpha == 0 {
            return;
        }

        if alpha == 255 {
            // No opacity work to accelerate. Run draw_fn directly
            // against self — clip/transform from the layer_fn are
            // dropped (see v0 limitation above), but the typical
            // α = 255 path is a plain pass-through with no clip or
            // transform anyway.
            draw_fn(self);
            return;
        }

        // PPA path. Push an ARGB8888 scratch layer onto the inner
        // DrawTarget's layer stack (allocates on first use of this
        // depth, reused thereafter), run the inner draw at full
        // opacity into the scratch buffer, and pop with a PPA alpha
        // blend onto whatever was underneath.
        self.inner.display_mut().push_layer(alpha);
        draw_fn(self);
        self.inner.display_mut().pop_layer_blend();
    }

    fn alpha(&self) -> u8 {
        // We don't propagate the layer alpha down to the inner
        // EmbeddedGraphicsRenderTarget — the PPA blend on layer exit
        // applies it. The inner target draws at full intensity.
        255
    }

    fn report_active_animation(&mut self) {
        self.inner.report_active_animation()
    }

    fn clear_animation_status(&mut self) -> bool {
        self.inner.clear_animation_status()
    }

    fn fill<C: Into<Self::ColorFormat>>(
        &mut self,
        transform: impl Into<LinearTransform>,
        brush: &impl Brush<ColorFormat = C>,
        brush_offset: Option<Point>,
        shape: &impl Shape,
    ) {
        self.inner.fill(transform, brush, brush_offset, shape)
    }

    fn stroke<C: Into<Self::ColorFormat>>(
        &mut self,
        stroke: &Stroke,
        transform: impl Into<LinearTransform>,
        brush: &impl Brush<ColorFormat = C>,
        brush_offset: Option<Point>,
        shape: &impl Shape,
    ) {
        self.inner
            .stroke(stroke, transform, brush, brush_offset, shape)
    }

    fn draw_glyphs<C: Into<Self::ColorFormat>, F: FontRender<Self::ColorFormat>>(
        &mut self,
        offset: Point,
        brush: &impl Brush<ColorFormat = C>,
        glyphs: impl Iterator<Item = Glyph>,
        font: &F,
        font_attributes: &F::Attributes,
        conservative_bounds: &Rectangle,
    ) {
        self.inner.draw_glyphs(
            offset,
            brush,
            glyphs,
            font,
            font_attributes,
            conservative_bounds,
        )
    }

    fn raw_surface(&mut self) -> impl Surface<Color = Self::ColorFormat> + '_ {
        self.inner.raw_surface()
    }
}
