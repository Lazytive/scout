//! Linux (Wayland): the overlay as a layer-shell surface. Wayland lets no app place its
//! own windows, so a normal window is put where the compositor likes (KDE: the middle of
//! the screen). The layer-shell protocol (KDE Plasma, wlroots desktops; not GNOME) lets a
//! surface sit in a corner of an output, above fullscreen windows, without taking the
//! keyboard. GTK 3 speaks it through gtk-layer-shell (LGPL-3.0-or-later and MIT), which
//! is loaded at run time when installed (`libgtk-layer-shell.so.0`): without it, or on a
//! compositor without the protocol, the overlay stays a normal window.
//!
//! The window is found by its title in GTK's list of toplevels, so callers hand over no
//! pointers. Every function must run on the GTK main thread, which is checked (the
//! default main context's owner); elsewhere they return an error.

use crate::PlatformError;

/// Where the overlay sits on its output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayAnchor {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// No edge: the compositor centres it.
    Center,
}

impl OverlayAnchor {
    /// From the overlay setting (`top-left`, `top-right`, `bottom-left`, `bottom-right`;
    /// anything else is top right).
    pub fn from_corner(corner: &str) -> OverlayAnchor {
        match corner {
            "top-left" => OverlayAnchor::TopLeft,
            "bottom-left" => OverlayAnchor::BottomLeft,
            "bottom-right" => OverlayAnchor::BottomRight,
            _ => OverlayAnchor::TopRight,
        }
    }

    /// (left, right, top, bottom) edges anchored.
    #[cfg(any(target_os = "linux", test))]
    fn edges(self) -> [bool; 4] {
        match self {
            OverlayAnchor::TopLeft => [true, false, true, false],
            OverlayAnchor::TopRight => [false, true, true, false],
            OverlayAnchor::BottomLeft => [true, false, false, true],
            OverlayAnchor::BottomRight => [false, true, false, true],
            OverlayAnchor::Center => [false; 4],
        }
    }
}

/// Make the window titled `title` a layer-shell surface on the overlay layer that never
/// takes the keyboard. Call on the GTK main thread before the window is first shown.
/// `Ok(false)`: gtk-layer-shell isn't installed or the compositor lacks the protocol, and
/// the window stays a normal one.
pub fn overlay_layer_init(title: &str) -> Result<bool, PlatformError> {
    #[cfg(target_os = "linux")]
    {
        imp::init(title)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = title;
        Ok(false)
    }
}

/// Size the layer-shell overlay titled `title` (see [`overlay_layer_init`]) to `size`
/// logical pixels and put it at `anchor`, `margin` logical pixels from the anchored
/// edges; `keyboard`: take the keyboard while shown (the quick menu). Call on the GTK
/// main thread. `Ok(false)`: not a layer surface.
pub fn overlay_layer_place(
    title: &str,
    size: (i32, i32),
    anchor: OverlayAnchor,
    margin: i32,
    keyboard: bool,
) -> Result<bool, PlatformError> {
    #[cfg(target_os = "linux")]
    {
        imp::place(title, size, anchor, margin, keyboard)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (title, size, anchor, margin, keyboard);
        Ok(false)
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)] // Calls into GTK and gtk-layer-shell; each block says why it is sound.
mod imp {
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::sync::OnceLock;

    use libloading::Library;

    use super::OverlayAnchor;
    use crate::PlatformError;

    type GBoolean = c_int;

    /// GLib's `GList` node.
    #[repr(C)]
    struct GList {
        data: *mut c_void,
        next: *mut GList,
        prev: *mut GList,
    }

    // gtk-layer-shell's enums (gtk-layer-shell.h, 0.6 and later).
    const LAYER_OVERLAY: c_int = 3;
    const KEYBOARD_NONE: c_int = 0;
    const KEYBOARD_EXCLUSIVE: c_int = 1;
    /// `GTK_LAYER_SHELL_EDGE_{LEFT, RIGHT, TOP, BOTTOM}`.
    const EDGES: [c_int; 4] = [0, 1, 2, 3];
    /// More toplevels than any GTK app has: the walk stops there.
    const MAX_TOPLEVELS: usize = 1024;

    /// The functions used, resolved once; the libraries stay loaded with them.
    struct Api {
        _layer_shell: Library,
        is_supported: unsafe extern "C" fn() -> GBoolean,
        init_for_window: unsafe extern "C" fn(*mut c_void),
        is_layer_window: unsafe extern "C" fn(*mut c_void) -> GBoolean,
        set_namespace: unsafe extern "C" fn(*mut c_void, *const c_char),
        set_layer: unsafe extern "C" fn(*mut c_void, c_int),
        set_anchor: unsafe extern "C" fn(*mut c_void, c_int, GBoolean),
        set_margin: unsafe extern "C" fn(*mut c_void, c_int, c_int),
        set_keyboard_mode: unsafe extern "C" fn(*mut c_void, c_int),
        list_toplevels: unsafe extern "C" fn() -> *mut GList,
        get_title: unsafe extern "C" fn(*mut c_void) -> *const c_char,
        /// A layer surface takes its size from the size request, not from a resize.
        set_size_request: unsafe extern "C" fn(*mut c_void, c_int, c_int),
        window_resize: unsafe extern "C" fn(*mut c_void, c_int, c_int),
        list_free: unsafe extern "C" fn(*mut GList),
        main_context_default: unsafe extern "C" fn() -> *mut c_void,
        main_context_is_owner: unsafe extern "C" fn(*mut c_void) -> GBoolean,
    }

    /// Copy a function pointer of type `T` out of `lib`.
    ///
    /// # Safety
    /// `T` must be the symbol's real C signature.
    unsafe fn sym<T: Copy>(lib: &Library, name: &[u8]) -> Option<T> {
        // SAFETY: the caller names the symbol's true type; the pointer is copied out and
        // only used while `lib` stays loaded (`Api` keeps it, or it is the process).
        unsafe { lib.get::<T>(name).ok().map(|s| *s) }
    }

    fn load() -> Option<Api> {
        // SAFETY: loading the system's gtk-layer-shell runs its initialisers, which only
        // register GObject types; it links against the GTK this process already uses.
        let layer = unsafe { Library::new("libgtk-layer-shell.so.0") }.ok()?;
        // GTK and GLib are already in this process (the app links them); a process
        // without GTK (the CLI) finds no `gtk_*` symbols and gets `None`.
        let this: Library = libloading::os::unix::Library::this().into();
        // SAFETY: each type below is the C signature from gtk-layer-shell.h, gtkwindow.h,
        // glist.h and gmain.h (gboolean is int; GtkWindow*, GList* and GMainContext* are
        // passed as opaque pointers).
        unsafe {
            Some(Api {
                is_supported: sym(&layer, b"gtk_layer_is_supported\0")?,
                init_for_window: sym(&layer, b"gtk_layer_init_for_window\0")?,
                is_layer_window: sym(&layer, b"gtk_layer_is_layer_window\0")?,
                set_namespace: sym(&layer, b"gtk_layer_set_namespace\0")?,
                set_layer: sym(&layer, b"gtk_layer_set_layer\0")?,
                set_anchor: sym(&layer, b"gtk_layer_set_anchor\0")?,
                set_margin: sym(&layer, b"gtk_layer_set_margin\0")?,
                set_keyboard_mode: sym(&layer, b"gtk_layer_set_keyboard_mode\0")?,
                list_toplevels: sym(&this, b"gtk_window_list_toplevels\0")?,
                get_title: sym(&this, b"gtk_window_get_title\0")?,
                set_size_request: sym(&this, b"gtk_widget_set_size_request\0")?,
                window_resize: sym(&this, b"gtk_window_resize\0")?,
                list_free: sym(&this, b"g_list_free\0")?,
                main_context_default: sym(&this, b"g_main_context_default\0")?,
                main_context_is_owner: sym(&this, b"g_main_context_is_owner\0")?,
                _layer_shell: layer,
            })
        }
    }

    fn api() -> Option<&'static Api> {
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(load).as_ref()
    }

    /// Run `f` with the toplevel titled `title`, on the GTK main thread only.
    fn with_window<R>(
        api: &Api,
        title: &str,
        f: impl FnOnce(*mut c_void) -> R,
    ) -> Result<R, PlatformError> {
        // SAFETY: plain GLib calls without arguments to keep alive.
        let on_main = unsafe { (api.main_context_is_owner)((api.main_context_default)()) } != 0;
        if !on_main {
            return Err(PlatformError::Os(
                "layer shell: not on the GTK main thread".into(),
            ));
        }
        let title = CString::new(title)
            .map_err(|_| PlatformError::Os("layer shell: bad window title".into()))?;
        // SAFETY: on the GTK main thread (checked above). The list is a new one we free;
        // its windows are GTK's (not referenced by the list) and stay alive during this
        // call, since nothing here runs the main loop or destroys a window.
        unsafe {
            let list = (api.list_toplevels)();
            let mut node = list;
            let mut found = None;
            let mut seen = 0usize;
            while !node.is_null() && seen < MAX_TOPLEVELS {
                let window = (*node).data;
                if !window.is_null() {
                    let t = (api.get_title)(window);
                    if !t.is_null() && CStr::from_ptr(t) == title.as_c_str() {
                        found = Some(window);
                        break;
                    }
                }
                node = (*node).next;
                seen = seen.saturating_add(1);
            }
            let result = found.map(f);
            (api.list_free)(list);
            result.ok_or_else(|| PlatformError::Os("layer shell: no such window".into()))
        }
    }

    pub(super) fn init(title: &str) -> Result<bool, PlatformError> {
        let Some(api) = api() else { return Ok(false) };
        // SAFETY: no arguments; it asks the GDK display, which exists once GTK is up.
        if unsafe { (api.is_supported)() } == 0 {
            return Ok(false);
        }
        with_window(api, title, |w| {
            // SAFETY: `w` is a live GtkWindow on the GTK main thread (`with_window`); the
            // namespace is a static C string that gtk-layer-shell copies.
            unsafe {
                (api.init_for_window)(w);
                (api.set_namespace)(w, c"scout-overlay".as_ptr());
                (api.set_layer)(w, LAYER_OVERLAY);
                (api.set_keyboard_mode)(w, KEYBOARD_NONE);
            }
        })?;
        Ok(true)
    }

    pub(super) fn place(
        title: &str,
        (width, height): (i32, i32),
        anchor: OverlayAnchor,
        margin: i32,
        keyboard: bool,
    ) -> Result<bool, PlatformError> {
        let Some(api) = api() else { return Ok(false) };
        let (width, height) = (width.max(1), height.max(1));
        with_window(api, title, |w| {
            // SAFETY: `w` is a live GtkWindow on the GTK main thread (`with_window`).
            if unsafe { (api.is_layer_window)(w) } == 0 {
                return false;
            }
            // SAFETY: as above; sizes are positive.
            unsafe {
                (api.set_size_request)(w, width, height);
                (api.window_resize)(w, width, height);
            }
            for (edge, on) in EDGES.iter().zip(anchor.edges()) {
                // SAFETY: as above; edges and modes are gtk-layer-shell's enum values.
                unsafe {
                    (api.set_anchor)(w, *edge, GBoolean::from(on));
                    (api.set_margin)(w, *edge, if on { margin } else { 0 });
                }
            }
            let mode = if keyboard {
                KEYBOARD_EXCLUSIVE
            } else {
                KEYBOARD_NONE
            };
            // SAFETY: as above.
            unsafe { (api.set_keyboard_mode)(w, mode) };
            true
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_anchor_two_edges_and_centre_none() {
        assert_eq!(
            OverlayAnchor::from_corner("top-left"),
            OverlayAnchor::TopLeft
        );
        assert_eq!(
            OverlayAnchor::from_corner("bottom-right"),
            OverlayAnchor::BottomRight
        );
        assert_eq!(
            OverlayAnchor::from_corner("nonsense"),
            OverlayAnchor::TopRight
        );
        // (left, right, top, bottom)
        assert_eq!(OverlayAnchor::TopRight.edges(), [false, true, true, false]);
        assert_eq!(
            OverlayAnchor::BottomLeft.edges(),
            [true, false, false, true]
        );
        assert_eq!(OverlayAnchor::Center.edges(), [false; 4]);
    }

    /// A process without GTK (this test) gets "not available", never an error.
    #[test]
    fn without_gtk_it_is_unavailable() {
        assert!(!overlay_layer_init("Scout overlay").unwrap());
        assert!(
            !overlay_layer_place(
                "Scout overlay",
                (392, 100),
                OverlayAnchor::TopRight,
                18,
                false
            )
            .unwrap()
        );
    }
}
