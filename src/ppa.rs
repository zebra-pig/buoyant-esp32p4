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
    ppa_alpha_update_mode_t, ppa_alpha_update_mode_t_PPA_ALPHA_FIX_VALUE,
    ppa_alpha_update_mode_t_PPA_ALPHA_NO_CHANGE, ppa_alpha_update_mode_t_PPA_ALPHA_SCALE,
    ppa_blend_color_mode_t, ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_ARGB8888,
    ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_RGB565,
    ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_RGB888, ppa_blend_oper_config_t,
    ppa_fill_color_mode_t, ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_ARGB8888,
    ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_RGB565,
    ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_RGB888, ppa_fill_oper_config_t, ppa_operation_t,
    ppa_operation_t_PPA_OPERATION_BLEND, ppa_operation_t_PPA_OPERATION_FILL,
    ppa_operation_t_PPA_OPERATION_SRM, ppa_srm_color_mode_t,
    ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_ARGB8888,
    ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB565,
    ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB888, ppa_srm_oper_config_t,
    ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_0, ppa_trans_mode_t,
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
    /// `framebuffer_bytes` bytes, [`CACHE_LINE`]-aligned (use
    /// [`PsramBuffer`] or `heap_caps_aligned_alloc(64, …, MALLOC_CAP_SPIRAM)`).
    /// The allocation must remain valid for the entire lifetime `'a`.
    /// A debug build asserts the alignment; a release build trusts the
    /// caller (the PPA driver will reject misaligned buffers at
    /// dispatch time with an `ESP_ERR_INVALID_ARG`).
    pub unsafe fn new(
        client: &'a Client,
        framebuffer_ptr: *mut u8,
        framebuffer_bytes: usize,
        width: u32,
        height: u32,
        color_mode: ppa_fill_color_mode_t,
    ) -> Self {
        debug_assert!(
            (framebuffer_ptr as usize) % CACHE_LINE == 0,
            "PpaFillTarget framebuffer must be {CACHE_LINE}-byte aligned, got {framebuffer_ptr:p}"
        );
        debug_assert!(
            framebuffer_bytes >= (width as usize) * (height as usize) * 2,
            "PpaFillTarget framebuffer_bytes ({framebuffer_bytes}) too small for {width}x{height}"
        );
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

/// A bound output framebuffer the PPA scale-rotate-mirror engine can
/// blit into. Parallel to [`PpaFillTarget`] but for image copies rather
/// than solid fills: holds a borrowed [`Client`] (registered for
/// [`Operation::Srm`]) plus the destination buffer's pointer, size,
/// dimensions, and color mode.
///
/// Source buffers passed to [`Self::blit`] / [`Self::blit_scaled`] must
/// be 64-byte aligned and live in DMA-capable memory (PSRAM or DRAM).
/// The implementation flushes the source's cache lines before
/// submitting and invalidates the destination's afterward, so the
/// CPU's next read of either sees the final state.
pub struct PpaSrmTarget<'a> {
    client: &'a Client,
    framebuffer_ptr: *mut u8,
    framebuffer_bytes: usize,
    width: u32,
    height: u32,
    color_mode: ppa_srm_color_mode_t,
}

impl<'a> PpaSrmTarget<'a> {
    /// Bind an SRM client to a destination framebuffer.
    ///
    /// # Safety
    /// Same constraints as [`PpaFillTarget::new`].
    pub unsafe fn new(
        client: &'a Client,
        framebuffer_ptr: *mut u8,
        framebuffer_bytes: usize,
        width: u32,
        height: u32,
        color_mode: ppa_srm_color_mode_t,
    ) -> Self {
        debug_assert!(
            (framebuffer_ptr as usize) % CACHE_LINE == 0,
            "PpaSrmTarget framebuffer must be {CACHE_LINE}-byte aligned, got {framebuffer_ptr:p}"
        );
        Self {
            client,
            framebuffer_ptr,
            framebuffer_bytes,
            width,
            height,
            color_mode,
        }
    }

    /// 1:1 blit a source image into the framebuffer at `(dst_x, dst_y)`.
    ///
    /// The source is interpreted as a packed pixel grid of `src_w * src_h`
    /// pixels in the SRM target's color mode. Equivalent to
    /// [`Self::blit_scaled`] with `dst_w = src_w` and `dst_h = src_h`.
    ///
    /// # Safety
    /// `src_ptr` must point at a writable allocation of at least
    /// `src_w * src_h * bytes_per_pixel` bytes, 64-byte aligned. The
    /// allocation must remain valid for the duration of the call (the
    /// PPA transaction is submitted in blocking mode so the function
    /// returns only once the read is complete). Bytes-per-pixel is
    /// implied by the color mode passed to [`Self::new`].
    pub unsafe fn blit(
        &self,
        src_ptr: *const u8,
        src_w: u32,
        src_h: u32,
        dst_x: u32,
        dst_y: u32,
    ) -> Result<(), sys::esp_err_t> {
        self.blit_scaled(src_ptr, src_w, src_h, dst_x, dst_y, src_w, src_h)
    }

    /// Scaled blit: read `src_w × src_h` from `src_ptr` and write
    /// `dst_w × dst_h` at `(dst_x, dst_y)` on the framebuffer. Scale
    /// factors are computed from the size ratios. The PPA supports
    /// arbitrary positive scales; the ESP-IDF v5.5 driver clamps
    /// extreme ratios internally.
    ///
    /// # Safety
    /// Same as [`Self::blit`] regarding `src_ptr` lifetime and
    /// alignment.
    pub unsafe fn blit_scaled(
        &self,
        src_ptr: *const u8,
        src_w: u32,
        src_h: u32,
        dst_x: u32,
        dst_y: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> Result<(), sys::esp_err_t> {
        let bpp = srm_bytes_per_pixel(self.color_mode);
        let src_bytes = (src_w as usize) * (src_h as usize) * bpp;

        // Flush source cache lines so the PPA reads what the CPU just
        // wrote. Source is treated as immutable for the duration of
        // the transaction.
        msync_flush(src_ptr, src_bytes)?;

        let mut in_anon = sys::ppa::ppa_in_pic_blk_config_t__bindgen_ty_1::default();
        in_anon.srm_cm = self.color_mode;
        let mut out_anon = sys::ppa::ppa_out_pic_blk_config_t__bindgen_ty_1::default();
        out_anon.srm_cm = self.color_mode;

        let scale_x = dst_w as f32 / src_w as f32;
        let scale_y = dst_h as f32 / src_h as f32;

        let cfg = ppa_srm_oper_config_t {
            in_: sys::ppa::ppa_in_pic_blk_config_t {
                buffer: src_ptr as *const c_void,
                pic_w: src_w,
                pic_h: src_h,
                block_w: src_w,
                block_h: src_h,
                block_offset_x: 0,
                block_offset_y: 0,
                __bindgen_anon_1: in_anon,
                yuv_range: 0,
                yuv_std: 0,
            },
            out: sys::ppa::ppa_out_pic_blk_config_t {
                buffer: self.framebuffer_ptr as *mut c_void,
                buffer_size: self.framebuffer_bytes as u32,
                pic_w: self.width,
                pic_h: self.height,
                block_offset_x: dst_x,
                block_offset_y: dst_y,
                __bindgen_anon_1: out_anon,
                yuv_range: 0,
                yuv_std: 0,
            },
            rotation_angle: ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_0,
            scale_x,
            scale_y,
            mirror_x: false,
            mirror_y: false,
            rgb_swap: false,
            byte_swap: false,
            alpha_update_mode: ppa_alpha_update_mode_t_PPA_ALPHA_NO_CHANGE,
            __bindgen_anon_1: core::mem::zeroed(),
            mode: ppa_trans_mode_t_PPA_TRANS_MODE_BLOCKING,
            user_data: ptr::null_mut(),
        };
        self.client.do_srm(&cfg)?;
        msync_invalidate(self.framebuffer_ptr, self.framebuffer_bytes)
    }
}

unsafe impl<'a> Send for PpaSrmTarget<'a> {}

/// Phase 7 building block: a bound output framebuffer the PPA blend
/// engine can composite into. Parallel to [`PpaFillTarget`] and
/// [`PpaSrmTarget`]. Holds a borrowed blend-mode [`Client`] plus the
/// destination buffer's pointer, byte size, and dimensions (always
/// interpreted as RGB565 — the only background color mode we support
/// today).
///
/// Source buffers passed to [`Self::blend_argb_over_rgb565`] must be
/// ARGB8888, 64-byte aligned, and in DMA-capable memory. The
/// implementation flushes the source's cache lines before submitting
/// and invalidates the destination's afterward.
pub struct PpaBlendTarget<'a> {
    client: &'a Client,
    framebuffer_ptr: *mut u8,
    framebuffer_bytes: usize,
    width: u32,
    height: u32,
}

impl<'a> PpaBlendTarget<'a> {
    /// Bind a blend client to an RGB565 destination framebuffer.
    ///
    /// # Safety
    /// Same as [`PpaFillTarget::new`].
    pub unsafe fn new(
        client: &'a Client,
        framebuffer_ptr: *mut u8,
        framebuffer_bytes: usize,
        width: u32,
        height: u32,
    ) -> Self {
        debug_assert!(
            (framebuffer_ptr as usize) % CACHE_LINE == 0,
            "PpaBlendTarget framebuffer must be {CACHE_LINE}-byte aligned, got {framebuffer_ptr:p}"
        );
        Self {
            client,
            framebuffer_ptr,
            framebuffer_bytes,
            width,
            height,
        }
    }

    /// Alpha-blend an ARGB8888 source over the bound RGB565 framebuffer
    /// at `(dst_x, dst_y)`. `scalar_alpha` is the layer-level opacity
    /// (0-255); it's multiplied with the source's per-pixel alpha via
    /// the PPA's `PPA_ALPHA_SCALE` mode. Untouched source pixels
    /// (per-pixel alpha = 0) leave the destination unchanged; fully
    /// opaque source pixels (per-pixel alpha = 255) blend at
    /// `scalar_alpha / 255` strength.
    ///
    /// The destination is read by the PPA (it needs the current
    /// framebuffer contents to compute the over-blend) and written
    /// back. The destination block geometry is `src_w × src_h` starting
    /// at `(dst_x, dst_y)`.
    ///
    /// # Safety
    /// `src_argb_ptr` must point at a writable allocation of at least
    /// `src_w * src_h * 4` bytes, 64-byte aligned, in DMA-capable
    /// memory, valid for the duration of the call. The framebuffer
    /// region `[(dst_x, dst_y), (dst_x + src_w, dst_y + src_h))` must
    /// lie within the bound framebuffer.
    pub unsafe fn blend_argb_over_rgb565(
        &self,
        src_argb_ptr: *const u8,
        src_w: u32,
        src_h: u32,
        dst_x: u32,
        dst_y: u32,
        scalar_alpha: u8,
    ) -> Result<(), sys::esp_err_t> {
        let src_bytes = (src_w as usize) * (src_h as usize) * 4;
        msync_flush(src_argb_ptr, src_bytes)?;
        // Also flush the destination so the PPA reads up-to-date
        // pre-blend pixels (the CPU may have written into the FB
        // earlier in this frame).
        msync_flush(self.framebuffer_ptr, self.framebuffer_bytes)?;

        let mut bg_anon = sys::ppa::ppa_in_pic_blk_config_t__bindgen_ty_1::default();
        bg_anon.blend_cm = ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_RGB565;
        let mut fg_anon = sys::ppa::ppa_in_pic_blk_config_t__bindgen_ty_1::default();
        fg_anon.blend_cm = ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_ARGB8888;
        let mut out_anon = sys::ppa::ppa_out_pic_blk_config_t__bindgen_ty_1::default();
        out_anon.blend_cm = ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_RGB565;
        let mut bg_alpha_anon = sys::ppa::ppa_blend_oper_config_t__bindgen_ty_1::default();
        bg_alpha_anon.bg_alpha_fix_val = 255;
        // FG alpha: PPA_ALPHA_SCALE multiplies the per-pixel alpha by
        // this ratio. Range (0, 1); 0 isn't usable so clamp.
        let scale = (scalar_alpha as f32 / 255.0).clamp(1.0 / 256.0, 1.0);
        let mut fg_alpha_anon = sys::ppa::ppa_blend_oper_config_t__bindgen_ty_2::default();
        fg_alpha_anon.fg_alpha_scale_ratio = scale;

        let cfg = ppa_blend_oper_config_t {
            in_bg: sys::ppa::ppa_in_pic_blk_config_t {
                buffer: self.framebuffer_ptr as *const c_void,
                pic_w: self.width,
                pic_h: self.height,
                block_w: src_w,
                block_h: src_h,
                block_offset_x: dst_x,
                block_offset_y: dst_y,
                __bindgen_anon_1: bg_anon,
                yuv_range: 0,
                yuv_std: 0,
            },
            in_fg: sys::ppa::ppa_in_pic_blk_config_t {
                buffer: src_argb_ptr as *const c_void,
                pic_w: src_w,
                pic_h: src_h,
                block_w: src_w,
                block_h: src_h,
                block_offset_x: 0,
                block_offset_y: 0,
                __bindgen_anon_1: fg_anon,
                yuv_range: 0,
                yuv_std: 0,
            },
            out: sys::ppa::ppa_out_pic_blk_config_t {
                buffer: self.framebuffer_ptr as *mut c_void,
                buffer_size: self.framebuffer_bytes as u32,
                pic_w: self.width,
                pic_h: self.height,
                block_offset_x: dst_x,
                block_offset_y: dst_y,
                __bindgen_anon_1: out_anon,
                yuv_range: 0,
                yuv_std: 0,
            },
            bg_rgb_swap: false,
            bg_byte_swap: false,
            bg_alpha_update_mode: ppa_alpha_update_mode_t_PPA_ALPHA_NO_CHANGE,
            __bindgen_anon_1: bg_alpha_anon,
            fg_rgb_swap: false,
            fg_byte_swap: false,
            fg_alpha_update_mode: ppa_alpha_update_mode_t_PPA_ALPHA_SCALE,
            __bindgen_anon_2: fg_alpha_anon,
            fg_fix_rgb_val: sys::ppa::color_pixel_rgb888_data_t::default(),
            bg_ck_en: false,
            bg_ck_rgb_low_thres: sys::ppa::color_pixel_rgb888_data_t::default(),
            bg_ck_rgb_high_thres: sys::ppa::color_pixel_rgb888_data_t::default(),
            fg_ck_en: false,
            fg_ck_rgb_low_thres: sys::ppa::color_pixel_rgb888_data_t::default(),
            fg_ck_rgb_high_thres: sys::ppa::color_pixel_rgb888_data_t::default(),
            ck_rgb_default_val: sys::ppa::color_pixel_rgb888_data_t::default(),
            ck_reverse_bg2fg: false,
            mode: ppa_trans_mode_t_PPA_TRANS_MODE_BLOCKING,
            user_data: ptr::null_mut(),
        };
        self.client.do_blend(&cfg)?;
        msync_invalidate(self.framebuffer_ptr, self.framebuffer_bytes)
    }
}

unsafe impl<'a> Send for PpaBlendTarget<'a> {}

/// Phase 7 trait: a layer stack that a [`crate::PpaLayeredRenderTarget`]
/// can drive. Buoyant's `view.opacity(α)` causes the renderer to call
/// [`Self::push_layer`] before drawing the inner subtree and
/// [`Self::pop_layer_blend`] after, at which point the implementation
/// composites the captured contents onto whatever was underneath.
///
/// The framebuffer type the user supplies as the inner `D` implements
/// this trait. [`PpaLayeredFramebuffer`] is the ready-made
/// implementation that uses ARGB8888 PSRAM scratch buffers and PPA
/// alpha-blend on pop.
pub trait LayerStack {
    /// Push a new layer onto the stack with the given scalar alpha.
    /// Subsequent draws on the [`embedded_graphics::draw_target::DrawTarget`]
    /// route to the new layer's scratch buffer instead of the layer
    /// underneath.
    fn push_layer(&mut self, alpha: u8);

    /// Pop the top layer, alpha-composite its contents onto the layer
    /// underneath (or the base framebuffer if this was the last
    /// layer), and recycle the scratch buffer for the next push.
    /// Errors during composition fall through silently — the visible
    /// frame will be incorrect but the program continues.
    fn pop_layer_blend(&mut self);
}

/// Bytes per pixel for an SRM color mode. Only the RGB modes are
/// supported here; YUV needs more careful range/standard handling and
/// can be added when there's a concrete need.
fn srm_bytes_per_pixel(mode: ppa_srm_color_mode_t) -> usize {
    if mode == ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB565 {
        2
    } else if mode == ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB888 {
        3
    } else if mode == ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_ARGB8888 {
        4
    } else {
        // Best-effort fallback; the PPA will reject mis-sized buffers
        // on dispatch anyway.
        2
    }
}

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

/// Phase 7: a layer-stack-aware `embedded-graphics` framebuffer wrapper
/// that captures Buoyant `view.opacity(α)` regions into ARGB8888
/// scratch buffers in PSRAM and PPA-blends them onto the base RGB565
/// framebuffer on layer exit. Wraps any `DrawTarget<Color = Rgb565> +
/// OriginDimensions` so it can sit on top of `PpaDrawTarget`
/// (Phase 5) or a raw framebuffer.
///
/// Scratch buffers are allocated lazily on first `push_layer` and
/// reused across frames; nothing is freed until the wrapper is
/// dropped. Each buffer is `width × height × 4` bytes (full-screen
/// ARGB8888) so any opacity layer of any size fits without per-push
/// allocation churn. On a 720×1280 panel that's 3.7 MiB per layer;
/// with 32 MiB PSRAM and a 1.84 MiB base FB there's room for a half-
/// dozen concurrent layers before memory pressure matters.
///
/// Drawing semantics while layers are active: `fill_solid` and `clear`
/// write into the top-of-stack ARGB8888 buffer with per-pixel alpha
/// set to 255 (i.e. drawn pixels are opaque; untouched pixels keep
/// their initial alpha = 0 from `push_layer`'s zero-init).
/// `draw_iter` is supported but slow (per-pixel conversion through
/// the standard DrawTarget path); `fill_contiguous` falls back to
/// embedded-graphics's default looping over `draw_iter`. The PPA
/// scalar alpha attached to the layer is applied at `pop_layer_blend`.
///
/// **Limitations**: only RGB565 base framebuffers, only scalar layer
/// alpha (no per-pixel alpha brushes), and clip/transform from a
/// `LayerHandle` are not honoured inside the opacity region — Buoyant's
/// `with_layer` clip + transform paths still work, just not when
/// composed with this wrapper's opacity layering. Document that
/// limitation alongside the user-visible `PpaLayeredRenderTarget`.
pub struct PpaLayeredFramebuffer<'a, D>
where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>
        + embedded_graphics::geometry::OriginDimensions,
{
    base: &'a mut D,
    blend_target: &'a PpaBlendTarget<'a>,
    width: u32,
    height: u32,
    /// LIFO pool of ARGB8888 scratch buffers. Index `active_layers` is
    /// the next slot to fill on `push_layer`; indices below it are the
    /// currently-active stack.
    pool: std::vec::Vec<PsramBuffer>,
    alphas: std::vec::Vec<u8>,
    active_layers: usize,
}

impl<'a, D> PpaLayeredFramebuffer<'a, D>
where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>
        + embedded_graphics::geometry::OriginDimensions,
{
    /// Wrap a base RGB565 framebuffer. `blend_target` must be bound to
    /// the same underlying memory as `base` (typically constructed
    /// against the same raw pointer): the composite-on-pop step blends
    /// the top scratch buffer over the base by submitting `ppa_do_blend`
    /// with the framebuffer as both bg and out.
    pub fn new(base: &'a mut D, blend_target: &'a PpaBlendTarget<'a>) -> Self {
        let size = base.size();
        Self {
            base,
            blend_target,
            width: size.width,
            height: size.height,
            pool: std::vec::Vec::new(),
            alphas: std::vec::Vec::new(),
            active_layers: 0,
        }
    }

    /// Pre-allocate `n` scratch buffers so the first N `push_layer`
    /// calls don't pay the heap-cap allocation cost on the render
    /// thread. Optional — the wrapper allocates lazily otherwise.
    pub fn reserve_layers(&mut self, n: usize) -> Result<(), &'static str> {
        let bytes = (self.width as usize) * (self.height as usize) * 4;
        while self.pool.len() < n {
            let buf = PsramBuffer::new(bytes)
                .ok_or("PSRAM exhausted while reserving layer scratch")?;
            self.pool.push(buf);
        }
        Ok(())
    }

    /// Number of layers currently pushed (0 when drawing directly to
    /// the base framebuffer).
    pub fn layer_depth(&self) -> usize {
        self.active_layers
    }

    /// Write a single Rgb565 pixel at `(x, y)` into the active draw
    /// target — either the top-of-stack ARGB8888 buffer (with alpha
    /// promoted to 255) or, if no layers are active, the inner base
    /// DrawTarget.
    fn write_pixel(
        &mut self,
        x: i32,
        y: i32,
        color: embedded_graphics::pixelcolor::Rgb565,
    ) -> Result<(), D::Error> {
        if x < 0 || y < 0 || (x as u32) >= self.width || (y as u32) >= self.height {
            return Ok(());
        }
        if self.active_layers > 0 {
            let buf = &mut self.pool[self.active_layers - 1];
            unsafe {
                let off = (y as usize * self.width as usize + x as usize) * 4;
                let ptr = buf.as_ptr_mut().add(off);
                let argb = rgb565_to_argb8888_opaque(color);
                core::ptr::write(ptr.cast::<u32>(), argb);
            }
            Ok(())
        } else {
            use embedded_graphics::draw_target::DrawTarget;
            use embedded_graphics::Pixel;
            self.base.draw_iter(core::iter::once(Pixel(
                embedded_graphics::geometry::Point::new(x, y),
                color,
            )))
        }
    }
}

impl<'a, D> embedded_graphics::geometry::OriginDimensions for PpaLayeredFramebuffer<'a, D>
where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>
        + embedded_graphics::geometry::OriginDimensions,
{
    fn size(&self) -> embedded_graphics::geometry::Size {
        embedded_graphics::geometry::Size::new(self.width, self.height)
    }
}

impl<'a, D> embedded_graphics::draw_target::DrawTarget for PpaLayeredFramebuffer<'a, D>
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
        if self.active_layers == 0 {
            return self.base.draw_iter(pixels);
        }
        for embedded_graphics::Pixel(p, c) in pixels {
            self.write_pixel(p.x, p.y, c)?;
        }
        Ok(())
    }

    fn fill_solid(
        &mut self,
        area: &embedded_graphics::primitives::Rectangle,
        color: Self::Color,
    ) -> Result<(), Self::Error> {
        if self.active_layers == 0 {
            return self.base.fill_solid(area, color);
        }
        let x0 = area.top_left.x.max(0) as u32;
        let y0 = area.top_left.y.max(0) as u32;
        let x1 = ((area.top_left.x + area.size.width as i32).max(0) as u32).min(self.width);
        let y1 = ((area.top_left.y + area.size.height as i32).max(0) as u32).min(self.height);
        let argb = rgb565_to_argb8888_opaque(color);
        let buf = &mut self.pool[self.active_layers - 1];
        let width = self.width as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let off = (y as usize * width + x as usize) * 4;
                unsafe {
                    let ptr = buf.as_ptr_mut().add(off).cast::<u32>();
                    core::ptr::write(ptr, argb);
                }
            }
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        if self.active_layers == 0 {
            return self.base.clear(color);
        }
        let argb = rgb565_to_argb8888_opaque(color);
        let buf = &mut self.pool[self.active_layers - 1];
        let pixels = (self.width as usize) * (self.height as usize);
        unsafe {
            let ptr = buf.as_ptr_mut().cast::<u32>();
            for i in 0..pixels {
                core::ptr::write(ptr.add(i), argb);
            }
        }
        Ok(())
    }
}

impl<'a, D> LayerStack for PpaLayeredFramebuffer<'a, D>
where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>
        + embedded_graphics::geometry::OriginDimensions,
{
    fn push_layer(&mut self, alpha: u8) {
        // Grow the pool lazily on first use of each depth level.
        if self.pool.len() <= self.active_layers {
            let bytes = (self.width as usize) * (self.height as usize) * 4;
            let buf = match PsramBuffer::new(bytes) {
                Some(b) => b,
                None => {
                    // Out of PSRAM — track depth anyway so push/pop stay
                    // balanced, but the layer's draws will fault on a
                    // missing pool entry. Pragmatically fall back to
                    // doing nothing (drawing skipped via a guard) is
                    // cleaner; for now we panic to surface the problem.
                    panic!("PpaLayeredFramebuffer: PSRAM exhausted on push_layer");
                }
            };
            self.pool.push(buf);
        }
        // Zero the layer (alpha = 0 everywhere, i.e. fully transparent).
        self.pool[self.active_layers].zero();
        self.alphas.push(alpha);
        self.active_layers += 1;
    }

    fn pop_layer_blend(&mut self) {
        if self.active_layers == 0 {
            return;
        }
        let alpha = self.alphas.pop().expect("alphas mirrors active_layers");
        self.active_layers -= 1;
        let depth = self.active_layers;

        if depth == 0 {
            // Bottom layer popping back to the base FB → PPA blend.
            let src_ptr = self.pool[depth].as_ptr();
            let _ = unsafe {
                self.blend_target.blend_argb_over_rgb565(
                    src_ptr,
                    self.width,
                    self.height,
                    0,
                    0,
                    alpha,
                )
            };
        } else {
            // Nested layer → ARGB8888 composite onto the layer below.
            // Our `PpaBlendTarget` is RGB565-output-only, so this falls
            // back to a software composite. v0 limitation — see
            // ROADMAP. Use `split_at_mut` to borrow source (immutably)
            // and dest (mutably) concurrently without violating Rust's
            // exclusive-borrow rule.
            let (lower, upper) = self.pool.split_at_mut(depth);
            let src_ptr = upper[0].as_ptr().cast::<u32>();
            let dst_ptr = lower[depth - 1].as_ptr_mut().cast::<u32>();
            let pixels = (self.width as usize) * (self.height as usize);
            for i in 0..pixels {
                unsafe {
                    let s = core::ptr::read(src_ptr.add(i));
                    let s_a = ((s >> 24) & 0xFF) as u32;
                    if s_a == 0 {
                        continue;
                    }
                    let eff = (s_a * alpha as u32 + 127) / 255;
                    let d = core::ptr::read(dst_ptr.add(i));
                    let d_a = ((d >> 24) & 0xFF) as u32;
                    let inv = 255 - eff;
                    let blend_ch =
                        |sc: u32, dc: u32| -> u32 { (sc * eff + dc * inv + 127) / 255 };
                    let r = blend_ch((s >> 16) & 0xFF, (d >> 16) & 0xFF);
                    let g = blend_ch((s >> 8) & 0xFF, (d >> 8) & 0xFF);
                    let b = blend_ch(s & 0xFF, d & 0xFF);
                    // Output alpha: src eff + dst lit by inverse.
                    let out_a = (eff + (d_a * inv + 127) / 255).min(255);
                    let new = (out_a << 24) | (r << 16) | (g << 8) | b;
                    core::ptr::write(dst_ptr.add(i), new);
                }
            }
        }
    }
}

/// Expand an RGB565 pixel into a packed ARGB8888 `u32` (host endian)
/// with alpha = 255. Used by [`PpaLayeredFramebuffer`]'s DrawTarget impl
/// when writing into a scratch layer.
fn rgb565_to_argb8888_opaque(c: embedded_graphics::pixelcolor::Rgb565) -> u32 {
    use embedded_graphics::pixelcolor::RgbColor;
    let r = c.r() as u32; // 5 bits
    let g = c.g() as u32; // 6 bits
    let b = c.b() as u32; // 5 bits
    let r8 = (r << 3) | (r >> 2);
    let g8 = (g << 2) | (g >> 4);
    let b8 = (b << 3) | (b >> 2);
    (0xFFu32 << 24) | (r8 << 16) | (g8 << 8) | b8
}
