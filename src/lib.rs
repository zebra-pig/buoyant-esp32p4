//! Hardware-accelerated Buoyant render target for ESP32-P4.
//!
//! This crate plugs into Buoyant by implementing
//! [`buoyant::render_target::RenderTarget`] with fast-paths that dispatch
//! to the ESP32-P4 PPA (Pixel Processing Accelerator) for the operations
//! the hardware can do — solid rectangle fills, image blits, alpha blends.
//! Everything else (curves, glyph rasterization) falls back to the
//! software path through `embedded-graphics` primitives.
//!
//! Status: pre-implementation scaffold. See `ROADMAP.md` at the repo root
//! for the phased plan.

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(dead_code)]

// Phase 1 places `pub struct PpaRenderTarget<...>` and its
// `impl RenderTarget` here. Until then the crate is an empty placeholder
// that other crates can depend on without a breaking signature.
