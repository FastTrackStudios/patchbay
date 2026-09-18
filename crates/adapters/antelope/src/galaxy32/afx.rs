//! Schema-driven AFX (effects) catalog and parameter encoder.
//!
//! Every effect has `set_<name>_conf` whose args are POSITIONAL in the
//! schema's `params.fields` order: `[type_id, inst_id, <fields…>]`, e.g.
//! `["set_opto2a_conf", [59, 6, 0, 1, 31, 1], {}]` (confirmed live). The
//! effect's type id is the `ext3` of its `get_<name>_conf` schema header
//! (cross-checked against captured `set_afx_order` slots).
//!
//! TODO(afx-read): per-instance read-back is not understood —
//! `get_<name>_conf` ignores the instance argument and returns some other
//! instance — so parameter values can't be read from the device yet.
//! TODO(afx-units): values are raw schema integers/floats; knob units and
//! ranges are unmapped.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{AntelopeError, Result};
use crate::protocol::RPC_SCHEMA;
use crate::protocol::call::Call;

/// Type ids missing from the schema (no `get_<name>_conf`), from captures.
const TYPE_ID_OVERRIDES: [(&str, u8); 1] = [("antares_autotune", 52)];

/// Scalar field type from the schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AfxFieldType {
    /// `uint8` / `ubyte`.
    U8,
    /// `int8` / `byte`.
    I8,
    /// `uint16`.
    U16,
    /// `int16`.
    I16,
    /// `uint32`.
    U32,
    /// `float`.
    F32,
    /// Arrays and anything else: passed through unchecked.
    Other(String),
}

impl AfxFieldType {
    fn parse(spec: &[Value]) -> Self {
        let name = spec.first().and_then(Value::as_str).unwrap_or_default();
        // `[type, bits]` is a scalar bitfield; longer specs are arrays.
        if spec.len() > 2 || (spec.len() == 2 && !matches!(name, "uint8" | "ubyte")) {
            return Self::Other(Value::Array(spec.to_vec()).to_string());
        }
        match name {
            // `["ubyte", 6]`-style entries carry a bit width; still a byte.
            "uint8" | "ubyte" => Self::U8,
            "int8" | "byte" => Self::I8,
            "uint16" => Self::U16,
            "int16" => Self::I16,
            "uint32" => Self::U32,
            "float" => Self::F32,
            other => Self::Other(other.to_owned()),
        }
    }

    /// Integer range, for integer types.
    #[must_use]
    pub const fn int_range(&self) -> Option<(i64, i64)> {
        match self {
            Self::U8 => Some((0, 255)),
            Self::I8 => Some((-128, 127)),
            Self::U16 => Some((0, 65_535)),
            Self::I16 => Some((-32_768, 32_767)),
            Self::U32 => Some((0, 4_294_967_295)),
            Self::F32 | Self::Other(_) => None,
        }
    }

    fn check(&self, field: &str, v: &Value) -> Result<()> {
        let ok = match self {
            Self::F32 => v.is_number(),
            Self::Other(_) => true,
            int => {
                let (lo, hi) = int.int_range().unwrap_or((i64::MIN, i64::MAX));
                v.as_i64().is_some_and(|n| n >= lo && n <= hi)
                    // The panel sends some flags as JSON booleans.
                    || v.is_boolean()
            }
        };
        if ok {
            Ok(())
        } else {
            Err(AntelopeError::Invalid(format!(
                "AFX field `{field}` ({self:?}): bad value {v}"
            )))
        }
    }
}

/// One settable effect field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfxField {
    /// Schema field name, e.g. `gain`.
    pub name: String,
    /// Schema type.
    pub ty: AfxFieldType,
}

/// One effect type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfxEffect {
    /// Schema name, e.g. `opto2a`.
    pub name: String,
    /// `set_<name>_conf`.
    pub set_method: String,
    /// Type id used in `set_afx_order` slots and as arg 0.
    pub type_id: Option<u8>,
    /// Fields after `[type_id, inst_id]`, in wire order.
    pub fields: Vec<AfxField>,
    /// `false` when the arg prefix isn't `[type_id, inst_id]`
    /// (`guitar_cab`); [`AfxEffect::encode`] refuses those.
    pub generic: bool,
}

impl AfxEffect {
    /// Field index by name.
    #[must_use]
    pub fn field_index(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name == name)
    }

    /// Encode `set_<name>_conf [type_id, inst, values…]`. `values` are all
    /// fields in [`AfxEffect::fields`] order (the call replaces the whole
    /// instance config).
    ///
    /// # Errors
    /// Unknown type id, non-generic effect, wrong arity or out-of-range value.
    pub fn encode(&self, inst: u8, values: &[Value]) -> Result<Call> {
        let type_id = self.type_id.ok_or_else(|| {
            AntelopeError::Invalid(format!("AFX `{}`: type id unknown", self.name))
        })?;
        if !self.generic {
            return Err(AntelopeError::Invalid(format!(
                "AFX `{}`: non-generic arg layout, use raw_call",
                self.name
            )));
        }
        if values.len() != self.fields.len() {
            return Err(AntelopeError::Invalid(format!(
                "AFX `{}` takes {} values ({}), got {}",
                self.name,
                self.fields.len(),
                self.fields
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                values.len()
            )));
        }
        let mut call = Call::new(self.set_method.clone()).arg(type_id).arg(inst);
        for (f, v) in self.fields.iter().zip(values) {
            f.ty.check(&f.name, v)?;
            call = call.arg(v.clone());
        }
        Ok(call)
    }

    /// [`AfxEffect::encode`] from a `{field: value}` map that must name
    /// every field.
    ///
    /// # Errors
    /// Missing/unknown fields, plus everything [`AfxEffect::encode`] rejects.
    pub fn encode_named(&self, inst: u8, values: &Map<String, Value>) -> Result<Call> {
        if let Some(extra) = values.keys().find(|k| self.field_index(k).is_none()) {
            return Err(AntelopeError::Invalid(format!(
                "AFX `{}` has no field `{extra}`",
                self.name
            )));
        }
        let ordered = self
            .fields
            .iter()
            .map(|f| {
                values.get(&f.name).cloned().ok_or_else(|| {
                    AntelopeError::Invalid(format!("AFX `{}`: missing `{}`", self.name, f.name))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.encode(inst, &ordered)
    }
}

/// All effects in the embedded Manager Server schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfxCatalog {
    effects: Vec<AfxEffect>,
}

#[derive(Deserialize)]
struct Schema {
    requests: Map<String, Value>,
}

impl AfxCatalog {
    /// Parse the embedded schema.
    ///
    /// # Errors
    /// Schema JSON malformed (never for the embedded copy).
    pub fn load() -> Result<Self> {
        Self::from_schema_json(RPC_SCHEMA)
    }

    /// Parse any `rpc_schema_full.json`-shaped document.
    ///
    /// # Errors
    /// JSON malformed.
    pub fn from_schema_json(json: &str) -> Result<Self> {
        let schema: Schema = serde_json::from_str(json)?;
        let mut effects: Vec<AfxEffect> = schema
            .requests
            .iter()
            .filter_map(|(method, spec)| {
                let name = method.strip_prefix("set_")?.strip_suffix("_conf")?;
                let fields: Vec<(String, AfxFieldType)> = spec
                    .pointer("/params/fields")?
                    .as_array()?
                    .iter()
                    .filter_map(|f| {
                        let arr = f.as_array()?;
                        let (fname, rest) = arr.split_first()?;
                        Some((fname.as_str()?.to_owned(), AfxFieldType::parse(rest)))
                    })
                    .collect();
                let generic = fields.first().is_some_and(|(n, _)| n == "type_id")
                    && fields.get(1).is_some_and(|(n, _)| n == "inst_id");
                let type_id = schema
                    .requests
                    .get(&format!("get_{name}_conf"))
                    .and_then(|g| g.pointer("/header/ext3"))
                    .and_then(Value::as_u64)
                    .and_then(|n| u8::try_from(n).ok())
                    .or_else(|| {
                        TYPE_ID_OVERRIDES
                            .iter()
                            .find(|(n, _)| *n == name)
                            .map(|(_, id)| *id)
                    });
                Some(AfxEffect {
                    name: name.to_owned(),
                    set_method: method.clone(),
                    type_id,
                    fields: fields
                        .into_iter()
                        .skip(2)
                        .map(|(name, ty)| AfxField { name, ty })
                        .collect(),
                    generic,
                })
            })
            .collect();
        effects.sort_by(|a, b| a.type_id.cmp(&b.type_id).then_with(|| a.name.cmp(&b.name)));
        Ok(Self { effects })
    }

    /// All effects, sorted by type id (unknown ids first).
    #[must_use]
    pub fn effects(&self) -> &[AfxEffect] {
        &self.effects
    }

    /// Effect by schema name (`opto2a`).
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&AfxEffect> {
        self.effects.iter().find(|e| e.name == name)
    }

    /// Effect by type id (`59`).
    #[must_use]
    pub fn by_type(&self, type_id: u8) -> Option<&AfxEffect> {
        self.effects.iter().find(|e| e.type_id == Some(type_id))
    }

    /// Effects with a known type id — the ones a slot can hold.
    pub fn insertable(&self) -> impl Iterator<Item = &AfxEffect> {
        self.effects.iter().filter(|e| e.type_id.is_some())
    }
}
