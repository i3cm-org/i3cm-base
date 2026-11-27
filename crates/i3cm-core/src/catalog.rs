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
    pub profiles: Vec<ProfileDef>,
    pub i18n: BTreeMap<String, String>,
    #[serde(default)]
    pub aspects: Vec<AspectDef>,
    #[serde(default)]
    pub operations: Vec<OperationDef>,
    #[serde(default)]
    pub hooks: Vec<HookDef>,
    #[serde(default)]
    pub schedules: Vec<ScheduleDef>,
    #[serde(default)]
    pub carriers: Vec<CarrierDef>,
    /// Delivered with the kernel and activated in every tenant at creation (DEC-138).
    #[serde(default)]
    pub mandatory: bool,
}

/// An operation exported by a module's WebAssembly component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationDef {
    pub name: String,
    /// Carriers of the same module the operation may impersonate (DEC-117).
    #[serde(default)]
    pub impersonates: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDef {
    #[serde(rename = "type")]
    pub type_name: String,
    pub on: String,
    pub phase: String,
    pub op: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleDef {
    pub op: String,
    pub rrule: String,
}

/// A rights carrier owned by a module: an agent account created on activation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CarrierDef {
    pub name: String,
    #[serde(default)]
    pub profiles: Vec<String>,
}

/// A business aspect: traversal and display rules (views).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AspectDef {
    /// Qualified name, e.g. `base:mailbox`.
    pub name: String,
    pub entry: AspectEntry,
    pub nodes: BTreeMap<String, NodeDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AspectEntry {
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(rename = "groupBy", default)]
    pub group_by: Vec<String>,
    /// View filter (`entry.where`), see `paths::view_filter`.
    #[serde(rename = "where", default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeDef {
    #[serde(default)]
    pub attributes: Vec<Labeled>,
    #[serde(rename = "subNodes", default)]
    pub sub_nodes: Vec<SubNodeDef>,
    #[serde(default)]
    pub files: Vec<FileDef>,
    #[serde(default)]
    pub tabs: Vec<TabDef>,
    #[serde(default)]
    pub functions: Vec<FunctionDef>,
    /// The files block comes before the sub-node block in the definition.
    #[serde(rename = "filesFirst", default = "yes")]
    pub files_first: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Labeled {
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubNodeDef {
    pub label: String,
    pub path: String,
    #[serde(default)]
    pub folder: bool,
    /// Files copied into this folder become new objects of this type (writable views).
    #[serde(rename = "dropType", default, skip_serializing_if = "Option::is_none")]
    pub drop_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDef {
    /// File name; `{attr}` is replaced by an attribute of the object.
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabDef {
    pub label: String,
    pub path: String,
    pub columns: Vec<Labeled>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileDef {
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
    /// Rights path: the right applies only to objects for which this condition holds, e.g.
    /// `applicant.team.leader = %me` (plus rights only; DEC-045, SPEC-004).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
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
    /// Searched by `grep` (default); off for texts such as the OCR layer of a scan.
    #[serde(default = "yes")]
    pub grep: bool,
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

    /// Types whose attributes are readable in paths at `t`: `t`, its supertypes, and the host types
    /// it requires where carrying them is guaranteed (`@requires` with one type, or `@requiresAll`).
    pub fn path_types(&self, t: &str) -> Vec<&TypeDef> {
        let mut out: Vec<&TypeDef> = self.ancestors(t);
        let mut i = 0;
        while i < out.len() {
            let d = out[i];
            if d.requires.mode == RequireMode::All || d.requires.types.len() == 1 {
                for h in &d.requires.types {
                    for a in self.ancestors(h) {
                        if !out.iter().any(|x| x.name == a.name) {
                            out.push(a);
                        }
                    }
                }
            }
            i += 1;
            if out.len() > 128 {
                break;
            }
        }
        out
    }

    /// Like `defining_type`, also searching the guaranteed host types (`path_types`).
    pub fn path_defining_type(&self, t: &str, attr: &str) -> Option<&TypeDef> {
        self.path_types(t).into_iter().find(|a| a.attributes.iter().any(|x| x.name == attr))
    }

    /// Type that defines attribute `attr`, searching `t` and its supertypes.
    pub fn defining_type(&self, t: &str, attr: &str) -> Option<&TypeDef> {
        self.ancestors(t).into_iter().find(|a| a.attributes.iter().any(|x| x.name == attr))
    }

    /// Module (namespace) of a type or reference name.
    pub fn module_of(name: &str) -> &str {
        name.split(':').next().unwrap_or("")
    }

    /// Modules a tenant has without activation: the base and the mandatory ones.
    pub fn is_default_module(&self, name: &str) -> bool {
        name == "base" || self.catalog.modules.iter().any(|m| m.name == name && m.mandatory)
    }

    /// Closure under implications, leaving out types of modules not active in the tenant.
    pub fn implied_closure_in(&self, attached: &BTreeSet<String>, inactive: &BTreeSet<String>) -> BTreeSet<String> {
        if inactive.is_empty() {
            return self.implied_closure(attached);
        }
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
                if set.contains(&cand.name) || inactive.contains(Self::module_of(&cand.name)) {
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

    /// The type and all its subtypes, transitively.
    pub fn subtypes_closure(&self, name: &str) -> Vec<String> {
        let mut out = vec![name.to_string()];
        let mut i = 0;
        while i < out.len() {
            let more: Vec<String> = self.subtypes(&out[i]).to_vec();
            for x in more {
                if !out.contains(&x) {
                    out.push(x);
                }
            }
            i += 1;
        }
        out
    }

    pub fn subtypes(&self, name: &str) -> &[String] {
        self.children.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn profiles(&self) -> impl Iterator<Item = &ProfileDef> {
        self.catalog.modules.iter().flat_map(|m| m.profiles.iter())
    }

    pub fn aspect(&self, name: &str) -> Option<&AspectDef> {
        self.catalog.modules.iter().flat_map(|m| m.aspects.iter()).find(|a| a.name == name)
    }

    pub fn aspects(&self) -> impl Iterator<Item = &AspectDef> {
        self.catalog.modules.iter().flat_map(|m| m.aspects.iter())
    }

    /// Node definition for an object carrying `set`: the first attached type (or supertype)
    /// the aspect defines.
    pub fn aspect_node<'a>(&self, a: &'a AspectDef, set: &BTreeSet<String>) -> Option<(String, &'a NodeDef)> {
        for t in set {
            for anc in self.ancestors(t) {
                if let Some(n) = a.nodes.get(&anc.name) {
                    return Some((t.clone(), n));
                }
            }
        }
        None
    }

    /// Text type by full or local name, owned by one of the given types.
    pub fn text_type_for(&self, set: &BTreeSet<String>, name: &str) -> Option<&TextTypeDef> {
        let tables = self.table_types(set);
        self.catalog.text_types.iter().find(|t| tables.contains(&t.owner) && (t.name == name || t.name.split_once(':').map(|x| x.1) == Some(name)))
    }

    pub fn text_type(&self, name: &str) -> Option<&TextTypeDef> {
        self.catalog.text_types.iter().find(|t| t.name == name)
    }
}
