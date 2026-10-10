//! Panic-free property setting. `glib`'s typed setters panic on unknown properties or
//! type mismatches; these helpers check the property spec first and report instead.

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;

/// Set `name` from its string form (`"true"`, `"4000"`, an enum nick, …) if the element
/// has a writable property of that name. Returns whether it was set.
pub fn set_str(el: &gst::Element, name: &str, value: &str) -> bool {
    let Some(pspec) = el.find_property(name) else {
        tracing::debug!(element = %el.name(), property = name, "property not present");
        return false;
    };
    let flags = pspec.flags();
    if !flags.contains(glib::ParamFlags::WRITABLE)
        || flags.contains(glib::ParamFlags::CONSTRUCT_ONLY)
    {
        tracing::warn!(element = %el.name(), property = name, "property not writable");
        return false;
    }
    let value = match glib::Value::deserialize_with_pspec(value, &pspec) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(element = %el.name(), property = name, value, error = %e, "bad property value");
            return false;
        }
    };
    if value.type_() != pspec.value_type() {
        return false;
    }
    el.set_property_from_value(name, &value);
    true
}

/// Like [`set_str`] but an error if the property is missing or rejects the value.
pub fn require(el: &gst::Element, name: &str, value: &str) -> Result<(), String> {
    if set_str(el, name, value) {
        Ok(())
    } else {
        Err(format!("{}: cannot set {name}={value}", el.name()))
    }
}
