//! Path language: a strict subset of FHIRPath (https://hl7.org/fhirpath/).
//!
//! Every step yields a collection; collections are flattened; a path into nothing is empty.
//! Names on an object are attributes, named reference ends or reserved names starting with `_`.
//! The evaluator is generic over [`Nav`], which the store implements (and which applies rights).

use crate::error::{BaseError, Result};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use uuid::Uuid;

/* ------------------------------------------------------------------ lexer */

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(String),
    Dot,
    LParen,
    RParen,
    Comma,
    Op(&'static str),
    This,
}

fn err(d: &str, expr: &str) -> BaseError {
    BaseError::invalid("path.syntax", d.to_string()).with("expr", expr)
}

fn lex(src: &str) -> Result<Vec<Tok>> {
    let cs: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < cs.len() {
        let c = cs[i];
        match c {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '.' => {
                out.push(Tok::Dot);
                i += 1
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1
            }
            '=' => {
                out.push(Tok::Op("="));
                i += 1
            }
            '!' if cs.get(i + 1) == Some(&'=') => {
                out.push(Tok::Op("!="));
                i += 2
            }
            '<' | '>' => {
                let eq = cs.get(i + 1) == Some(&'=');
                out.push(Tok::Op(match (c, eq) {
                    ('<', true) => "<=",
                    ('<', false) => "<",
                    ('>', true) => ">=",
                    _ => ">",
                }));
                i += if eq { 2 } else { 1 };
            }
            '\'' => {
                let mut s = String::new();
                i += 1;
                loop {
                    match cs.get(i) {
                        None => return Err(err("unterminated string", src)),
                        Some('\\') => {
                            match cs.get(i + 1) {
                                Some('n') => s.push('\n'),
                                Some('t') => s.push('\t'),
                                Some(x) => s.push(*x),
                                None => return Err(err("unterminated string", src)),
                            }
                            i += 2;
                        }
                        Some('\'') => {
                            i += 1;
                            break;
                        }
                        Some(x) => {
                            s.push(*x);
                            i += 1
                        }
                    }
                }
                out.push(Tok::Str(s));
            }
            '`' => {
                let mut s = String::new();
                i += 1;
                loop {
                    match cs.get(i) {
                        None => return Err(err("unterminated delimited identifier", src)),
                        Some('`') => {
                            i += 1;
                            break;
                        }
                        Some(x) => {
                            s.push(*x);
                            i += 1
                        }
                    }
                }
                out.push(Tok::Ident(s));
            }
            '$' => {
                let rest: String = cs[i..].iter().take(5).collect();
                if rest == "$this" {
                    out.push(Tok::This);
                    i += 5;
                } else {
                    return Err(err("only $this is supported", src));
                }
            }
            d if d.is_ascii_digit() => {
                let mut s = String::new();
                while i < cs.len() && (cs[i].is_ascii_digit() || (cs[i] == '.' && cs.get(i + 1).map(|x| x.is_ascii_digit()).unwrap_or(false))) {
                    s.push(cs[i]);
                    i += 1;
                }
                out.push(Tok::Num(s));
            }
            a if a.is_ascii_alphabetic() || a == '_' => {
                let mut s = String::new();
                while i < cs.len() && (cs[i].is_ascii_alphanumeric() || cs[i] == '_') {
                    s.push(cs[i]);
                    i += 1;
                }
                match s.as_str() {
                    "and" => out.push(Tok::Op("and")),
                    "or" => out.push(Tok::Op("or")),
                    _ => out.push(Tok::Ident(s)),
                }
            }
            other => return Err(err(&format!("unexpected character {other:?}"), src)),
        }
    }
    Ok(out)
}

/* ------------------------------------------------------------------ parser */

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    This,
    Lit(Value),
    /// `target.name`; target `None` means the focus.
    Member(Option<Box<Expr>>, String),
    /// `target.f(args)`; target `None` means the focus.
    Call(Option<Box<Expr>>, String, Vec<Expr>),
    Bin(&'static str, Box<Expr>, Box<Expr>),
}

/// Functions of the profile "views".
pub const VIEW_FUNCTIONS: &[&str] = &["where", "ofType", "first", "last", "count", "exists", "empty", "not", "toString", "substring", "upper", "lower"];

struct P<'a> {
    t: Vec<Tok>,
    i: usize,
    src: &'a str,
}

impl P<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        t
    }
    fn expect(&mut self, t: Tok) -> Result<()> {
        if self.next() == Some(t.clone()) { Ok(()) } else { Err(err(&format!("expected {t:?}"), self.src)) }
    }
    fn or(&mut self) -> Result<Expr> {
        let mut l = self.and()?;
        while self.peek() == Some(&Tok::Op("or")) {
            self.next();
            l = Expr::Bin("or", Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr> {
        let mut l = self.cmp()?;
        while self.peek() == Some(&Tok::Op("and")) {
            self.next();
            l = Expr::Bin("and", Box::new(l), Box::new(self.cmp()?));
        }
        Ok(l)
    }
    fn cmp(&mut self) -> Result<Expr> {
        let l = self.term()?;
        if let Some(Tok::Op(op)) = self.peek().cloned() {
            if ["=", "!=", "<", "<=", ">", ">="].contains(&op) {
                self.next();
                return Ok(Expr::Bin(op, Box::new(l), Box::new(self.term()?)));
            }
        }
        Ok(l)
    }
    fn args(&mut self) -> Result<Vec<Expr>> {
        let mut a = vec![];
        if self.peek() == Some(&Tok::RParen) {
            self.next();
            return Ok(a);
        }
        loop {
            a.push(self.or()?);
            match self.next() {
                Some(Tok::Comma) => continue,
                Some(Tok::RParen) => return Ok(a),
                _ => return Err(err("expected , or )", self.src)),
            }
        }
    }
    fn step(&mut self, target: Option<Box<Expr>>) -> Result<Expr> {
        match self.next() {
            Some(Tok::Ident(n)) => {
                if self.peek() == Some(&Tok::LParen) {
                    self.next();
                    let a = self.args()?;
                    Ok(Expr::Call(target, n, a))
                } else {
                    Ok(Expr::Member(target, n))
                }
            }
            _ => Err(err("expected a name", self.src)),
        }
    }
    fn term(&mut self) -> Result<Expr> {
        let mut e = match self.peek().cloned() {
            Some(Tok::Str(s)) => {
                self.next();
                Expr::Lit(Value::String(s))
            }
            Some(Tok::Num(n)) => {
                self.next();
                Expr::Lit(serde_json::from_str(&n).map_err(|_| err("bad number", self.src))?)
            }
            Some(Tok::This) => {
                self.next();
                Expr::This
            }
            Some(Tok::LParen) => {
                self.next();
                let e = self.or()?;
                self.expect(Tok::RParen)?;
                e
            }
            Some(Tok::Ident(n)) if n == "true" || n == "false" => {
                self.next();
                Expr::Lit(Value::Bool(n == "true"))
            }
            Some(Tok::Ident(_)) => self.step(None)?,
            _ => return Err(err("unexpected end or token", self.src)),
        };
        while self.peek() == Some(&Tok::Dot) {
            self.next();
            e = self.step(Some(Box::new(e)))?;
        }
        Ok(e)
    }
}

pub fn parse(src: &str) -> Result<Expr> {
    let t = lex(src)?;
    if t.is_empty() {
        return Err(err("empty expression", src));
    }
    let mut p = P { t, i: 0, src };
    let e = p.or()?;
    if p.i != p.t.len() {
        return Err(err("unexpected trailing tokens", src));
    }
    Ok(e)
}

/// Check that only the given functions are used.
pub fn check_profile(e: &Expr, allowed: &[&str], src: &str) -> Result<()> {
    match e {
        Expr::This | Expr::Lit(_) => Ok(()),
        Expr::Member(t, _) => t.as_ref().map(|t| check_profile(t, allowed, src)).unwrap_or(Ok(())),
        Expr::Call(t, f, a) => {
            if !allowed.contains(&f.as_str()) {
                return Err(BaseError::invalid("path.function_not_allowed", "function not allowed in this profile").with("function", f.clone()).with("expr", src));
            }
            if let Some(t) = t {
                check_profile(t, allowed, src)?;
            }
            // the argument of ofType is a type name, not a path
            if f != "ofType" {
                for x in a {
                    check_profile(x, allowed, src)?;
                }
            }
            Ok(())
        }
        Expr::Bin(_, l, r) => {
            check_profile(l, allowed, src)?;
            check_profile(r, allowed, src)
        }
    }
}

/// The first name a path reads from its focus (for checks against the focus type).
pub fn first_name(e: &Expr) -> Option<&str> {
    match e {
        Expr::Member(None, n) => Some(n),
        Expr::Member(Some(t), _) | Expr::Call(Some(t), _, _) => first_name(t),
        Expr::Bin(_, l, _) => first_name(l),
        _ => None,
    }
}

/* ------------------------------------------------------------------ evaluation */

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Obj(Uuid),
    /// Primitive or structured value (string, number, bool, object such as money).
    Val(Value),
    /// Binary content of an object.
    Content(Uuid),
    /// The texts of an object (`_texts`).
    Texts(Uuid),
    /// One text of an object: (object, text type).
    Text(Uuid, String),
}

impl Item {
    pub fn to_json(&self) -> Value {
        match self {
            Item::Obj(id) => Value::String(id.to_string()),
            Item::Val(v) => v.clone(),
            Item::Content(id) => serde_json::json!({"content": id}),
            Item::Texts(id) => serde_json::json!({"texts": id}),
            Item::Text(id, t) => serde_json::json!({"text": t, "object": id}),
        }
    }
}

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Navigation over the data, implemented by the store; applies rights.
pub trait Nav: Sync {
    /// Value(s) of `name` on an object, a `_texts` handle or similar.
    fn member<'a>(&'a self, focus: &'a Item, name: &'a str) -> BoxFut<'a, Result<Vec<Item>>>;
    /// Does the object carry the type (or a subtype)?
    fn is_type<'a>(&'a self, obj: Uuid, type_name: &'a str) -> BoxFut<'a, Result<bool>>;
}

fn single_bool(c: &[Item]) -> Option<bool> {
    match c {
        [Item::Val(Value::Bool(b))] => Some(*b),
        _ => None,
    }
}

fn cmp_vals(op: &str, a: &Item, b: &Item) -> Option<bool> {
    use std::cmp::Ordering;
    let ord = match (a, b) {
        (Item::Val(Value::Number(x)), Item::Val(Value::Number(y))) => x.as_f64()?.partial_cmp(&y.as_f64()?)?,
        (Item::Val(Value::String(x)), Item::Val(Value::String(y))) => x.cmp(y),
        (Item::Val(Value::Bool(x)), Item::Val(Value::Bool(y))) => x.cmp(y),
        (Item::Obj(x), Item::Obj(y)) => x.cmp(y),
        (Item::Obj(x), Item::Val(Value::String(y))) | (Item::Val(Value::String(y)), Item::Obj(x)) => {
            if op == "=" || op == "!=" {
                if x.to_string() == *y { Ordering::Equal } else { Ordering::Less }
            } else {
                return None;
            }
        }
        (Item::Val(x), Item::Val(y)) if op == "=" || op == "!=" => {
            if x == y { Ordering::Equal } else { Ordering::Less }
        }
        _ => return None,
    };
    Some(match op {
        "=" => ord == Ordering::Equal,
        "!=" => ord != Ordering::Equal,
        "<" => ord == Ordering::Less,
        "<=" => ord != Ordering::Greater,
        ">" => ord == Ordering::Greater,
        _ => ord != Ordering::Less,
    })
}

fn to_string(i: &Item) -> Option<String> {
    match i {
        Item::Val(Value::String(s)) => Some(s.clone()),
        Item::Val(Value::Null) => None,
        Item::Val(Value::Object(o)) if o.contains_key("amount") => Some(crate::values::display(&Value::Object(o.clone()))),
        Item::Val(v) => Some(match v {
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            other => other.to_string(),
        }),
        Item::Obj(id) => Some(id.to_string()),
        _ => None,
    }
}

fn int_arg(c: &[Item]) -> Option<i64> {
    match c {
        [Item::Val(Value::Number(n))] => n.as_i64(),
        _ => None,
    }
}

fn type_name_arg(e: &Expr) -> Option<String> {
    match e {
        Expr::Member(None, n) => Some(n.clone()),
        Expr::Lit(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Evaluate an expression against a focus collection.
pub fn eval<'a>(nav: &'a dyn Nav, e: &'a Expr, focus: &'a [Item]) -> BoxFut<'a, Result<Vec<Item>>> {
    Box::pin(async move {
        Ok(match e {
            Expr::This => focus.to_vec(),
            Expr::Lit(v) => vec![Item::Val(v.clone())],
            Expr::Member(target, name) => {
                let base = match target {
                    Some(t) => eval(nav, t, focus).await?,
                    None => focus.to_vec(),
                };
                let mut out = vec![];
                for it in &base {
                    match it {
                        Item::Val(Value::Object(o)) => {
                            if let Some(v) = o.get(name) {
                                push_val(&mut out, v);
                            }
                        }
                        Item::Val(_) => {}
                        other => out.extend(nav.member(other, name).await?),
                    }
                }
                out
            }
            Expr::Bin(op, l, r) => {
                let a = eval(nav, l, focus).await?;
                let b = eval(nav, r, focus).await?;
                match *op {
                    "and" => match (single_bool(&a), single_bool(&b)) {
                        (Some(false), _) | (_, Some(false)) => vec![Item::Val(Value::Bool(false))],
                        (Some(true), Some(true)) => vec![Item::Val(Value::Bool(true))],
                        _ => vec![],
                    },
                    "or" => match (single_bool(&a), single_bool(&b)) {
                        (Some(true), _) | (_, Some(true)) => vec![Item::Val(Value::Bool(true))],
                        (Some(false), Some(false)) => vec![Item::Val(Value::Bool(false))],
                        _ => vec![],
                    },
                    _ => {
                        if a.len() != 1 || b.len() != 1 {
                            vec![]
                        } else {
                            match cmp_vals(op, &a[0], &b[0]) {
                                Some(x) => vec![Item::Val(Value::Bool(x))],
                                None => vec![],
                            }
                        }
                    }
                }
            }
            Expr::Call(target, f, args) => {
                let base = match target {
                    Some(t) => eval(nav, t, focus).await?,
                    None => focus.to_vec(),
                };
                call(nav, f, args, base).await?
            }
        })
    })
}

fn push_val(out: &mut Vec<Item>, v: &Value) {
    match v {
        Value::Null => {}
        Value::Array(a) => a.iter().for_each(|x| push_val(out, x)),
        other => out.push(Item::Val(other.clone())),
    }
}

async fn call(nav: &dyn Nav, f: &str, args: &[Expr], base: Vec<Item>) -> Result<Vec<Item>> {
    let b = |x: bool| vec![Item::Val(Value::Bool(x))];
    Ok(match f {
        "where" => {
            let cond = args.first().ok_or_else(|| BaseError::invalid("path.arguments", "where needs a condition"))?;
            let mut out = vec![];
            for it in base {
                let r = eval(nav, cond, std::slice::from_ref(&it)).await?;
                if single_bool(&r) == Some(true) {
                    out.push(it);
                }
            }
            out
        }
        "ofType" => {
            let t = args.first().and_then(type_name_arg).ok_or_else(|| BaseError::invalid("path.arguments", "ofType needs a type name"))?;
            let mut out = vec![];
            for it in base {
                if let Item::Obj(id) = it {
                    if nav.is_type(id, &t).await? {
                        out.push(it);
                    }
                }
            }
            out
        }
        "first" => base.into_iter().take(1).collect(),
        "last" => base.into_iter().last().into_iter().collect(),
        "count" => vec![Item::Val(Value::from(base.len() as i64))],
        "exists" => b(!base.is_empty()),
        "empty" => b(base.is_empty()),
        "not" => match single_bool(&base) {
            Some(x) => b(!x),
            None => vec![],
        },
        "toString" => base.iter().filter_map(to_string).map(|s| Item::Val(Value::String(s))).take(1).collect(),
        "upper" | "lower" => base
            .iter()
            .filter_map(|i| match i {
                Item::Val(Value::String(s)) => Some(Item::Val(Value::String(if f == "upper" { s.to_uppercase() } else { s.to_lowercase() }))),
                _ => None,
            })
            .collect(),
        "substring" => {
            let start = match args.first() {
                Some(a) => int_arg(&eval(nav, a, &[]).await?),
                None => None,
            };
            let len = match args.get(1) {
                Some(a) => int_arg(&eval(nav, a, &[]).await?),
                None => None,
            };
            let Some(start) = start else { return Ok(vec![]) };
            base.iter()
                .filter_map(|i| match i {
                    Item::Val(Value::String(s)) => {
                        let cs: Vec<char> = s.chars().collect();
                        let st = start.max(0) as usize;
                        if st >= cs.len() {
                            return None;
                        }
                        let end = match len {
                            Some(l) => (st + l.max(0) as usize).min(cs.len()),
                            None => cs.len(),
                        };
                        Some(Item::Val(Value::String(cs[st..end].iter().collect())))
                    }
                    _ => None,
                })
                .collect()
        }
        other => return Err(BaseError::invalid("path.unknown_function", "unknown function").with("function", other)),
    })
}

/// Recognize `<attr>`, `<attr>.toString()` and `<attr>.toString().substring(0, n)`
/// for translation into SQL: (attribute, prefix length).
pub fn simple_prefix(e: &Expr) -> Option<(String, Option<usize>)> {
    match e {
        Expr::Member(None, a) => Some((a.clone(), None)),
        Expr::Call(Some(t), f, args) if f == "toString" && args.is_empty() => match t.as_ref() {
            Expr::Member(None, a) => Some((a.clone(), None)),
            _ => None,
        },
        Expr::Call(Some(t), f, args) if f == "substring" && args.len() == 2 => {
            let (a, None) = simple_prefix(t)? else { return None };
            match (&args[0], &args[1]) {
                (Expr::Lit(Value::Number(s)), Expr::Lit(Value::Number(n))) if s.as_i64() == Some(0) => Some((a, Some(n.as_u64()? as usize))),
                _ => None,
            }
        }
        _ => None,
    }
}

/// A grouping rule: an attribute, optionally `.toString()`, `.substring(0, n)` and finally
/// `.upper()` or `.lower()`. Only such rules are allowed in `groupBy`; they always run in SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupKey {
    pub attr: String,
    pub len: Option<usize>,
    pub case: Option<&'static str>,
}

pub fn group_key(e: &Expr) -> Option<GroupKey> {
    match e {
        Expr::Call(Some(t), f, args) if (f == "upper" || f == "lower") && args.is_empty() => {
            let mut k = group_key(t)?;
            if k.case.is_some() {
                return None;
            }
            k.case = Some(if f == "upper" { "upper" } else { "lower" });
            Some(k)
        }
        _ => simple_prefix(e).map(|(attr, len)| GroupKey { attr, len, case: None }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    /// In-memory graph: object -> name -> items; types per object.
    struct Mem {
        members: HashMap<(Uuid, String), Vec<Item>>,
        types: HashMap<Uuid, Vec<String>>,
    }
    impl Nav for Mem {
        fn member<'a>(&'a self, focus: &'a Item, name: &'a str) -> BoxFut<'a, Result<Vec<Item>>> {
            Box::pin(async move {
                Ok(match focus {
                    Item::Obj(id) => self.members.get(&(*id, name.to_string())).cloned().unwrap_or_default(),
                    _ => vec![],
                })
            })
        }
        fn is_type<'a>(&'a self, obj: Uuid, t: &'a str) -> BoxFut<'a, Result<bool>> {
            Box::pin(async move { Ok(self.types.get(&obj).map(|v| v.iter().any(|x| x == t)).unwrap_or(false)) })
        }
    }

    fn block<F: Future>(f: F) -> F::Output {
        // minimal executor: the in-memory navigator never pends
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
        fn noop(_: *const ()) {}
        fn clone(p: *const ()) -> RawWaker {
            RawWaker::new(p, &VT)
        }
        static VT: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VT)) };
        let mut cx = Context::from_waker(&waker);
        let mut f = Box::pin(f);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    fn fixture() -> (Mem, Uuid) {
        let (asm, p1, p2) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut m = Mem { members: HashMap::new(), types: HashMap::new() };
        m.members.insert((asm, "parts".into()), vec![Item::Obj(p1), Item::Obj(p2)]);
        m.members.insert((asm, "name".into()), vec![Item::Val(json!("Gearbox"))]);
        m.members.insert((asm, "built".into()), vec![Item::Val(json!("2026-11-24T07:15:00+00:00"))]);
        m.members.insert((p1, "number".into()), vec![Item::Val(json!("A-1"))]);
        m.members.insert((p1, "qty".into()), vec![Item::Val(json!(2))]);
        m.members.insert((p1, "price".into()), vec![Item::Val(json!({"amount": "1.50", "currency": "EUR"}))]);
        m.members.insert((p2, "number".into()), vec![Item::Val(json!("B-2"))]);
        m.members.insert((p2, "qty".into()), vec![Item::Val(json!(5))]);
        m.types.insert(p1, vec!["x:Screw".into()]);
        m.types.insert(p2, vec!["x:Nut".into()]);
        (m, asm)
    }

    fn run(src: &str) -> Vec<Value> {
        let (m, asm) = fixture();
        let e = parse(src).unwrap();
        block(eval(&m, &e, &[Item::Obj(asm)])).unwrap().iter().map(|i| i.to_json()).collect()
    }

    #[test]
    fn flattening_and_filters() {
        assert_eq!(run("parts.number"), vec![json!("A-1"), json!("B-2")]);
        assert_eq!(run("parts.where(qty > 3).number"), vec![json!("B-2")]);
        assert_eq!(run("parts.where(number = 'A-1').qty"), vec![json!(2)]);
        assert_eq!(run("parts.ofType(`x:Nut`).number"), vec![json!("B-2")]);
        assert_eq!(run("parts.count()"), vec![json!(2)]);
        assert_eq!(run("parts.price.currency"), vec![json!("EUR")]);
        assert_eq!(run("parts.first().number"), vec![json!("A-1")]);
    }

    #[test]
    fn empty_paths_and_three_valued_logic() {
        assert!(run("nothing.at.all").is_empty());
        assert_eq!(run("nothing.exists()"), vec![json!(false)]);
        assert!(run("nothing = 'x'").is_empty(), "comparison with empty is empty");
        assert_eq!(run("nothing = 'x' or name = 'Gearbox'"), vec![json!(true)]);
        assert_eq!(run("name = 'Gearbox' and parts.count() > 1"), vec![json!(true)]);
        assert_eq!(run("(name = 'x').not()"), vec![json!(true)]);
    }

    #[test]
    fn strings() {
        assert_eq!(run("built.toString().substring(0, 10)"), vec![json!("2026-11-24")]);
        assert_eq!(run("name.upper()"), vec![json!("GEARBOX")]);
    }

    #[test]
    fn syntax_and_profile() {
        assert!(parse("a.b(").is_err());
        assert!(parse("a..b").is_err());
        assert!(parse("`inventar:kundennummer`").is_ok());
        let e = parse("a.repeat(b)").unwrap();
        assert_eq!(check_profile(&e, VIEW_FUNCTIONS, "a.repeat(b)").unwrap_err().error_id, "path.function_not_allowed");
        assert_eq!(first_name(&parse("attachments.where(x = 1).filename").unwrap()), Some("attachments"));
        assert_eq!(simple_prefix(&parse("receivedAt.toString().substring(0, 7)").unwrap()), Some(("receivedAt".into(), Some(7))));
        assert_eq!(simple_prefix(&parse("receivedAt").unwrap()), Some(("receivedAt".into(), None)));
        assert_eq!(simple_prefix(&parse("subject.substring(0, 1)").unwrap()), Some(("subject".into(), Some(1))));
        let k = group_key(&parse("subject.substring(0, 1).upper()").unwrap()).unwrap();
        assert_eq!((k.attr.as_str(), k.len, k.case), ("subject", Some(1), Some("upper")));
        assert_eq!(group_key(&parse("subject.upper().lower()").unwrap()), None);
        assert_eq!(group_key(&parse("attachments.filename").unwrap()), None);
        assert_eq!(group_key(&parse("subject.count()").unwrap()), None);
    }
}
