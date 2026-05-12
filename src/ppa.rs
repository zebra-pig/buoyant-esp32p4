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
    ppa_blend_oper_config_t, ppa_fill_oper_config_t, ppa_operation_t,
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
