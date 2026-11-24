//! The type catalog produced by `typecat` from the TypeScript type definitions.

use crate::error::{BaseError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const FORMAT: u32 = 1;
/// Pseudo type name for "any object" (reference targets) and "all types" (rights).
pub const ANY: &str = "*";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub format: u32,
    pub modules: Vec<Module>,
    pub types: Vec<TypeDef>,
    pub references: Vec<RefDef>,
    #[serde(rename = "textTypes")]
    pub text_types: Vec<TextTypeDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Module {
    pub name: String,
    pub requires: Vec<String>,
    pub rooms: Vec<String>,
    pub roles: Vec<RoleDef>,
    pub i18n: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleDef {
    pub name: String,
    #[serde(default)]
    pub guardian: bool,
    #[serde(default)]
    pub includes: Vec<String>,
    pub rights: Vec<RightDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RightDef {
    pub sign: Sign,
    pub art: Vec<Art>,
    pub types: Vec<String>,
    /// "root" or the name of a module room.
    pub room: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Sign {
    #[serde(rename = "+")]
    Plus,
    #[serde(rename = "-")]
    Minus,
    #[serde(rename = "block")]
    Block,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Art {
    Read,
    Write,
    Create,
    Place,
    Remove,
    Delete,
    Grant,
}

impl Art {
    pub fn as_str(&self) -> &'static str {
        match self {
            Art::Read => "read",
            Art::Write => "write",
            Art::Create => "create",
            Art::Place => "place",
            Art::Remove => "remove",
            Art::Delete => "delete",
            Art::Grant => "grant",
        }
    }
    pub fn parse(s: &str) -> Option<Art> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeDef {
    pub name: String,
    pub ns: String,
    pub local: String,
    #[serde(rename = "abstract")]
    pub is_abstract: bool,
    pub extends: Option<String>,
    pub attributes: Vec<AttrDef>,
    pub requires: Requires,
    #[serde(rename = "imprintsOn")]
    pub imprints_on: Vec<String>,
    pub brings: Vec<String>,
    pub content: Option<ContentDef>,
    #[serde(rename = "textTypes")]
    pub text_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Requires {
    pub mode: RequireMode,
    pub types: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RequireMode {
    Any,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentDef {
    #[serde(rename = "mediaType")]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttrDef {
    pub name: String,
    pub vt: ValueType,
    pub required: bool,
    pub list: bool,
    pub indexed: bool,
    pub unique: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ValueType {
    String,
    Boolean,
    Int,
    Decimal { precision: u32, scale: u32 },
    Text { formats: Vec<String> },
    Date,
    Datetime,
    Time,
    Duration,
    Enum { values: Vec<String> },
    Money,
    Formatted { format: String },
    Recurrence,
    Struct { fields: Vec<StructField> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructField {
    pub name: String,
    pub vt: ValueType,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefDef {
    pub name: String,
    pub holder: End,
    pub target: End,
    #[serde(rename = "partOf")]
    pub part_of: bool,
    pub ordered: bool,
    #[serde(rename = "rightsRelevant")]
    pub rights_relevant: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct End {
    #[serde(rename = "type")]
    pub type_name: String,
    pub end: String,
    pub min: u32,
    pub max: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextTypeDef {
    pub name: String,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub owner: String,
}

/// Which side of a reference an end belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Holder,
    Target,
}

/// Catalog plus lookup indices. Built once at startup.
#[derive(Debug)]
pub struct Model {
    pub catalog: Catalog,
    types: HashMap<String, usize>,
    refs: HashMap<String, usize>,
    /// direct subtypes
    children: HashMap<String, Vec<String>>,
}

impl Model {
    pub fn new(catalog: Catalog) -> Result<Model> {
        let mut types = HashMap::new();
        for (i, t) in catalog.types.iter().enumerate() {
            if types.insert(t.name.clone(), i).is_some() {
                return Err(BaseError::invalid("catalog.duplicate_type", "type defined twice").with("type", t.name.clone()));
            }
        }
        let mut refs = HashMap::new();
        for (i, r) in catalog.references.iter().enumerate() {
            if refs.insert(r.name.clone(), i).is_some() {
                return Err(BaseError::invalid("catalog.duplicate_reference", "reference defined twice").with("reference", r.name.clone()));
            }
        }
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        for t in &catalog.types {
            if let Some(p) = &t.extends {
                children.entry(p.clone()).or_default().push(t.name.clone());
            }
        }
        let m = Model { catalog, types, refs, children };
        crate::checker::check(&m)?;
        Ok(m)
    }

    pub fn from_json(json: &str) -> Result<Model> {
        let c: Catalog = serde_json::from_str(json)
            .map_err(|e| BaseError::invalid("catalog.parse", "catalog.json cannot be read").with("reason", e.to_string()))?;
        if c.format != FORMAT {
            return Err(BaseError::invalid("catalog.format", "unsupported catalog format").with("format", c.format));
        }
        Model::new(c)
    }

    pub fn type_def(&self, name: &str) -> Option<&TypeDef> {
        self.types.get(name).map(|&i| &self.catalog.types[i])
    }
    pub fn require_type(&self, name: &str) -> Result<&TypeDef> {
        self.type_def(name).ok_or_else(|| BaseError::invalid("type.unknown", "unknown type").with("type", name))
    }
    pub fn ref_def(&self, name: &str) -> Option<&RefDef> {
        self.refs.get(name).map(|&i| &self.catalog.references[i])
    }

    /// The type and all its supertypes, most general last.
    pub fn ancestors(&self, name: &str) -> Vec<&TypeDef> {
        let mut out = Vec::new();
        let mut cur = self.type_def(name);
        while let Some(t) = cur {
            out.push(t);
            cur = t.extends.as_deref().and_then(|p| self.type_def(p));
            if out.len() > 64 {
                break;
            }
        }
        out
    }

    /// True if `t` equals `of` or is a subtype of it; everything is a subtype of ANY.
    pub fn is_a(&self, t: &str, of: &str) -> bool {
        of == ANY || self.ancestors(t).iter().any(|a| a.name == of)
    }

    /// True if an object carrying the given types counts as `of`.
    pub fn set_is_a<'a>(&self, set: impl IntoIterator<Item = &'a String>, of: &str) -> bool {
        set.into_iter().any(|t| self.is_a(t, of))
    }

    /// Type that defines attribute `attr`, searching `t` and its supertypes.
    pub fn defining_type(&self, t: &str, attr: &str) -> Option<&TypeDef> {
        self.ancestors(t).into_iter().find(|a| a.attributes.iter().any(|x| x.name == attr))
    }

    /// Closure of attached types under implications (brings, imprintsOn).
    pub fn implied_closure(&self, attached: &BTreeSet<String>) -> BTreeSet<String> {
        let mut set = attached.clone();
        loop {
            let mut added = false;
            let snapshot: Vec<String> = set.iter().cloned().collect();
            for t in &snapshot {
                for a in self.ancestors(t) {
                    for b in &a.brings {
                        added |= set.insert(b.clone());
                    }
                }
            }
            for cand in &self.catalog.types {
                if set.contains(&cand.name) {
                    continue;
                }
                if cand.imprints_on.iter().any(|on| self.set_is_a(&set, on)) {
                    set.insert(cand.name.clone());
                    added = true;
                }
            }
            if !added {
                return set;
            }
        }
    }

    /// All concrete types (attached + ancestors) whose tables hold rows for the given set.
    pub fn table_types(&self, attached: &BTreeSet<String>) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for t in attached {
            for a in self.ancestors(t) {
                out.insert(a.name.clone());
            }
        }
        out
    }

    /// Reference ends visible from an object carrying `set`: (reference, side of this object).
    pub fn ends_for(&self, set: &BTreeSet<String>) -> Vec<(&RefDef, Side)> {
        let mut out = Vec::new();
        for r in &self.catalog.references {
            if self.set_is_a(set, &r.holder.type_name) {
                out.push((r, Side::Holder));
            }
            if r.target.type_name == ANY || self.set_is_a(set, &r.target.type_name) {
                out.push((r, Side::Target));
            }
        }
        out
    }

    /// Find an end by name for an object carrying `set`.
    pub fn end_by_name(&self, set: &BTreeSet<String>, name: &str) -> Option<(&RefDef, Side)> {
        self.ends_for(set).into_iter().find(|(r, side)| match side {
            Side::Holder => r.holder.end == name,
            Side::Target => r.target.end == name,
        })
    }

    pub fn subtypes(&self, name: &str) -> &[String] {
        self.children.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn roles(&self) -> impl Iterator<Item = &RoleDef> {
        self.catalog.modules.iter().flat_map(|m| m.roles.iter())
    }

    pub fn text_type(&self, name: &str) -> Option<&TextTypeDef> {
        self.catalog.text_types.iter().find(|t| t.name == name)
    }
}
