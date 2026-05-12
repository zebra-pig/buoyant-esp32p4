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
