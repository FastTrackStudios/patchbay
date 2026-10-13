//! Client → server calls: `["method", [args...], {kwargs}]`.

use serde::ser::{SerializeMap, SerializeTuple};
use serde::{Serialize, Serializer};
use serde_json::Value;

use super::framing::encode_frame;
use crate::error::Result;

/// One RPC call. Keyword arguments keep insertion order so the encoded
/// JSON is byte-identical to what the official panel sends (the server
/// doesn't care, but captures and tests do).
///
/// Writes (`set_*`) are fire-and-forget: the server sends no reply.
/// Reads (`get_*`) are answered by a `single` frame whose header carries
/// the method's `ext2` id and the `ext3` kwarg (the read index).
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// Method name, e.g. `set_mixer`.
    pub method: String,
    /// Positional arguments.
    pub args: Vec<Value>,
    /// Keyword arguments, in wire order.
    pub kwargs: Vec<(String, Value)>,
}

impl Call {
    /// A call with no arguments.
    #[must_use]
    pub fn new(method: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            args: Vec::new(),
            kwargs: Vec::new(),
        }
    }

    /// Append a positional argument.
    #[must_use]
    pub fn arg(mut self, v: impl Into<Value>) -> Self {
        self.args.push(v.into());
        self
    }

    /// Append a keyword argument.
    #[must_use]
    pub fn kwarg(mut self, k: impl Into<String>, v: impl Into<Value>) -> Self {
        self.kwargs.push((k.into(), v.into()));
        self
    }

    /// Value of a keyword argument.
    #[must_use]
    pub fn kwarg_value(&self, k: &str) -> Option<&Value> {
        self.kwargs.iter().find(|(key, _)| key == k).map(|(_, v)| v)
    }

    /// Compact JSON, exactly as sent on the wire.
    ///
    /// # Errors
    /// Serialisation failure (only possible for non-finite floats).
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// The complete length-prefixed frame.
    ///
    /// # Errors
    /// Serialisation or framing failure.
    pub fn to_frame(&self) -> Result<Vec<u8>> {
        encode_frame(&serde_json::to_vec(self)?)
    }

    /// Parse a call from its JSON array form (as carried in
    /// `notification` contents). Missing args/kwargs default to empty.
    #[must_use]
    pub fn from_value(v: &Value) -> Option<Self> {
        let arr = v.as_array()?;
        let mut it = arr.iter();
        let method = it.next()?.as_str()?.to_owned();
        let args = it
            .next()
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let kwargs = it
            .next()
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        Some(Self {
            method,
            args,
            kwargs,
        })
    }
}

struct Kwargs<'a>(&'a [(String, Value)]);

impl Serialize for Kwargs<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

impl Serialize for Call {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut t = s.serialize_tuple(3)?;
        t.serialize_element(&self.method)?;
        t.serialize_element(&self.args)?;
        t.serialize_element(&Kwargs(&self.kwargs))?;
        t.end()
    }
}
