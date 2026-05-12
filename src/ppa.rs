//! Safe Rust wrappers over the ESP-IDF PPA (Pixel Processing Accelerator)
//! driver, plus PSRAM-aligned framebuffer allocation and cache-flush
//! helpers. **No Buoyant integration in this module** — that arrives in
//! Phase 4 when the trait methods on [`crate::PpaRenderTarget`] start
//! dispatching to the PPA fast-paths defined here.
//!
//! All exposed types and functions go through `esp_idf_sys::ppa::*`, the
//! extra-bindings module declared in this crate's `Cargo.toml`. Consumers
//! that enable `accel-ppa` automatically pick those bindings up because
//! `esp-idf-sys` aggregates `package.metadata.esp-idf-sys.extra_components`
//! across the dependency graph.
//!
//! Only available on ESP32-P4. On other ESP32 targets the PPA peripheral
//! does not exist; symbols will fail to link.

use core::ffi::c_void;
use core::ptr;
use esp_idf_sys as sys;

use sys::ppa::{
    esp_cache_msync, heap_caps_aligned_alloc, heap_caps_aligned_free, ppa_client_config_t,
    ppa_client_handle_t, ppa_do_blend, ppa_do_fill, ppa_do_scale_rotate_mirror,
    ppa_register_client, ppa_unregister_client, MALLOC_CAP_SPIRAM,
};

pub use sys::ppa::{
    ppa_blend_oper_config_t, ppa_fill_color_mode_t,
    ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_ARGB8888,
    ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_RGB565,
    ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_RGB888, ppa_fill_oper_config_t, ppa_operation_t,
    ppa_operation_t_PPA_OPERATION_BLEND, ppa_operation_t_PPA_OPERATION_FILL,
    ppa_operation_t_PPA_OPERATION_SRM, ppa_srm_oper_config_t, ppa_trans_mode_t,
    ppa_trans_mode_t_PPA_TRANS_MODE_BLOCKING, ppa_trans_mode_t_PPA_TRANS_MODE_NON_BLOCKING,
};

/// Cache-line size the PPA requires for any buffer it touches (input or
/// output). 64 bytes covers ESP32-P4's L1 cache line; external memory
/// (PSRAM) additionally requires alignment to the L2 cache line size,
/// which on P4 is also 64 bytes for the default configuration.
pub const CACHE_LINE: usize = 64;

/// A registered PPA client. The driver maintains an internal queue of
/// pending transactions per client; dropping this handle unregisters
/// the client and frees that queue. One `Client` per [`Operation`] type
/// is the documented usage pattern.
pub struct Client {
    handle: ppa_client_handle_t,
}

/// Which PPA operation a [`Client`] is registered for.
#[derive(Copy, Clone, Debug)]
pub enum Operation {
    /// Scale, rotate or mirror an input picture into an output picture.
    Srm,
    /// Alpha-blend a foreground over a background into an output.
    Blend,
    /// Fill a target window with a constant pixel value.
    Fill,
}

impl Operation {
    fn raw(self) -> ppa_operation_t {
        match self {
            Operation::Srm => ppa_operation_t_PPA_OPERATION_SRM,
            Operation::Blend => ppa_operation_t_PPA_OPERATION_BLEND,
            Operation::Fill => ppa_operation_t_PPA_OPERATION_FILL,
        }
    }
}

impl Client {
    /// Register a new PPA client. `max_pending_trans_num` of 1 is
    /// sufficient when every call uses [`ppa_trans_mode_t_PPA_TRANS_MODE_BLOCKING`].
    pub fn new(op: Operation, max_pending: u32) -> Result<Self, sys::esp_err_t> {
        let config = ppa_client_config_t {
            oper_type: op.raw(),
            max_pending_trans_num: max_pending,
            // 0 == default (PPA_DATA_BURST_LENGTH_128). Avoid pulling in
            // the enum constant since its name varies across IDF revisions.
            data_burst_length: 0,
        };
        let mut handle: ppa_client_handle_t = ptr::null_mut();
        let err = unsafe { ppa_register_client(&config, &mut handle) };
        if err != sys::ESP_OK {
            return Err(err);
        }
        Ok(Self { handle })
    }

    /// Convenience: a fill client with `max_pending = 1`.
    pub fn new_fill() -> Result<Self, sys::esp_err_t> {
        Self::new(Operation::Fill, 1)
    }

    /// Convenience: a blend client with `max_pending = 1`.
    pub fn new_blend() -> Result<Self, sys::esp_err_t> {
        Self::new(Operation::Blend, 1)
    }

    /// Convenience: a scale-rotate-mirror client with `max_pending = 1`.
    pub fn new_srm() -> Result<Self, sys::esp_err_t> {
        Self::new(Operation::Srm, 1)
    }

    /// Raw handle for use with higher-level PPA helpers that don't yet
    /// have a wrapper.
    pub fn raw(&self) -> ppa_client_handle_t {
        self.handle
    }

    /// Submit a fill transaction. The `out.buffer` must be 64-byte
    /// aligned (cache-line aligned) — use [`alloc_psram_aligned`] to
    /// allocate framebuffers.
    pub fn do_fill(&self, config: &ppa_fill_oper_config_t) -> Result<(), sys::esp_err_t> {
        match unsafe { ppa_do_fill(self.handle, config) } {
            sys::ESP_OK => Ok(()),
            err => Err(err),
        }
    }

    /// Submit a blend transaction.
    pub fn do_blend(&self, config: &ppa_blend_oper_config_t) -> Result<(), sys::esp_err_t> {
        match unsafe { ppa_do_blend(self.handle, config) } {
            sys::ESP_OK => Ok(()),
            err => Err(err),
        }
    }

    /// Submit a scale-rotate-mirror transaction.
    pub fn do_srm(&self, config: &ppa_srm_oper_config_t) -> Result<(), sys::esp_err_t> {
        match unsafe { ppa_do_scale_rotate_mirror(self.handle, config) } {
            sys::ESP_OK => Ok(()),
            err => Err(err),
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe {
            ppa_unregister_client(self.handle);
        }
    }
}

unsafe impl Send for Client {}

/// A heap allocation in PSRAM, cache-line aligned for the PPA.
///
/// The buffer is freed when this value is dropped. Constructing a
/// `PsramBuffer` does not zero the memory; cast through [`Self::as_ptr_mut`]
/// and write what you need (or call [`Self::zero`]).
pub struct PsramBuffer {
    ptr: *mut u8,
    size: usize,
}

impl PsramBuffer {
    /// Allocate `size` bytes in PSRAM with [`CACHE_LINE`] alignment. The
    /// PPA driver requires every input/output buffer to satisfy this.
    pub fn new(size: usize) -> Option<Self> {
        let ptr = unsafe { heap_caps_aligned_alloc(CACHE_LINE, size, MALLOC_CAP_SPIRAM) }
            as *mut u8;
        if ptr.is_null() {
            None
        } else {
            Some(Self { ptr, size })
        }
    }

    /// Allocate and zero `size` bytes in aligned PSRAM.
    pub fn new_zeroed(size: usize) -> Option<Self> {
        let mut b = Self::new(size)?;
        b.zero();
        Some(b)
    }

    /// Write zeros over the entire buffer.
    pub fn zero(&mut self) {
        unsafe { ptr::write_bytes(self.ptr, 0, self.size) };
    }

    /// Raw pointer to the buffer's start.
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    /// Mutable raw pointer to the buffer's start.
    pub fn as_ptr_mut(&mut self) -> *mut u8 {
        self.ptr
    }

    /// Size of the allocation in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Borrow the allocation as a byte slice. Caller is responsible for
    /// honouring any PPA cache-coherency invariants before reading data
    /// the PPA wrote (see [`msync_invalidate`]).
    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr, self.size) }
    }

    /// Mutable counterpart of [`Self::as_slice`].
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.size) }
    }
}

impl Drop for PsramBuffer {
    fn drop(&mut self) {
        unsafe { heap_caps_aligned_free(self.ptr as *mut c_void) };
    }
}

unsafe impl Send for PsramBuffer {}

/// Flush dirty cache lines covering `[ptr, ptr+size)` out to memory, so
/// the PPA reads what the CPU just wrote. Call this on PPA *input*
/// buffers after CPU writes and before submitting the transaction.
///
/// `ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_TYPE_DATA`.
pub fn msync_flush(ptr: *const u8, size: usize) -> Result<(), sys::esp_err_t> {
    // Direction C2M = 0x0, Type DATA = 0x0 → flags = 0. Some IDF revisions
    // also accept ESP_CACHE_MSYNC_FLAG_UNALIGNED if alignment is loose,
    // but PPA buffers are always 64-byte aligned so leave it off.
    let err = unsafe { esp_cache_msync(ptr as *mut c_void, size, 0i32) };
    match err {
        sys::ESP_OK => Ok(()),
        e => Err(e),
    }
}

/// Invalidate cache lines covering `[ptr, ptr+size)` so the CPU re-reads
/// data the PPA just wrote. Call this on PPA *output* buffers after the
/// transaction completes and before the CPU reads pixels back.
///
/// `ESP_CACHE_MSYNC_FLAG_DIR_M2C | ESP_CACHE_MSYNC_FLAG_TYPE_DATA`.
pub fn msync_invalidate(ptr: *mut u8, size: usize) -> Result<(), sys::esp_err_t> {
    // Direction M2C is bit 0; type DATA is 0. See esp_cache.h.
    const ESP_CACHE_MSYNC_FLAG_DIR_M2C: i32 = 1 << 0;
    let err = unsafe { esp_cache_msync(ptr as *mut c_void, size, ESP_CACHE_MSYNC_FLAG_DIR_M2C) };
    match err {
        sys::ESP_OK => Ok(()),
        e => Err(e),
    }
}

/// A bound output framebuffer the PPA fill engine can write into. Holds
/// the borrowed [`Client`] (registered for `Operation::Fill`) plus the
/// destination buffer metadata that doesn't change between frames:
/// pointer, byte length, width, height, and the buffer's PPA color mode.
///
/// Per-frame work happens in [`Self::clear`]: it builds the
/// [`ppa_fill_oper_config_t`], submits a blocking transaction, and
/// invalidates the L1/L2 cache lines covering the destination so the
/// CPU's next read (typically the blit that pushes pixels to the panel)
/// sees the PPA's writes.
///
/// The pointer + size are NOT owned — the caller (typically a wrapper
/// around the on-screen framebuffer) is responsible for keeping the
/// allocation alive for the lifetime `'a`.
pub struct PpaFillTarget<'a> {
    client: &'a Client,
    framebuffer_ptr: *mut u8,
    framebuffer_bytes: usize,
    width: u32,
    height: u32,
    color_mode: ppa_fill_color_mode_t,
}

impl<'a> PpaFillTarget<'a> {
    /// Bind a fill client to a destination framebuffer.
    ///
    /// # Safety
    /// `framebuffer_ptr` must point at a writable allocation of at least
    /// `framebuffer_bytes` bytes, 64-byte aligned (use [`PsramBuffer`] or
    /// `heap_caps_aligned_alloc(64, …, MALLOC_CAP_SPIRAM)`). The
    /// allocation must remain valid for the entire lifetime `'a`.
    pub unsafe fn new(
        client: &'a Client,
        framebuffer_ptr: *mut u8,
        framebuffer_bytes: usize,
        width: u32,
        height: u32,
        color_mode: ppa_fill_color_mode_t,
    ) -> Self {
        Self {
            client,
            framebuffer_ptr,
            framebuffer_bytes,
            width,
            height,
            color_mode,
        }
    }

    /// Fill the entire framebuffer with `fill_val` (already encoded for
    /// the buffer's color mode — e.g. an Rgb565 word packed into the low
    /// 16 bits when [`Self`] was created with
    /// [`ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_RGB565`]). Blocks
    /// until the PPA finishes the transaction and the destination
    /// cache lines are invalidated.
    pub fn clear(&self, fill_val: u32) -> Result<(), sys::esp_err_t> {
        self.fill_rect(0, 0, self.width, self.height, fill_val)
    }

    /// Fill a sub-rectangle `[x, x+w) × [y, y+h)` of the framebuffer
    /// with `fill_val`. Same semantics and constraints as [`Self::clear`].
    /// Clipping to the framebuffer bounds is the caller's responsibility
    /// — the PPA driver will reject out-of-range coordinates.
    pub fn fill_rect(
        &self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        fill_val: u32,
    ) -> Result<(), sys::esp_err_t> {
        let mut out_anon = sys::ppa::ppa_out_pic_blk_config_t__bindgen_ty_1::default();
        out_anon.fill_cm = self.color_mode;
        let mut fill_anon = sys::ppa::ppa_fill_oper_config_t__bindgen_ty_1::default();
        fill_anon.fill_color_val = fill_val;
        let cfg = ppa_fill_oper_config_t {
            out: sys::ppa::ppa_out_pic_blk_config_t {
                buffer: self.framebuffer_ptr as *mut c_void,
                buffer_size: self.framebuffer_bytes as u32,
                pic_w: self.width,
                pic_h: self.height,
                block_offset_x: x,
                block_offset_y: y,
                __bindgen_anon_1: out_anon,
                yuv_range: 0,
                yuv_std: 0,
            },
            fill_block_w: w,
            fill_block_h: h,
            __bindgen_anon_1: fill_anon,
            mode: ppa_trans_mode_t_PPA_TRANS_MODE_BLOCKING,
            user_data: ptr::null_mut(),
        };
        self.client.do_fill(&cfg)?;
        msync_invalidate(self.framebuffer_ptr, self.framebuffer_bytes)
    }
}

unsafe impl<'a> Send for PpaFillTarget<'a> {}

/// `embedded-graphics` [`DrawTarget`] wrapper that intercepts
/// [`DrawTarget::fill_solid`] for sufficiently large rectangles and
/// dispatches them to the PPA fill engine instead of going through the
/// inner display's pixel-level path. All other methods pass through.
///
/// Phase 5 of the roadmap. Buoyant's `RenderTarget::fill(rect, solid_brush, …)`
/// ultimately lowers to `embedded-graphics`'s `fill_solid`; intercepting
/// there gets us PPA fast-paths for button backgrounds and other solid
/// rectangles without touching [`crate::PpaRenderTarget`]'s `#[repr(transparent)]`
/// invariant (which Phase 1's `with_layer` recast depends on).
///
/// The inner display must back its pixels with the same memory the
/// `PpaFillTarget` was configured to point at — typically a single
/// framebuffer struct that both this wrapper borrows mutably and the
/// `PpaFillTarget` borrows the raw pointer of. The caller is
/// responsible for that aliasing being well-behaved across the PPA
/// dispatch boundary; the PPA path invalidates cache lines after the
/// fill so subsequent CPU reads observe the fill.
pub struct PpaDrawTarget<'a, D>
where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>
        + embedded_graphics::geometry::OriginDimensions,
{
    inner: &'a mut D,
    fill_target: &'a PpaFillTarget<'a>,
    /// Below this pixel count, fall through to the software path —
    /// the PPA's per-transaction setup cost outweighs the bandwidth
    /// win on tiny fills.
    min_pixels_for_ppa: u32,
}

impl<'a, D> PpaDrawTarget<'a, D>
where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>
        + embedded_graphics::geometry::OriginDimensions,
{
    /// Wrap `inner` so its `fill_solid` calls dispatch through `fill_target`
    /// when the area exceeds [`Self::min_pixels_for_ppa`].
    pub fn new(inner: &'a mut D, fill_target: &'a PpaFillTarget<'a>) -> Self {
        Self {
            inner,
            fill_target,
            // Default crossover: roughly where the PPA's per-transaction
            // setup cost (~100 µs on ESP32-P4) breaks even with CPU
            // PSRAM writes (~30 ns/px). Below this we don't bother.
            min_pixels_for_ppa: 4096,
        }
    }

    /// Override the default PPA dispatch threshold.
    pub fn with_min_pixels_for_ppa(mut self, n: u32) -> Self {
        self.min_pixels_for_ppa = n;
        self
    }

    /// Borrow the underlying display.
    pub fn inner(&self) -> &D {
        self.inner
    }

    /// Mutable counterpart of [`Self::inner`].
    pub fn inner_mut(&mut self) -> &mut D {
        self.inner
    }
}

impl<'a, D> embedded_graphics::geometry::OriginDimensions for PpaDrawTarget<'a, D>
where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>
        + embedded_graphics::geometry::OriginDimensions,
{
    fn size(&self) -> embedded_graphics::geometry::Size {
        self.inner.size()
    }
}

impl<'a, D> embedded_graphics::draw_target::DrawTarget for PpaDrawTarget<'a, D>
where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>
        + embedded_graphics::geometry::OriginDimensions,
{
    type Color = embedded_graphics::pixelcolor::Rgb565;
    type Error = D::Error;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = embedded_graphics::Pixel<Self::Color>>,
    {
        self.inner.draw_iter(pixels)
    }

    fn fill_contiguous<I>(
        &mut self,
        area: &embedded_graphics::primitives::Rectangle,
        colors: I,
    ) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        self.inner.fill_contiguous(area, colors)
    }

    fn fill_solid(
        &mut self,
        area: &embedded_graphics::primitives::Rectangle,
        color: Self::Color,
    ) -> Result<(), Self::Error> {
        use embedded_graphics::pixelcolor::raw::RawData;
        let pixels = area.size.width.saturating_mul(area.size.height);
        let display_size = self.inner.size();

        // Bail to software for sub-threshold fills and any area that
        // strays off-display (the PPA refuses out-of-range coordinates;
        // letting embedded-graphics handle clipping is cheaper than
        // reproducing it here).
        let in_bounds = area.top_left.x >= 0
            && area.top_left.y >= 0
            && area.top_left.x as u32 + area.size.width <= display_size.width
            && area.top_left.y as u32 + area.size.height <= display_size.height;

        if pixels < self.min_pixels_for_ppa || !in_bounds {
            return self.inner.fill_solid(area, color);
        }

        let raw =
            embedded_graphics::pixelcolor::raw::RawU16::from(color).into_inner() as u32;
        match self.fill_target.fill_rect(
            area.top_left.x as u32,
            area.top_left.y as u32,
            area.size.width,
            area.size.height,
            raw,
        ) {
            Ok(()) => Ok(()),
            Err(_) => self.inner.fill_solid(area, color),
        }
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        use embedded_graphics::pixelcolor::raw::RawData;
        let raw =
            embedded_graphics::pixelcolor::raw::RawU16::from(color).into_inner() as u32;
        match self.fill_target.clear(raw) {
            Ok(()) => Ok(()),
            Err(_) => self.inner.clear(color),
        }
    }
}
