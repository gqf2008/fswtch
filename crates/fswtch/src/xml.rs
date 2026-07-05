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
}
