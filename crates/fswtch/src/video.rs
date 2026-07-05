//! Video image helpers and chromakey.
//!
//! Wraps the commonly-used subset of FreeSWITCH's video API (`switch_core_video.h` /
//! `switch_image.h`): decoding an image from a file, allocating a blank image, copying a
//! sub-rectangle out into a new image, adjusting an image's viewport, in-place pixel ops
//! (fill / grayscale / attenuate / fit-to-size), rendering an image to a `data:` URL, wrapping
//! raw bytes into an image, and the chromakey (green/blue-screen) pipeline.
//!
//! [`Image`] is owned: its [`Drop`] calls `switch_img_free`. [`Chromakey`] is owned too:
//! its [`Drop`] calls `switch_chromakey_destroy`. Neither wrapper exposes raw pointers in
//! its public API.

use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::{Result, SwitchError, cstring, status_to_result, strdup_to_string, sys};

/// The image format used by FreeSWITCH's image layer (`switch_img_fmt_t`, an alias of
/// libvpx's `vpx_img_fmt_t`).
///
/// Newtype wrapper so callers cannot mix it with other `u32` flags. Only the formats
/// FreeSWITCH's image layer actually produces are exposed as associated constants;
/// `from_raw` covers any other `vpx_img_fmt_*` value.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ImageFormat(pub sys::switch_img_fmt_t);

impl ImageFormat {
    /// No format (sentinel for an uninitialised image).
    pub const NONE: Self = Self(sys::vpx_img_fmt_VPX_IMG_FMT_NONE);
    /// 8-bit YUV 4:2:0 — the format FreeSWITCH's video layer produces for H.26x codecs.
    pub const I420: Self = Self(sys::vpx_img_fmt_VPX_IMG_FMT_I420);
    /// 16-bit YUV 4:2:0.
    pub const I42016: Self = Self(sys::vpx_img_fmt_VPX_IMG_FMT_I42016);

    /// The raw `switch_img_fmt_t` value, for FFI.
    #[inline]
    pub const fn raw(self) -> sys::switch_img_fmt_t {
        self.0
    }

    /// Wraps a raw format value (e.g. any `sys::vpx_img_fmt_*` constant).
    #[inline]
    pub const fn from_raw(v: sys::switch_img_fmt_t) -> Self {
        Self(v)
    }
}

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

    /// Parses a FreeSWITCH color string (e.g. `"#rrggbb"`, `"#rrggbbaa"`, or a named color) via
    /// `switch_color_set_rgb` and returns the resulting color.
    ///
    /// Returns [`crate::SwitchError`](`crate::GENERR`) only when the string contains an interior
    /// NUL byte. FreeSWITCH's parser has no failure status: an unparseable string leaves the color
    /// at its zero/default value, which is returned as-is — callers that care should validate the
    /// result against the input.
    pub fn from_rgb_str(s: impl AsRef<str>) -> Result<Self> {
        let s = cstring(s)?;
        let mut color = sys::switch_rgb_color_s::default();
        // SAFETY: `s` is a valid null-terminated C string for the call; `color` is writable
        // out-storage for the parsed RGB value.
        unsafe { sys::switch_color_set_rgb(&mut color, s.as_ptr()) };
        Ok(Self {
            b: color.b,
            g: color.g,
            r: color.r,
            a: color.a,
        })
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

/// The fit strategy applied by [`Image::fit`] (`switch_img_fit_t`).
///
/// Controls how an image is rescaled to a target width/height: scale to fill, scale preserving
/// aspect ratio, scale only if the target is smaller, or leave the image untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageFit(pub sys::switch_img_fit_t);

impl ImageFit {
    /// Scale to the exact target dimensions (aspect ratio may change).
    pub const SIZE: Self = Self(sys::switch_img_fit_t_SWITCH_FIT_SIZE);

    /// Scale preserving aspect ratio (`SWITCH_FIT_SCALE`).
    pub const SCALE: Self = Self(sys::switch_img_fit_t_SWITCH_FIT_SCALE);

    /// Scale to fit within the target while preserving aspect ratio
    /// (`SWITCH_FIT_SIZE_AND_SCALE`).
    pub const SIZE_AND_SCALE: Self = Self(sys::switch_img_fit_t_SWITCH_FIT_SIZE_AND_SCALE);

    /// Scale only if the image is larger than the target (`SWITCH_FIT_NECESSARY`).
    pub const NECESSARY: Self = Self(sys::switch_img_fit_t_SWITCH_FIT_NECESSARY);

    /// Do not scale (`SWITCH_FIT_NONE`).
    pub const NONE: Self = Self(sys::switch_img_fit_t_SWITCH_FIT_NONE);

    /// Wraps a raw `switch_img_fit_t` returned from FFI.
    #[inline]
    pub const fn from_raw(fit: sys::switch_img_fit_t) -> Self {
        Self(fit)
    }

    /// The underlying integer value.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// The anchor position computed by [`Image::find_position`] (`switch_img_position_t`).
///
/// Identifies one of nine anchor points within a larger surface (left/center/right ×
/// top/mid/bottom), or "no position".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImagePosition(pub sys::switch_img_position_t);

impl ImagePosition {
    /// Top-left corner (`POS_LEFT_TOP`).
    pub const LEFT_TOP: Self = Self(sys::switch_img_position_t_POS_LEFT_TOP);

    /// Middle-left (`POS_LEFT_MID`).
    pub const LEFT_MID: Self = Self(sys::switch_img_position_t_POS_LEFT_MID);

    /// Bottom-left (`POS_LEFT_BOT`).
    pub const LEFT_BOT: Self = Self(sys::switch_img_position_t_POS_LEFT_BOT);

    /// Top-center (`POS_CENTER_TOP`).
    pub const CENTER_TOP: Self = Self(sys::switch_img_position_t_POS_CENTER_TOP);

    /// Dead center (`POS_CENTER_MID`).
    pub const CENTER_MID: Self = Self(sys::switch_img_position_t_POS_CENTER_MID);

    /// Bottom-center (`POS_CENTER_BOT`).
    pub const CENTER_BOT: Self = Self(sys::switch_img_position_t_POS_CENTER_BOT);

    /// Top-right (`POS_RIGHT_TOP`).
    pub const RIGHT_TOP: Self = Self(sys::switch_img_position_t_POS_RIGHT_TOP);

    /// Middle-right (`POS_RIGHT_MID`).
    pub const RIGHT_MID: Self = Self(sys::switch_img_position_t_POS_RIGHT_MID);

    /// Bottom-right (`POS_RIGHT_BOT`).
    pub const RIGHT_BOT: Self = Self(sys::switch_img_position_t_POS_RIGHT_BOT);

    /// No positioning (`POS_NONE`).
    pub const NONE: Self = Self(sys::switch_img_position_t_POS_NONE);

    /// Wraps a raw `switch_img_position_t` returned from FFI.
    #[inline]
    pub const fn from_raw(pos: sys::switch_img_position_t) -> Self {
        Self(pos)
    }

    /// The underlying integer value.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// A color-family hint passed to [`Chromakey::autocolor`] (`switch_shade_t`).
///
/// Selects which channel (red/green/blue) the chromakey should treat as the "dominant" color
/// when auto-detecting a mask, or `Auto` to let FreeSWITCH pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shade(pub sys::switch_shade_t);

impl Shade {
    /// No explicit shade — disable autocolor (`SWITCH_SHADE_NONE`).
    pub const NONE: Self = Self(sys::switch_shade_t_SWITCH_SHADE_NONE);

    /// Treat red as the dominant color (`SWITCH_SHADE_RED`).
    pub const RED: Self = Self(sys::switch_shade_t_SWITCH_SHADE_RED);

    /// Treat green as the dominant color (`SWITCH_SHADE_GREEN`).
    pub const GREEN: Self = Self(sys::switch_shade_t_SWITCH_SHADE_GREEN);

    /// Treat blue as the dominant color (`SWITCH_SHADE_BLUE`).
    pub const BLUE: Self = Self(sys::switch_shade_t_SWITCH_SHADE_BLUE);

    /// Let FreeSWITCH auto-detect the dominant color (`SWITCH_SHADE_AUTO`).
    pub const AUTO: Self = Self(sys::switch_shade_t_SWITCH_SHADE_AUTO);

    /// Wraps a raw `switch_shade_t` returned from FFI.
    #[inline]
    pub const fn from_raw(shade: sys::switch_shade_t) -> Self {
        Self(shade)
    }

    /// The underlying integer value.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}
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
        NonNull::new(raw).map(|raw| Self {
            raw,
            _marker: PhantomData,
        })
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
        let raw =
            unsafe { sys::switch_img_alloc(std::ptr::null_mut(), fmt.raw(), width, height, align) };
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
        unsafe { ImageFormat::from_raw((*self.raw.as_ptr()).fmt) }
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

    /// Fills the rectangle (`x`,`y`,`w`,`h`) of this image with `color` in place, honoring the
    /// color's alpha channel. `x`/`y`/`w`/`h` are in pixels. Out-of-bounds rectangles are clamped
    /// by FreeSWITCH rather than reported as errors, so this method always succeeds.
    pub fn fill(&self, x: i32, y: i32, w: i32, h: i32, color: Color) {
        let mut color = color.to_sys();
        // SAFETY: `self.raw` is a live image; `color` is valid writable out-storage for the call.
        unsafe { sys::switch_img_fill(self.raw.as_ptr(), x, y, w, h, &mut color) };
    }

    /// Like [`Image::fill`] but forces the alpha channel of the filled pixels to fully opaque,
    /// overwriting any existing transparency within the rectangle.
    pub fn fill_noalpha(&self, x: i32, y: i32, w: i32, h: i32, color: Color) {
        let mut color = color.to_sys();
        // SAFETY: `self.raw` is a live image; `color` is valid writable out-storage for the call.
        unsafe { sys::switch_img_fill_noalpha(self.raw.as_ptr(), x, y, w, h, &mut color) };
    }

    /// Converts the rectangle (`x`,`y`,`w`,`h`) of this image to grayscale in place. Pass `0, 0, 0,
    /// 0` to grayscale the whole image (FreeSWITCH treats a zero extent as "full image").
    pub fn gray(&self, x: i32, y: i32, w: i32, h: i32) {
        // SAFETY: `self.raw` is a live image.
        unsafe { sys::switch_img_gray(self.raw.as_ptr(), x, y, w, h) };
    }

    /// Attenuates the image's pixels in place (FreeSWITCH's `switch_img_attenuate`). A quick
    /// dimming helper with no parameters; see the header for the exact per-channel effect.
    pub fn attenuate(&self) {
        // SAFETY: `self.raw` is a live image.
        unsafe { sys::switch_img_attenuate(self.raw.as_ptr()) };
    }

    /// Scales this image in place to `width`×`height` according to `fit`.
    ///
    /// `switch_img_fit` may free the existing image and replace it with a freshly allocated scaled
    /// one; this method transparently adopts the new pointer so the wrapper stays sound. On
    /// failure the original image is left untouched. Returns `Err(GENERR)` if the resize fails or
    /// `width`/`height` are zero (FreeSWITCH asserts on zero dimensions).
    pub fn fit(&mut self, width: u32, height: u32, fit: ImageFit) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(SwitchError(crate::GENERR));
        }
        let mut raw = self.raw.as_ptr();
        // SAFETY: `raw` is a live owned image. On success FreeSWITCH frees the old image and writes
        // the new (scaled) image pointer back into `*raw`. The failure-path ownership of `*srcP`
        // is undocumented in the header, so we treat the post-call value of `raw` as the only
        // authoritative pointer: if `NonNull::new` rejects it (null on success, or freed on failure),
        // we must not keep a dangling `self.raw` — the image is gone either way.
        let status = unsafe { sys::switch_img_fit(&mut raw, width as i32, height as i32, fit.0) };
        if status_to_result(status).is_ok() {
            // On success the contract guarantees a non-null replacement; if it is null (allocation
            // of the scaled image failed late), the old image was already freed, so `self.raw`
            // must be invalidated regardless.
            match NonNull::new(raw) {
                Some(new_raw) => {
                    self.raw = new_raw;
                    Ok(())
                }
                None => Err(SwitchError(crate::GENERR)),
            }
        } else {
            // Failure path: `*srcP` may be unchanged OR freed-and-nulled (undocumented). If `raw`
            // survived, keep using it; if FreeSWITCH nulled it, the image is gone — surface an
            // error and mark the handle invalid so a subsequent Drop does not free a dangling
            // pointer.
            match NonNull::new(raw) {
                Some(_) => Err(SwitchError(crate::GENERR)),
                None => {
                    // The image was freed by FreeSWITCH on the failure path; leak the `NonNull`
                    // by replacing it with a dangling marker that Drop will no-op. We cannot
                    // soundly call `switch_img_free` on freed memory, so we detach ownership.
                    self.raw = NonNull::dangling();
                    Err(SwitchError(crate::GENERR))
                }
            }
        }
    }

    /// Computes the largest dimensions that fit within `width`×`height` while preserving the
    /// image's aspect ratio. Returns `(new_w, new_h)`.
    ///
    /// Unlike [`Image::fit`], this does not modify the image — it only reports the computed
    /// dimensions.
    pub fn calc_fit(&self, width: u32, height: u32) -> (i32, i32) {
        let mut new_w: i32 = 0;
        let mut new_h: i32 = 0;
        // SAFETY: `self.raw` is a live image; `new_w`/`new_h` are valid writable out-storage.
        unsafe {
            sys::switch_img_calc_fit(
                self.raw.as_ptr(),
                width as i32,
                height as i32,
                &mut new_w,
                &mut new_h,
            )
        };
        (new_w, new_h)
    }

    /// Computes the top-left `(x, y)` at which an image of size `image_w`×`image_h` should be
    /// placed within a surface of `surface_w`×`surface_h` so that it sits at anchor `pos`.
    ///
    /// Pure math helper (no image access); included for convenience alongside [`Image::fit`].
    pub fn find_position(
        pos: ImagePosition,
        surface_w: i32,
        surface_h: i32,
        image_w: i32,
        image_h: i32,
    ) -> (i32, i32) {
        let mut x: i32 = 0;
        let mut y: i32 = 0;
        // SAFETY: a pure computation; `x`/`y` are valid writable out-storage.
        unsafe {
            sys::switch_img_find_position(
                pos.0, surface_w, surface_h, image_w, image_h, &mut x, &mut y,
            )
        };
        (x, y)
    }

    /// Renders this image to a `data:` URL string (base64-encoded, with the
    /// `data:image/<type>;base64,` prefix).
    ///
    /// `mime` selects the format: `"png"` or `"jpeg"`. `quality` (1–100) applies to JPEG and is
    /// ignored by PNG. Returns an owned [`String`] copied out of the C-allocated URL buffer, which
    /// is freed via `free` after copying.
    pub fn data_url(&self, mime: impl AsRef<str>, quality: i32) -> Result<String> {
        let mime = cstring(mime)?;
        let mut url: *mut std::os::raw::c_char = std::ptr::null_mut();
        // SAFETY: `self.raw` is a live image; `mime` is a valid null-terminated C string; `url` is
        // valid writable out-storage. On success `*url` is a malloc-allocated string the caller
        // must free; on failure it is left NULL.
        let status = unsafe {
            sys::switch_img_data_url(self.raw.as_ptr(), &mut url, mime.as_ptr(), quality)
        };
        status_to_result(status)?;
        // SAFETY: on success `url` is a malloc-allocated null-terminated string owned by us.
        unsafe { strdup_to_string(url) }.ok_or(SwitchError(crate::GENERR))
    }

    /// Renders this image to a PNG `data:` URL string. Convenience over [`Image::data_url`] for
    /// the common PNG case.
    pub fn data_url_png(&self) -> Result<String> {
        let mut url: *mut std::os::raw::c_char = std::ptr::null_mut();
        // SAFETY: `self.raw` is a live image; `url` is valid writable out-storage. On success
        // `*url` is a malloc-allocated string the caller must free; on failure it is left NULL.
        let status = unsafe { sys::switch_img_data_url_png(self.raw.as_ptr(), &mut url) };
        status_to_result(status)?;
        // SAFETY: on success `url` is a malloc-allocated null-terminated string owned by us.
        unsafe { strdup_to_string(url) }.ok_or(SwitchError(crate::GENERR))
    }

    /// Wraps a raw byte buffer as a new owned image, interpreting it as `fmt` at `width`×`height`.
    ///
    /// The pixel bytes are copied into the image's storage. `switch_img_from_raw` assumes
    /// contiguous rows; pass a buffer with no row padding.
    pub fn from_raw_bytes(data: &[u8], fmt: ImageFormat, width: u32, height: u32) -> Result<Self> {
        let mut raw: *mut sys::switch_image_t = std::ptr::null_mut();
        // SAFETY: `data` is a valid readable buffer of `data.len()` bytes; `raw` is valid
        // writable out-storage. On success `*raw` is a live owned image to be freed via
        // `switch_img_free`.
        let status = unsafe {
            sys::switch_img_from_raw(
                &mut raw,
                data.as_ptr().cast_mut().cast(),
                fmt.raw(),
                width as i32,
                height as i32,
            )
        };
        status_to_result(status)?;
        // SAFETY: on success `raw` is a live owned image.
        unsafe { Self::from_raw(raw) }.ok_or(SwitchError(crate::GENERR))
    }

    /// Writes the image to a file. `quality` (1–100) applies to JPEG and is ignored by other
    /// formats (PNG, BMP, TGA, HDR).
    pub fn write_to_file(&self, path: impl AsRef<str>, quality: i32) -> Result<()> {
        let path = cstring(path)?;
        // SAFETY: `self.raw` is a live image; `path` is a valid C string for the call.
        let status =
            unsafe { sys::switch_img_write_to_file(self.raw.as_ptr(), path.as_ptr(), quality) };
        status_to_result(status)
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        // `fit` may detach ownership by setting `self.raw` to `NonNull::dangling()` when
        // FreeSWITCH freed the image on a failure path — do not call `switch_img_free` on the
        // dangling sentinel (it is not a real allocation).
        if self.raw == NonNull::dangling() {
            return;
        }
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
        NonNull::new(raw).map(|raw| Self {
            raw,
            _marker: PhantomData,
        })
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
    ///
    /// Takes `&mut self` because mutating the color set can invalidate the cached image returned
    /// by [`Chromakey::cache_image`]; the exclusive borrow ensures no live [`CachedImage`] can
    /// outlive the change.
    pub fn add_color(&mut self, color: Color, threshold: u32) -> Result<()> {
        let mut color = color.to_sys();
        // SAFETY: `self.raw` is a live chromakey; `color` is a valid out-parameter for the call.
        let status =
            unsafe { sys::switch_chromakey_add_color(self.raw.as_ptr(), &mut color, threshold) };
        status_to_result(status)
    }

    /// Sets the default threshold applied to colors added without an explicit per-color value.
    pub fn set_default_threshold(&mut self, threshold: u32) {
        // SAFETY: `self.raw` is a live chromakey.
        unsafe { sys::switch_chromakey_set_default_threshold(self.raw.as_ptr(), threshold) };
    }

    /// Removes every color previously added with [`Chromakey::add_color`].
    pub fn clear_colors(&mut self) -> Result<()> {
        // SAFETY: `self.raw` is a live chromakey.
        let status = unsafe { sys::switch_chromakey_clear_colors(self.raw.as_ptr()) };
        status_to_result(status)
    }

    /// Applies the chromakey mask to `image` in place: pixels matching a registered color are
    /// made transparent (alpha set to 0).
    ///
    /// Takes `&mut self` because processing refreshes the chromakey's cached image, which would
    /// invalidate any live [`CachedImage`] view.
    pub fn process(&mut self, image: &mut Image) {
        // SAFETY: Both pointers are live; `image` is borrowed mutably for the in-place edit.
        unsafe { sys::switch_chromakey_process(self.raw.as_ptr(), image.as_ptr()) };
    }

    /// Registers an auto-detected mask for `shade` with the given `threshold` (0–255; larger
    /// values widen the band of pixels treated as matching).
    ///
    /// Pass [`Shade::AUTO`] to let FreeSWITCH pick the dominant color, or a specific shade to
    /// bias the detector toward red/green/blue.
    pub fn autocolor(&mut self, shade: Shade, threshold: u32) -> Result<()> {
        // SAFETY: `self.raw` is a live chromakey; `shade`/`threshold` are plain integers.
        let status =
            unsafe { sys::switch_chromakey_autocolor(self.raw.as_ptr(), shade.0, threshold) };
        status_to_result(status)
    }

    /// Returns the most recently processed image cached inside the chromakey, borrowed for the
    /// duration of `&self`.
    ///
    /// `switch_chromakey_cache_image` returns a pointer that the chromakey continues to own, so
    /// the returned [`CachedImage`] does not free it on drop — it is a non-owning view tied to
    /// this chromakey's lifetime. Returns `None` before [`Chromakey::process`] has cached an
    /// image.
    pub fn cache_image(&self) -> Option<CachedImage<'_>> {
        // SAFETY: `self.raw` is a live chromakey. The returned pointer is NULL when no image is
        // cached, or a live image owned by the chromakey for the duration of `&self`.
        let raw = unsafe { sys::switch_chromakey_cache_image(self.raw.as_ptr()) };
        NonNull::new(raw).map(|raw| CachedImage {
            raw,
            _marker: PhantomData,
        })
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

/// A borrowed, non-owning view of the image a [`Chromakey`] has cached from its most recent
/// [`Chromakey::process`](Chromakey::process) call.
///
/// Obtained via [`Chromakey::cache_image`]. The underlying `switch_image_t` is owned by the
/// chromakey, so [`CachedImage`] does **not** free it on drop — it is a view tied to the
/// borrowing chromakey's lifetime. There is no public way to mutate the cached image; use the
/// accessors to read its dimensions and format, or [`CachedImage::as_ptr`] as an FFI escape
/// hatch.
pub struct CachedImage<'a> {
    raw: NonNull<sys::switch_image_t>,
    // Tied to the borrowing chromakey's lifetime, and not thread-safe (shared C state). A raw
    // pointer to a reference carries the lifetime and is `!Send + !Sync`.
    _marker: PhantomData<*const &'a ()>,
}

impl CachedImage<'_> {
    /// The raw image pointer (escape hatch for FFI). The chromakey retains ownership.
    pub fn as_ptr(&self) -> *mut sys::switch_image_t {
        self.raw.as_ptr()
    }

    /// The cached image's width in pixels.
    pub fn width(&self) -> u32 {
        // SAFETY: `self.raw` is a live image; `d_w` is a plain field read.
        unsafe { (*self.raw.as_ptr()).d_w }
    }

    /// The cached image's height in pixels.
    pub fn height(&self) -> u32 {
        // SAFETY: `self.raw` is a live image; `d_h` is a plain field read.
        unsafe { (*self.raw.as_ptr()).d_h }
    }

    /// The cached image's pixel format.
    pub fn format(&self) -> ImageFormat {
        // SAFETY: `self.raw` is a live image; `fmt` is a plain field read.
        unsafe { ImageFormat::from_raw((*self.raw.as_ptr()).fmt) }
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

    #[test]
    fn image_fit_variants_match_sys() {
        assert_eq!(ImageFit::SIZE.0, sys::switch_img_fit_t_SWITCH_FIT_SIZE);
        assert_eq!(ImageFit::SCALE.0, sys::switch_img_fit_t_SWITCH_FIT_SCALE);
        assert_eq!(
            ImageFit::SIZE_AND_SCALE.0,
            sys::switch_img_fit_t_SWITCH_FIT_SIZE_AND_SCALE
        );
    }

    #[test]
    fn image_position_variants_match_sys() {
        assert_eq!(
            ImagePosition::CENTER_MID.0,
            sys::switch_img_position_t_POS_CENTER_MID
        );
        assert_eq!(ImagePosition::NONE.0, sys::switch_img_position_t_POS_NONE);
    }

    #[test]
    fn shade_variants_match_sys() {
        assert_eq!(Shade::AUTO.0, sys::switch_shade_t_SWITCH_SHADE_AUTO);
        assert_eq!(Shade::GREEN.0, sys::switch_shade_t_SWITCH_SHADE_GREEN);
    }
}

#[cfg(all(test, feature = "live_fs"))]
mod live_tests {
    use super::*;

    #[test]
    fn find_position_centers_a_quarter_image() {
        // A 2x2 image inside a 4x4 surface, centered, should sit at (1, 1).
        let (x, y) = Image::find_position(ImagePosition::CENTER_MID, 4, 4, 2, 2);
        assert_eq!((x, y), (1, 1));
    }

    #[test]
    fn find_position_top_left_is_origin() {
        let (x, y) = Image::find_position(ImagePosition::LEFT_TOP, 4, 4, 2, 2);
        assert_eq!((x, y), (0, 0));
    }
}
