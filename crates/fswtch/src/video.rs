//! Video image helpers and chromakey.
//!
//! Wraps the commonly-used subset of FreeSWITCH's video API (`switch_core_video.h` /
//! `switch_image.h`): decoding an image from a file, allocating a blank image, copying a
//! sub-rectangle out into a new image, adjusting an image's viewport, and the chromakey
//! (green/blue-screen) pipeline.
//!
//! [`Image`] is owned: its [`Drop`] calls `switch_img_free`. [`Chromakey`] is owned too:
//! its [`Drop`] calls `switch_chromakey_destroy`. Neither wrapper exposes raw pointers in
//! its public API.

use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::{Result, SwitchError, cstring, status_to_result, sys};

/// The image formats recognized by FreeSWITCH's image layer (`switch_img_fmt_t`, an alias of
/// libvpx's `vpx_img_fmt_t`).
///
/// Re-exported verbatim from the bindgen bindings so callers can name the value they need
/// (e.g. [`sys::vpx_img_fmt_VPX_IMG_FMT_I420`]) without re-importing `sys` themselves.
pub type ImageFormat = sys::switch_img_fmt_t;

/// An 8-bit-per-channel RGBA color, matching FreeSWITCH's `switch_rgb_color_t`.
///
/// The bindgen layout uses the little-endian byte order `{ b, g, r, a }`; the fields here mirror
/// that order so a value built in Rust has the same in-memory representation as one produced by C.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    /// Blue channel.
    pub b: u8,
    /// Green channel.
    pub g: u8,
    /// Red channel.
    pub r: u8,
    /// Alpha channel.
    pub a: u8,
}

impl Color {
    /// Builds a color from red, green, blue components with fully opaque alpha.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Builds a color from red, green, blue, alpha components.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    fn to_sys(self) -> sys::switch_rgb_color_t {
        sys::switch_rgb_color_s {
            b: self.b,
            g: self.g,
            r: self.r,
            a: self.a,
        }
    }
}

/// An owned FreeSWITCH video image (`switch_image_t`, a libvpx image descriptor).
///
/// Created by [`Image::read_file`] (auto-detects format) or [`Image::alloc`] (a blank image of
/// the requested format and dimensions). The wrapper frees the image on drop via
/// `switch_img_free`; pass it by value to transfer ownership, or use [`Image::as_ptr`] for an
/// escape hatch into FFI.
pub struct Image {
    raw: NonNull<sys::switch_image_t>,
    // Not thread-safe; image operations mutate pixel/C state.
    _marker: PhantomData<*const ()>,
}

impl Image {
    /// Wraps a FreeSWITCH image pointer that this wrapper will own and free on drop.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `switch_image_t` obtained from a `switch_img_*` allocator
    /// (e.g. `switch_img_alloc`, `switch_img_read_file`, `switch_img_copy_rect`), and the caller
    /// must transfer sole ownership to this wrapper — the image must not be freed elsewhere.
    pub unsafe fn from_raw(raw: *mut sys::switch_image_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw, _marker: PhantomData })
    }

    /// The raw image pointer (escape hatch for FFI). The wrapper retains ownership.
    pub fn as_ptr(&self) -> *mut sys::switch_image_t {
        self.raw.as_ptr()
    }

    /// Reads an image from a file, auto-detecting the format from the extension
    /// (PNG, JPG, BMP, TGA, HDR are supported per `switch_core_video.h`).
    ///
    /// Returns `Ok(None)` if the file cannot be read or decoded.
    pub fn read_file(path: impl AsRef<str>) -> Result<Option<Self>> {
        let path = cstring(path)?;
        // SAFETY: `path` is a valid C string for the call. The returned pointer is either a live,
        // owned image (to be freed via `switch_img_free`) or NULL on failure.
        let raw = unsafe { sys::switch_img_read_file(path.as_ptr()) };
        // SAFETY: `raw` is NULL or a freshly allocated image whose ownership transfers here.
        Ok(unsafe { Self::from_raw(raw) })
    }

    /// Allocates a blank image of the given format and dimensions. The pixel buffer is owned by
    /// the image and freed on drop.
    ///
    /// `fmt` is a `vpx_img_fmt` value (e.g. `sys::vpx_img_fmt_VPX_IMG_FMT_I420`). `align` is the
    /// row alignment in bytes (a multiple of 16 is optimal; pass `1` for packed rows).
    pub fn alloc(fmt: ImageFormat, width: u32, height: u32, align: u32) -> Result<Self> {
        // SAFETY: A null `img` argument requests a fresh allocation, which is the owned path we
        // want. The returned pointer is NULL on allocation failure.
        let raw = unsafe { sys::switch_img_alloc(std::ptr::null_mut(), fmt, width, height, align) };
        // SAFETY: `raw` is NULL on failure; otherwise it is a live owned image.
        unsafe { Self::from_raw(raw) }.ok_or(SwitchError(crate::GENERR))
    }

    /// Returns the image's width in pixels.
    pub fn width(&self) -> u32 {
        // SAFETY: `self.raw` is a live image; `d_w` is a plain field read.
        unsafe { (*self.raw.as_ptr()).d_w }
    }

    /// Returns the image's height in pixels.
    pub fn height(&self) -> u32 {
        // SAFETY: `self.raw` is a live image; `d_h` is a plain field read.
        unsafe { (*self.raw.as_ptr()).d_h }
    }

    /// Returns the image's pixel format.
    pub fn format(&self) -> ImageFormat {
        // SAFETY: `self.raw` is a live image; `fmt` is a plain field read.
        unsafe { (*self.raw.as_ptr()).fmt }
    }

    /// Adjusts the image's viewport — the visible rectangle of the underlying buffer — without
    /// copying pixel data. `x`/`y` are the top-left corner; `w`/`h` the extent. Returns
    /// `Err(GENERR)` if the rectangle lies outside the buffer.
    pub fn set_rect(&mut self, x: u32, y: u32, w: u32, h: u32) -> Result<()> {
        // SAFETY: `self.raw` is a live image. The vpx convention is that a non-zero return
        // indicates an invalid rectangle.
        let rc = unsafe { sys::switch_img_set_rect(self.raw.as_ptr(), x, y, w, h) };
        if rc == 0 {
            Ok(())
        } else {
            Err(SwitchError(crate::GENERR))
        }
    }

    /// Copies a rectangular region of this image into a newly allocated image and returns it.
    ///
    /// `x`/`y` is the top-left corner to read from; `w`/`h` the extent. Returns
    /// `Err(GENERR)` if the copy fails (e.g. the region is out of bounds or allocation fails).
    pub fn copy_rect(&self, x: u32, y: u32, w: u32, h: u32) -> Result<Self> {
        // SAFETY: `self.raw` is a live image. The returned pointer is NULL on failure, or a live,
        // owned image to be freed via `switch_img_free`.
        let raw = unsafe { sys::switch_img_copy_rect(self.raw.as_ptr(), x, y, w, h) };
        // SAFETY: `raw` is NULL on failure; otherwise it is a live owned image.
        unsafe { Self::from_raw(raw) }.ok_or(SwitchError(crate::GENERR))
    }

    /// Writes the image to a file. `quality` (1–100) applies to JPEG and is ignored by other
    /// formats (PNG, BMP, TGA, HDR).
    pub fn write_to_file(&self, path: impl AsRef<str>, quality: i32) -> Result<()> {
        let path = cstring(path)?;
        // SAFETY: `self.raw` is a live image; `path` is a valid C string for the call.
        let status = unsafe { sys::switch_img_write_to_file(self.raw.as_ptr(), path.as_ptr(), quality) };
        status_to_result(status)
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        let mut raw = self.raw.as_ptr();
        // SAFETY: `raw` is the live image this wrapper owns. `switch_img_free` nulls out `*raw`
        // after freeing, so a double drop is a no-op.
        unsafe { sys::switch_img_free(&mut raw) };
    }
}

/// An owned FreeSWITCH chromakey context (`switch_chromakey_t`).
///
/// A chromakey marks a set of colors as transparent and applies that mask to images via
/// [`Chromakey::process`]. Colors are added with [`Chromakey::add_color`]; the deep compositing
/// API is intentionally not wrapped here.
pub struct Chromakey {
    raw: NonNull<sys::switch_chromakey_t>,
    // Not thread-safe; `process`/`add_color` mutate C state.
    _marker: PhantomData<*const ()>,
}

impl Chromakey {
    /// Wraps a FreeSWITCH chromakey pointer that this wrapper will own and destroy on drop.
    ///
    /// # Safety
    ///
    /// `raw` must point to a live `switch_chromakey_t` obtained from `switch_chromakey_create`,
    /// and the caller must transfer sole ownership to this wrapper.
    pub unsafe fn from_raw(raw: *mut sys::switch_chromakey_t) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self { raw, _marker: PhantomData })
    }

    /// The raw chromakey pointer (escape hatch for FFI). The wrapper retains ownership.
    pub fn as_ptr(&self) -> *mut sys::switch_chromakey_t {
        self.raw.as_ptr()
    }

    /// Creates a new, empty chromakey context.
    pub fn new() -> Result<Self> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: `switch_chromakey_create` writes a live chromakey into `*ckP` on success.
        let status = unsafe { sys::switch_chromakey_create(&mut raw) };
        status_to_result(status)?;
        // SAFETY: On success `raw` is a live, owned chromakey; `from_raw` returns None only if the
        // FFI left it null despite reporting success, which we treat as a GENERR.
        unsafe { Self::from_raw(raw) }.ok_or(SwitchError(crate::GENERR))
    }

    /// Adds `color` to the set of transparent colors, with a per-color `threshold`
    /// (0–255; larger values widen the band of pixels treated as matching).
    pub fn add_color(&self, color: Color, threshold: u32) -> Result<()> {
        let mut color = color.to_sys();
        // SAFETY: `self.raw` is a live chromakey; `color` is a valid out-parameter for the call.
        let status = unsafe { sys::switch_chromakey_add_color(self.raw.as_ptr(), &mut color, threshold) };
        status_to_result(status)
    }

    /// Sets the default threshold applied to colors added without an explicit per-color value.
    pub fn set_default_threshold(&self, threshold: u32) {
        // SAFETY: `self.raw` is a live chromakey.
        unsafe { sys::switch_chromakey_set_default_threshold(self.raw.as_ptr(), threshold) };
    }

    /// Removes every color previously added with [`Chromakey::add_color`].
    pub fn clear_colors(&self) -> Result<()> {
        // SAFETY: `self.raw` is a live chromakey.
        let status = unsafe { sys::switch_chromakey_clear_colors(self.raw.as_ptr()) };
        status_to_result(status)
    }

    /// Applies the chromakey mask to `image` in place: pixels matching a registered color are
    /// made transparent (alpha set to 0).
    pub fn process(&self, image: &mut Image) {
        // SAFETY: Both pointers are live; `image` is borrowed mutably for the in-place edit.
        unsafe { sys::switch_chromakey_process(self.raw.as_ptr(), image.as_ptr()) };
    }
}

impl Drop for Chromakey {
    fn drop(&mut self) {
        let mut raw = self.raw.as_ptr();
        // SAFETY: `raw` is the live chromakey this wrapper owns. `switch_chromakey_destroy` nulls
        // `*ckP` after destroying, so a double drop is a no-op.
        let _ = unsafe { sys::switch_chromakey_destroy(&mut raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_rgb_is_opaque() {
        let c = Color::rgb(1, 2, 3);
        assert_eq!(c.a, 255);
        assert_eq!(c.r, 1);
        assert_eq!(c.g, 2);
        assert_eq!(c.b, 3);
    }

    #[test]
    fn color_to_sys_preserves_channels() {
        let c = Color::rgba(10, 20, 30, 40);
        let s = c.to_sys();
        assert_eq!((s.r, s.g, s.b, s.a), (10, 20, 30, 40));
    }
}
