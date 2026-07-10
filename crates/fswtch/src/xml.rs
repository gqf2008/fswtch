use std::{ffi::CStr, marker::PhantomData, ptr::NonNull};

use crate::{cstring, sys};

pub struct XmlConfig {
    root: NonNull<sys::switch_xml>,
    settings: Option<NonNull<sys::switch_xml>>,
}

impl XmlConfig {
    pub fn open(file: impl AsRef<str>) -> Option<Self> {
        let file = cstring(file).ok()?;
        let mut settings = std::ptr::null_mut();
        // SAFETY: FreeSWITCH writes the configuration node into `settings` when the file is found.
        let root =
            unsafe { sys::switch_xml_open_cfg(file.as_ptr(), &mut settings, std::ptr::null_mut()) };
        let root = NonNull::new(root)?;

        Some(Self {
            root,
            settings: NonNull::new(settings),
        })
    }

    pub fn settings(&self) -> Option<XmlNode<'_>> {
        self.settings.map(XmlNode::new)
    }
}

impl Drop for XmlConfig {
    fn drop(&mut self) {
        // SAFETY: `root` was returned by FreeSWITCH XML APIs and is owned by this wrapper.
        unsafe {
            sys::switch_xml_free(self.root.as_ptr());
        }
    }
}

#[derive(Copy, Clone)]
pub struct XmlNode<'a> {
    raw: NonNull<sys::switch_xml>,
    _owner: PhantomData<&'a XmlConfig>,
}

impl<'a> XmlNode<'a> {
    fn new(raw: NonNull<sys::switch_xml>) -> Self {
        Self {
            raw,
            _owner: PhantomData,
        }
    }

    pub fn child(self, name: impl AsRef<str>) -> Option<Self> {
        let name = cstring(name).ok()?;
        // SAFETY: `self.raw` is live and `name` is valid for the duration of this call.
        let child = unsafe { sys::switch_xml_child(self.raw.as_ptr(), name.as_ptr()) };
        NonNull::new(child).map(Self::new)
    }

    pub fn next(self) -> Option<Self> {
        // SAFETY: `self.raw` is a live XML node for traversal.
        NonNull::new(unsafe { (*self.raw.as_ptr()).next }).map(Self::new)
    }

    pub fn attr(self, name: impl AsRef<str>) -> Option<String> {
        let name = cstring(name).ok()?;
        // SAFETY: `self.raw` is live and `name` is valid for the duration of this call.
        let value = unsafe { sys::switch_xml_attr(self.raw.as_ptr(), name.as_ptr()) };
        if value.is_null() {
            return None;
        }

        // SAFETY: FreeSWITCH returns a null-terminated attribute value when present.
        unsafe { CStr::from_ptr(value) }
            .to_str()
            .ok()
            .map(ToOwned::to_owned)
    }

    /// Returns the tag name of this node (e.g. "param", "settings").
    pub fn name(self) -> Option<String> {
        // SAFETY: `self.raw` is a live XML node; `name` is a null-terminated tag.
        let name_ptr = unsafe { (*self.raw.as_ptr()).name };
        if name_ptr.is_null() {
            return None;
        }
        // SAFETY: FreeSWITCH guarantees `name` is null-terminated when present.
        unsafe { CStr::from_ptr(name_ptr) }
            .to_str()
            .ok()
            .map(ToOwned::to_owned)
    }

    // ── write / lookup operations ──────────────────────────────────────────

    /// Adds a child element named `name` at `off` (pass `0` for default). Returns the new node
    /// borrowing the same lifetime (it is owned by the parent tree).
    pub fn add_child(self, name: impl AsRef<str>, off: u64) -> Option<Self> {
        let name = cstring(name).ok()?;
        // SAFETY: `self.raw` live; `name` valid C string; `off` plain size.
        let child = unsafe {
            sys::switch_xml_add_child(self.raw.as_ptr(), name.as_ptr(), off as sys::switch_size_t)
        };
        NonNull::new(child).map(Self::new)
    }

    /// Like [`add_child`](Self::add_child) but the child is a "deep" copy (with its own
    /// allocation, freed when the tree is freed).
    pub fn add_child_d(self, name: impl AsRef<str>, off: u64) -> Option<Self> {
        let name = cstring(name).ok()?;
        // SAFETY: as above.
        let child = unsafe {
            sys::switch_xml_add_child_d(self.raw.as_ptr(), name.as_ptr(), off as sys::switch_size_t)
        };
        NonNull::new(child).map(Self::new)
    }

    /// Cuts (detaches) this node from its parent tree. Returns the detached node — it is now
    /// **owned by the caller** and must be freed via [`xml_free`].
    pub fn cut(self) -> Option<Self> {
        // SAFETY: `self.raw` live.
        let cut = unsafe { sys::switch_xml_cut(self.raw.as_ptr()) };
        NonNull::new(cut).map(Self::new)
    }

    /// Duplicates this node. Returns an **owned** copy — free via [`xml_free`].
    pub fn dup(self) -> Option<Self> {
        // SAFETY: `self.raw` live.
        let dup = unsafe { sys::switch_xml_dup(self.raw.as_ptr()) };
        NonNull::new(dup).map(Self::new)
    }

    /// Finds a descendant by `childname` whose `attrname` matches `value`. Returns the match or
    /// `None`.
    pub fn find_child(
        self,
        childname: impl AsRef<str>,
        attrname: impl AsRef<str>,
        value: impl AsRef<str>,
    ) -> Option<Self> {
        let cn = cstring(childname).ok()?;
        let an = cstring(attrname).ok()?;
        let v = cstring(value).ok()?;
        // SAFETY: `self.raw` live; three valid C strings.
        let found = unsafe {
            sys::switch_xml_find_child(self.raw.as_ptr(), cn.as_ptr(), an.as_ptr(), v.as_ptr())
        };
        NonNull::new(found).map(Self::new)
    }

    /// Returns the `idx`-th child of this node.
    pub fn idx(self, index: i32) -> Option<Self> {
        // SAFETY: `self.raw` live; plain int.
        let child = unsafe { sys::switch_xml_idx(self.raw.as_ptr(), index) };
        NonNull::new(child).map(Self::new)
    }

    /// Inserts this node into `dest` at `off`. Returns the inserted node.
    pub fn insert(self, dest: Self, off: u64) -> Option<Self> {
        // SAFETY: both nodes live; plain size.
        let ins = unsafe {
            sys::switch_xml_insert(
                self.raw.as_ptr(),
                dest.raw.as_ptr(),
                off as sys::switch_size_t,
            )
        };
        NonNull::new(ins).map(Self::new)
    }

    /// The last XML parse error for this node (borrowed static-ish string).
    pub fn error(self) -> Option<String> {
        // SAFETY: `self.raw` live; returns null or a C string.
        let ptr = unsafe { sys::switch_xml_error(self.raw.as_ptr()) };
        if ptr.is_null() {
            return None;
        }
        // SAFETY: null or a C string.
        unsafe { CStr::from_ptr(ptr) }
            .to_str()
            .ok()
            .map(ToOwned::to_owned)
    }

    /// Like [`attr`](Self::attr) but returns an empty string instead of `None` when the attribute
    /// is absent.
    pub fn attr_soft(self, attr: impl AsRef<str>) -> String {
        let attr = match cstring(attr) {
            Ok(s) => s,
            Err(_) => return String::new(),
        };
        // SAFETY: `self.raw` live; valid C string.
        let ptr = unsafe { sys::switch_xml_attr_soft(self.raw.as_ptr(), attr.as_ptr()) };
        if ptr.is_null() {
            return String::new();
        }
        // SAFETY: null or a C string (never null in practice — returns "" for absent).
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Allocates a new standalone XML node named `name`. The node is **owned by the caller** — free
/// via [`xml_free`].
pub fn xml_new(name: impl AsRef<str>) -> Option<XmlNode<'static>> {
    let name = cstring(name).ok()?;
    // SAFETY: `name` valid C string; returns null or a new owned XML node.
    let node = unsafe { sys::switch_xml_new(name.as_ptr()) };
    NonNull::new(node).map(|raw| XmlNode {
        raw,
        _owner: PhantomData,
    })
}

/// Frees an XML node allocated by [`xml_new`], [`XmlNode::cut`], or [`XmlNode::dup`]. Nodes that
/// remain attached to a tree owned by [`XmlConfig`] are freed automatically on its drop — do not
/// call this on those.
pub fn xml_free(node: &XmlNode<'_>) {
    // SAFETY: `node.raw` was obtained from a `switch_xml_*` allocator and is freed exactly once.
    unsafe { sys::switch_xml_free(node.raw.as_ptr()) };
}
