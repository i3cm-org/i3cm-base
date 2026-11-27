//! Module code as WebAssembly components inside the server.
//!
//! One order is one database transaction (a [`Session`]); every call into module code is a
//! frame with a fresh store and instance and its own `connection` handle. Writes go straight
//! into the transaction; any error or trap aborts the whole order (no savepoints). `call` runs
//! through the kernel with the identity of the calling handle; `impersonate` hands out a new
//! handle for a carrier of the same module, only for operations marked for it. Hooks of the
//! phase `before` get read-only handles; hooks of the phase `after` write in the same order with
//! the identity of the order (their writes run hooks again, up to the depth limit).

use i3cm_core::catalog::{HookDef, Model};
use i3cm_core::{BaseError, Result as IResult};
use i3cm_store::context::Ctx;
use i3cm_store::order::{Op, Order, OrderResult};
use i3cm_store::session::{Audit, Session};
use i3cm_store::Store;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use uuid::Uuid;
use wasmtime::component::{Component, Linker, Resource, ResourceTable};
use wasmtime::{Config, Engine};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "module",
    imports: { default: async | trappable },
    exports: { default: async },
    with: { "i3cm:module/host.connection": ConnRep },
});

use i3cm::module::host::{Host, HostConnection};

pub mod schedule;

/// Milliseconds per epoch tick; the order deadline is counted in ticks.
const TICK_MS: u64 = 10;
/// Hard deadline of an order.
pub const ORDER_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DEPTH: u32 = 16;
/// Targets per reference end reported by `get`; more are read with `ends`.
const REFS_PER_END: i64 = 100;

/// A `connection` handle: identity (its context), the frame it belongs to, read or write.
pub struct ConnRep {
    ctx: Ctx,
    module: String,
    op: String,
    depth: u32,
    writable: bool,
}

/// The order shared by all frames.
struct Shared {
    store: Store,
    session: Mutex<Option<Session>>,
    deadline_tick: u64,
}

struct HostState {
    table: ResourceTable,
    wasi: WasiCtx,
    rt: Arc<Inner>,
    order: Arc<Shared>,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

struct Inner {
    engine: Engine,
    pres: HashMap<String, ModulePre<HostState>>,
    model: Arc<Model>,
    ticks: Arc<AtomicU64>,
}

/// The runtime: compiled components of all modules that have operations.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
    store: Store,
}

fn err(e: BaseError) -> String {
    serde_json::to_string(&e).unwrap_or_else(|_| e.description.clone())
}

impl Runtime {
    /// Load `<dir>/<module>.wasm` for every module of the catalog that declares operations.
    pub fn new(store: Store, dir: &Path) -> IResult<Runtime> {
        let mut cfg = Config::new();
        cfg.epoch_interruption(true);
        let engine = Engine::new(&cfg).map_err(|e| BaseError::internal(format!("wasm engine: {e}")))?;
        let mut linker: Linker<HostState> = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(|e| BaseError::internal(format!("wasi: {e}")))?;
        Module::add_to_linker::<_, wasmtime::component::HasSelf<_>>(&mut linker, |s| s).map_err(|e| BaseError::internal(format!("linker: {e}")))?;
        let mut pres = HashMap::new();
        for m in &store.model.catalog.modules {
            if m.operations.is_empty() {
                continue;
            }
            let path = dir.join(format!("{}.wasm", m.name));
            let bytes = std::fs::read(&path).map_err(|e| {
                BaseError::internal(format!("module code missing: {e}")).with("module", m.name.clone()).with("path", path.display().to_string())
            })?;
            let comp = Component::new(&engine, &bytes).map_err(|e| BaseError::internal(format!("module code invalid: {e}")).with("module", m.name.clone()))?;
            let pre = linker.instantiate_pre(&comp).and_then(ModulePre::new).map_err(|e| BaseError::internal(format!("module code does not fit the kernel: {e}")).with("module", m.name.clone()))?;
            pres.insert(m.name.clone(), pre);
        }
        let ticks = Arc::new(AtomicU64::new(0));
        {
            let engine = engine.clone();
            let ticks = ticks.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(TICK_MS));
                ticks.fetch_add(1, Ordering::SeqCst);
                engine.increment_epoch();
            });
        }
        let inner = Arc::new(Inner { engine, pres, model: store.model.clone(), ticks });
        // every order of the base (shortcuts, files of aspects, tools) runs the hooks as well
        store.set_order_runner(Arc::new(HookRunner { inner: inner.clone() }));
        Ok(Runtime { inner, store })
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    fn shared(&self, session: Session) -> Arc<Shared> {
        shared_with(&self.inner, &self.store, session)
    }

    fn declared(&self, module: &str, op: &str) -> bool {
        self.inner.model.catalog.modules.iter().any(|m| m.name == module && m.operations.iter().any(|o| o.name == op))
    }

    /// Run an operation of a module as one order in the caller's identity.
    pub async fn run_op(&self, ctx: &Ctx, module: &str, op: &str, params: &Value, tool: Option<&str>) -> IResult<Value> {
        if !self.declared(module, op) || !self.inner.pres.contains_key(module) {
            return Err(BaseError::not_found("module.unknown_operation", "no such operation").with("module", module).with("op", op));
        }
        let audit = Audit { module: Some(module.into()), function: Some(op.into()), tool: tool.map(Into::into) };
        let session = self.store.begin(ctx, &audit).await?;
        self.run_in(session, ctx, module, op, params).await
    }

    /// Run an operation inside an already open session (scheduler: the claim is part of it).
    pub(crate) async fn run_in(&self, session: Session, ctx: &Ctx, module: &str, op: &str, params: &Value) -> IResult<Value> {
        let order = self.shared(session);
        let res = run_frame(self.inner.clone(), order.clone(), ctx.clone(), module.to_string(), op.to_string(), 0, true, params.to_string()).await;
        let session = order.session.lock().await.take();
        let Some(session) = session else { return Err(BaseError::internal("order lost its session")) };
        match res {
            Ok(Ok(out)) => {
                let mut c = ctx.clone();
                session.finish(&self.store, &mut c, true).await?;
                Ok(serde_json::from_str(&out).unwrap_or(Value::String(out)))
            }
            Ok(Err(e)) => {
                session.abort().await;
                Err(module_error(&e).with("module", module).with("op", op))
            }
            Err(trap) => {
                session.abort().await;
                let timeout = is_timeout(&trap);
                Err(BaseError::invalid(if timeout { "module.timeout" } else { "module.trap" }, "module code failed, order aborted")
                    .with("module", module)
                    .with("op", op)
                    .with("detail", format!("{trap:#}")))
            }
        }
    }

    /// A generic order with hooks: before hooks may reject, after hooks run in the same order.
    pub async fn execute(&self, ctx: &mut Ctx, order: &Order) -> IResult<OrderResult> {
        execute_with(&self.inner, &self.store, ctx, order).await
    }
}

fn shared_with(inner: &Inner, store: &Store, session: Session) -> Arc<Shared> {
    let now = inner.ticks.load(Ordering::SeqCst);
    Arc::new(Shared { store: store.clone(), session: Mutex::new(Some(session)), deadline_tick: now + ORDER_TIMEOUT.as_millis() as u64 / TICK_MS })
}

async fn execute_with(inner: &Arc<Inner>, store: &Store, ctx: &mut Ctx, order: &Order) -> IResult<OrderResult> {
    if order.ops.is_empty() {
        return Err(BaseError::invalid("order.empty", "order without operations"));
    }
    let rooms_before = ctx.rooms.clone();
    let session = store.begin(ctx, &Audit::default()).await?;
    let shared = shared_with(inner, store, session);
    let res = async {
        for (i, op) in order.ops.iter().enumerate() {
            apply_with_hooks(inner, &shared, ctx, op, 0).await.map_err(|e| e.with("op_index", i))?;
        }
        Ok::<_, BaseError>(())
    }
    .await;
    let session = shared.session.lock().await.take().ok_or_else(|| BaseError::internal("order lost its session"))?;
    let out = match res {
        Ok(()) => session.finish(store, ctx, !order.dry_run).await,
        Err(e) => {
            session.abort().await;
            Err(e)
        }
    };
    if out.is_err() || order.dry_run {
        ctx.rooms = rooms_before;
    }
    out
}

/// The hooks of module code for orders that reach the base by other ways than `POST /v1/orders`.
struct HookRunner {
    inner: Arc<Inner>,
}

impl i3cm_store::OrderRunner for HookRunner {
    fn run<'a>(&'a self, store: &'a Store, ctx: &'a mut Ctx, order: &'a Order) -> std::pin::Pin<Box<dyn std::future::Future<Output = IResult<OrderResult>> + Send + 'a>> {
        Box::pin(execute_with(&self.inner, store, ctx, order))
    }
}

/// An error string returned by module code: a JSON error object or plain text.
fn module_error(e: &str) -> BaseError {
    match serde_json::from_str::<BaseError>(e) {
        Ok(b) => b,
        Err(_) => BaseError::invalid("module.error", "module code rejected the order").with("detail", e.to_string()),
    }
}

fn is_timeout(e: &wasmtime::Error) -> bool {
    matches!(e.downcast_ref::<wasmtime::Trap>(), Some(wasmtime::Trap::Interrupt))
}

/// Which (event, types) an operation touches, for hooks.
async fn hook_event(shared: &Shared, ctx: &Ctx, op: &Op) -> IResult<Option<(&'static str, Vec<String>, Option<Uuid>)>> {
    let id_types = |id: &str| -> IResult<Uuid> { Uuid::parse_str(id).map_err(|_| BaseError::invalid("order.unknown_ref", "neither a UUID nor a local reference").with("id", id)) };
    Ok(match op {
        Op::Create { types, .. } => Some(("create", types.keys().cloned().collect(), None)),
        Op::Update { id, .. } | Op::Delete { id } => {
            let Ok(uid) = id_types(id) else { return Ok(None) };
            let g = shared.session.lock().await;
            let s = g.as_ref().ok_or_else(|| BaseError::internal("order lost its session"))?;
            let set = shared.store.types_in(s.client(), ctx.tenant, uid).await?;
            Some((if matches!(op, Op::Delete { .. }) { "delete" } else { "update" }, set, Some(uid)))
        }
        _ => None,
    })
}

fn hooks_for<'a>(model: &'a Model, on: &str, phase: &str, types: &[String]) -> Vec<(&'a str, &'a HookDef)> {
    let mut out = vec![];
    for m in &model.catalog.modules {
        for h in &m.hooks {
            if h.on == on && h.phase == phase && types.iter().any(|t| t == &h.type_name || model.is_a(t, &h.type_name)) {
                out.push((m.name.as_str(), h));
            }
        }
    }
    out
}

/// Apply one operation of an order, with the hooks around it, in the identity of the order.
/// Before hooks get read-only handles and may reject the order; after hooks may write in the
/// same order. Any failing hook aborts the order.
async fn apply_with_hooks(rt: &Arc<Inner>, shared: &Arc<Shared>, ctx: &mut Ctx, op: &Op, depth: u32) -> IResult<Option<Uuid>> {
    let ev = hook_event(shared, ctx, op).await?;
    let op_json = serde_json::to_value(op).unwrap_or(Value::Null);
    if let Some((on, types, id)) = &ev {
        for (module, h) in hooks_for(&rt.model, on, "before", types) {
            let params = json!({ "phase": "before", "on": on, "type": h.type_name, "id": id, "op": op_json });
            hook_frame(rt, shared, ctx, module, &h.op, depth, false, params).await?;
        }
    }
    let created = {
        let mut g = shared.session.lock().await;
        let s = g.as_mut().ok_or_else(|| BaseError::internal("order lost its session"))?;
        s.apply(&shared.store, ctx, op).await?
    };
    if let Some((on, types, id)) = &ev {
        let id = id.or(created);
        for (module, h) in hooks_for(&rt.model, on, "after", types) {
            let params = json!({ "phase": "after", "on": on, "type": h.type_name, "id": id, "op": op_json });
            hook_frame(rt, shared, ctx, module, &h.op, depth, true, params).await?;
        }
    }
    Ok(created)
}

#[allow(clippy::too_many_arguments)]
async fn hook_frame(rt: &Arc<Inner>, shared: &Arc<Shared>, ctx: &Ctx, module: &str, op: &str, depth: u32, writable: bool, params: Value) -> IResult<()> {
    if depth + 1 >= MAX_DEPTH {
        return Err(BaseError::invalid("module.depth", "call depth limit").with("hook", format!("{module}:{op}")));
    }
    match Box::pin(run_frame(rt.clone(), shared.clone(), ctx.clone(), module.into(), op.into(), depth + 1, writable, params.to_string())).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(module_error(&e).with("hook", format!("{module}:{op}"))),
        Err(trap) => Err(BaseError::invalid("module.trap", "hook failed, order aborted").with("hook", format!("{module}:{op}")).with("detail", format!("{trap:#}"))),
    }
}

/// One frame: fresh store and instance of the module, a new handle, call `run`.
#[allow(clippy::too_many_arguments)]
async fn run_frame(rt: Arc<Inner>, order: Arc<Shared>, ctx: Ctx, module: String, op: String, depth: u32, writable: bool, params: String) -> wasmtime::Result<std::result::Result<String, String>> {
    let pre = rt.pres.get(&module).ok_or_else(|| wasmtime::format_err!("no code for module {module}"))?.clone();
    let mut store = wasmtime::Store::new(&rt.engine, HostState { table: ResourceTable::new(), wasi: WasiCtx::builder().build(), rt: rt.clone(), order: order.clone() });
    let now = rt.ticks.load(Ordering::SeqCst);
    store.set_epoch_deadline(order.deadline_tick.saturating_sub(now).max(1));
    store.epoch_deadline_trap();
    let inst = pre.instantiate_async(&mut store).await?;
    {
        let mut g = order.session.lock().await;
        if let Some(s) = g.as_mut() {
            let _ = s.audit(&ctx, depth as i32, Some(&module), Some(&op), "run", None).await;
        }
    }
    let conn = store.data_mut().table.push(ConnRep { ctx, module, op: op.clone(), depth, writable })?;
    inst.call_run(&mut store, conn, &op, &params).await
}

impl Host for HostState {}

impl HostState {
    fn conn(&self, r: &Resource<ConnRep>) -> wasmtime::Result<&ConnRep> {
        Ok(self.table.get(r)?)
    }
}

impl HostConnection for HostState {
    async fn me(&mut self, s: Resource<ConnRep>) -> wasmtime::Result<String> {
        Ok(self.conn(&s)?.ctx.me().to_string())
    }

    async fn get(&mut self, s: Resource<ConnRep>, id: String) -> wasmtime::Result<std::result::Result<String, String>> {
        let ctx = self.conn(&s)?.ctx.clone();
        let Ok(uid) = Uuid::parse_str(&id) else { return Ok(Err(err(BaseError::invalid("object.bad_id", "not a UUID")))) };
        let g = self.order.session.lock().await;
        let Some(sess) = g.as_ref() else { return Ok(Err("order lost its session".into())) };
        let v = match self.order.store.view_in(sess.client(), &ctx, uid, "en").await {
            Ok(v) => v,
            Err(e) => return Ok(Err(err(e))),
        };
        let (refs, truncated) = match self.order.store.references_in(sess.client(), &ctx, uid, REFS_PER_END).await {
            Ok(r) => r,
            Err(e) => return Ok(Err(err(e))),
        };
        let mut out = serde_json::to_value(&v).unwrap_or(Value::Null);
        if let Some(o) = out.as_object_mut() {
            o.insert("references".into(), json!(refs));
            if !truncated.is_empty() {
                o.insert("referencesTruncated".into(), json!(truncated));
            }
        }
        Ok(Ok(out.to_string()))
    }

    async fn ends(&mut self, s: Resource<ConnRep>, id: String, end: String, offset: u32, limit: u32) -> wasmtime::Result<std::result::Result<String, String>> {
        let ctx = self.conn(&s)?.ctx.clone();
        let Ok(uid) = Uuid::parse_str(&id) else { return Ok(Err(err(BaseError::invalid("object.bad_id", "not a UUID")))) };
        let g = self.order.session.lock().await;
        let Some(sess) = g.as_ref() else { return Ok(Err("order lost its session".into())) };
        Ok(match self.order.store.end_ids_in(sess.client(), &ctx, uid, &end, offset as i64, limit.clamp(1, 500) as i64).await {
            Ok(ids) => Ok(json!(ids).to_string()),
            Err(e) => Err(err(e)),
        })
    }

    async fn query(&mut self, s: Resource<ConnRep>, t: String, filter: String) -> wasmtime::Result<std::result::Result<String, String>> {
        let ctx = self.conn(&s)?.ctx.clone();
        let f: Map<String, Value> = match serde_json::from_str(&filter) {
            Ok(Value::Object(m)) => m,
            _ if filter.trim().is_empty() => Map::new(),
            _ => return Ok(Err(err(BaseError::invalid("query.bad_filter", "filter must be a JSON object")))),
        };
        let g = self.order.session.lock().await;
        let Some(sess) = g.as_ref() else { return Ok(Err("order lost its session".into())) };
        if let Some(w) = f.get("where") {
            let Some(w) = w.as_str() else { return Ok(Err(err(BaseError::invalid("query.bad_filter", "where must be a string")))) };
            let after = match f.get("after") {
                None | Some(Value::Null) => None,
                Some(Value::String(a)) => match Uuid::parse_str(a) {
                    Ok(u) => Some(u),
                    Err(_) => return Ok(Err(err(BaseError::invalid("query.bad_after", "after must be an object id")))),
                },
                Some(_) => return Ok(Err(err(BaseError::invalid("query.bad_after", "after must be an object id")))),
            };
            let limit = f.get("limit").and_then(|l| l.as_i64()).unwrap_or(100);
            return Ok(match self.order.store.query_in(sess.client(), &ctx, &t, w, after, limit, "en").await {
                Ok((items, next)) => Ok(json!({ "items": items, "next": next }).to_string()),
                Err(e) => Err(err(e)),
            });
        }
        Ok(match self.order.store.find_in(sess.client(), &ctx, &t, &f, 200, "en").await {
            Ok(v) => Ok(serde_json::to_string(&v).unwrap_or_default()),
            Err(e) => Err(err(e)),
        })
    }

    async fn order(&mut self, s: Resource<ConnRep>, ops: String) -> wasmtime::Result<std::result::Result<String, String>> {
        let c = self.conn(&s)?;
        if !c.writable {
            return Ok(Err(err(BaseError::invalid("module.read_only", "this handle may only read (hook)").with("module", c.module.clone()).with("op", c.op.clone()))));
        }
        let (mut ctx, depth, module, op_name) = (c.ctx.clone(), c.depth, c.module.clone(), c.op.clone());
        let ops: Vec<Op> = match serde_json::from_str(&ops) {
            Ok(o) => o,
            Err(e) => return Ok(Err(err(BaseError::invalid("order.bad_ops", "ops must be a JSON array of operations").with("detail", e.to_string())))),
        };
        let mut created = Map::new();
        for (i, op) in ops.iter().enumerate() {
            match apply_with_hooks(&self.rt, &self.order, &mut ctx, op, depth).await {
                Ok(Some(id)) => {
                    let key = match op {
                        Op::Create { local: Some(l), .. } => l.clone(),
                        _ => i.to_string(),
                    };
                    created.insert(key, Value::String(id.to_string()));
                }
                Ok(None) => {}
                Err(e) => return Ok(Err(err(e.with("op_index", i)))),
            }
            let mut g = self.order.session.lock().await;
            if let Some(sess) = g.as_mut() {
                let target = serde_json::to_value(op).ok().and_then(|v| v.get("op").and_then(|x| x.as_str()).map(String::from));
                let _ = sess.audit(&ctx, depth as i32, Some(&module), Some(&op_name), "order", target.as_deref()).await;
            }
        }
        Ok(Ok(Value::Object(created).to_string()))
    }

    async fn call(&mut self, s: Resource<ConnRep>, op: String, params: String) -> wasmtime::Result<std::result::Result<String, String>> {
        let c = self.conn(&s)?;
        let Some((module, name)) = op.split_once(':') else { return Ok(Err(err(BaseError::invalid("module.bad_op", "operation must be ns:op")))) };
        if !self.rt.pres.contains_key(module) || !self.rt.model.catalog.modules.iter().any(|m| m.name == module && m.operations.iter().any(|o| o.name == name)) {
            return Ok(Err(err(BaseError::not_found("module.unknown_operation", "no such operation").with("op", op.clone()))));
        }
        if c.depth + 1 >= MAX_DEPTH {
            return Ok(Err(err(BaseError::invalid("module.depth", "call depth limit"))));
        }
        let (ctx, depth, writable) = (c.ctx.clone(), c.depth + 1, c.writable);
        // a trap in the callee aborts the whole order (no savepoints)
        Box::pin(run_frame(self.rt.clone(), self.order.clone(), ctx, module.to_string(), name.to_string(), depth, writable, params)).await
    }

    async fn impersonate(&mut self, s: Resource<ConnRep>, carrier: String) -> wasmtime::Result<std::result::Result<Resource<ConnRep>, String>> {
        let c = self.conn(&s)?;
        let declared = self
            .rt
            .model
            .catalog
            .modules
            .iter()
            .find(|m| m.name == c.module)
            .and_then(|m| m.operations.iter().find(|o| o.name == c.op))
            .map(|o| o.impersonates.contains(&carrier))
            .unwrap_or(false);
        if !declared {
            return Ok(Err(err(BaseError::not_found("module.impersonate_denied", "operation is not marked for this carrier").with("op", format!("{}:{}", c.module, c.op)).with("carrier", carrier))));
        }
        let (tenant, module, op, depth, writable, from) = (c.ctx.tenant, c.module.clone(), c.op.clone(), c.depth, c.writable, c.ctx.clone());
        let store = self.order.store.clone();
        let account = match store.module_carrier(tenant, &module, &carrier).await {
            Ok(a) => a,
            Err(e) => return Ok(Err(err(e))),
        };
        let ctx = match store.ctx(tenant, account).await {
            Ok(c) => c,
            Err(e) => return Ok(Err(err(e))),
        };
        {
            let mut g = self.order.session.lock().await;
            if let Some(sess) = g.as_mut() {
                let _ = sess.audit(&from, depth as i32, Some(&module), Some(&op), "impersonate", Some(&carrier)).await;
            }
        }
        Ok(Ok(self.table.push(ConnRep { ctx, module, op, depth, writable })?))
    }

    async fn drop(&mut self, rep: Resource<ConnRep>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}
