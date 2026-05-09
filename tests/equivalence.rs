//! Phase 1 acceptance test.
//!
//! Renders identical operations through `PpaRenderTarget` and
//! `EmbeddedGraphicsRenderTarget`; the two backbuffers must come out
//! pixel-identical. This is the wiring proof — when later phases
//! intercept methods to dispatch to the PPA, those phases will need
//! their own tests; this test stays as a regression guard against the
//! software fallback path drifting from upstream.

use buoyant::primitives::geometry::Rectangle as BuoyantRectangle;
use buoyant::primitives::transform::LinearTransform;
use buoyant::primitives::{Point as BuoyantPoint, Size as BuoyantSize};
use buoyant::render_target::{EmbeddedGraphicsRenderTarget, RenderTarget, SolidBrush};
use buoyant_esp32p4::PpaRenderTarget;
use embedded_graphics::mock_display::MockDisplay;
use embedded_graphics::pixelcolor::{Rgb565, RgbColor};

fn fresh_display() -> MockDisplay<Rgb565> {
    let mut d = MockDisplay::<Rgb565>::new();
    // Buoyant's primitive renderers may overdraw shape edges; allow it
    // here so the comparison is about *what gets drawn*, not the
    // strict-mode invariant.
    d.set_allow_overdraw(true);
    d.set_allow_out_of_bounds_drawing(true);
    d
}

#[test]
fn clear_matches_upstream() {
    let mut a = fresh_display();
    let mut b = fresh_display();

    PpaRenderTarget::new(&mut a).clear(Rgb565::CYAN);
    EmbeddedGraphicsRenderTarget::new(&mut b).clear(Rgb565::CYAN);

    assert_eq!(a, b, "clear() pixels diverged from upstream");
}

#[test]
fn fill_rectangle_matches_upstream() {
    let mut a = fresh_display();
    let mut b = fresh_display();

    let shape = BuoyantRectangle::new(BuoyantPoint::new(8, 12), BuoyantSize::new(20, 16));
    let brush = SolidBrush::new(Rgb565::RED);
    let xform = || LinearTransform::default();

    {
        let mut t = PpaRenderTarget::new(&mut a);
        t.clear(Rgb565::BLACK);
        t.fill(xform(), &brush, None, &shape);
    }
    {
        let mut t = EmbeddedGraphicsRenderTarget::new(&mut b);
        t.clear(Rgb565::BLACK);
        t.fill(xform(), &brush, None, &shape);
    }

    assert_eq!(a, b, "fill(rect, solid) diverged from upstream");
}

#[test]
fn with_layer_matches_upstream() {
    // Verifies the &mut Self recast inside our `with_layer` impl works:
    // the inner draw call lands on the same pixels as upstream's path.
    let mut a = fresh_display();
    let mut b = fresh_display();

    let shape = BuoyantRectangle::new(BuoyantPoint::new(4, 4), BuoyantSize::new(10, 10));
    let brush = SolidBrush::new(Rgb565::GREEN);
    let xform = || LinearTransform::default();

    {
        let mut t = PpaRenderTarget::new(&mut a);
        t.clear(Rgb565::BLACK);
        t.with_layer(
            |layer| layer,
            |inner| inner.fill(xform(), &brush, None, &shape),
        );
    }
    {
        let mut t = EmbeddedGraphicsRenderTarget::new(&mut b);
        t.clear(Rgb565::BLACK);
        t.with_layer(
            |layer| layer,
            |inner| inner.fill(xform(), &brush, None, &shape),
        );
    }

    assert_eq!(a, b, "with_layer body diverged from upstream");
}
