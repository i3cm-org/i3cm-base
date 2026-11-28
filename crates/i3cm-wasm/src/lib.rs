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
use std::collections::{BTreeSet, HashMap};
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
#[derive(Clone)]
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
    /// Room tree of the open transaction, once an operation changed it: every frame, handle and
    /// hook of the order works on this one (rooms created anywhere stay known everywhere).
    rooms: std::sync::Mutex<Option<i3cm_store::context::RoomTree>>,
    /// Bound filters an object written in this order belonged to when the order first touched
    /// it (empty for objects created in it), later the state last reported to filter hooks.
    trans: std::sync::Mutex<HashMap<Uuid, BTreeSet<usize>>>,
}

impl Shared {
    /// Take the order's current room tree into `ctx`.
    fn rooms_in(&self, ctx: &mut Ctx) {
        if let Some(r) = self.rooms.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            ctx.rooms = r.clone();
        }
    }
    /// Publish the room tree of `ctx` as the order's current one.
    fn rooms_out(&self, ctx: &Ctx) {
        *self.rooms.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctx.rooms.clone());
    }
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
    /// Named filters that hooks are bound to, with those hooks.
    bound: Vec<Bound>,
}

/// A named filter bound to filter hooks (`on: enter | leave`).
struct Bound {
    name: String,
    type_name: String,
    filter: i3cm_core::paths::Filter,
    hooks: Vec<(String, HookDef)>,
}

/// The named filters of all filter hooks of the catalog, parsed once.
fn bound_filters(model: &Model) -> IResult<Vec<Bound>> {
    let mut out: Vec<Bound> = vec![];
    for m in &model.catalog.modules {
        for h in &m.hooks {
            let Some(name) = &h.filter else { continue };
            let n = name.strip_prefix('@').unwrap_or(name);
            let def = model.named_filter(n, &m.name).ok_or_else(|| BaseError::internal("hook names an unknown filter").with("filter", name.clone()))?;
            if let Some(b) = out.iter_mut().find(|b| b.name == def.name) {
                b.hooks.push((m.name.clone(), h.clone()));
                continue;
            }
            let ns = def.name.split(':').next().unwrap_or(&m.name);
            let filter = i3cm_core::paths::filter_in(model, &def.type_name, &def.filter, i3cm_core::paths::Origin::Catalog, ns, &BTreeSet::new())?;
            out.push(Bound { name: def.name.clone(), type_name: def.type_name.clone(), filter, hooks: vec![(m.name.clone(), h.clone())] });
        }
    }
    Ok(out)
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
        let bound = bound_filters(&store.model)?;
        let inner = Arc::new(Inner { engine, pres, model: store.model.clone(), ticks, bound });
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
        let mut res = run_frame(self.inner.clone(), order.clone(), ctx.clone(), module.to_string(), op.to_string(), 0, true, params.to_string()).await;
        if let Ok(Ok(_)) = &res {
            let mut c = ctx.clone();
            if let Err(e) = settle(&self.inner, &order, &mut c).await {
                res = Ok(Err(err(e)));
            }
        }
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
    Arc::new(Shared {
        store: store.clone(),
        session: Mutex::new(Some(session)),
        deadline_tick: now + ORDER_TIMEOUT.as_millis() as u64 / TICK_MS,
        rooms: std::sync::Mutex::new(None),
        trans: std::sync::Mutex::new(HashMap::new()),
    })
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
        settle(inner, &shared, ctx).await
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

/// What a hook learns about an operation: event, types of the object, object, text type.
type HookEvent = (&'static str, Vec<String>, Option<Uuid>, Option<String>);

/// Which (event, types) an operation touches, for hooks.
async fn hook_event(shared: &Shared, ctx: &Ctx, op: &Op) -> IResult<Option<HookEvent>> {
    let id_types = |id: &str| -> IResult<Uuid> { Uuid::parse_str(id).map_err(|_| BaseError::invalid("order.unknown_ref", "neither a UUID nor a local reference").with("id", id)) };
    let types_of = |uid: Uuid| async move {
        let g = shared.session.lock().await;
        let s = g.as_ref().ok_or_else(|| BaseError::internal("order lost its session"))?;
        shared.store.types_in(s.client(), ctx.tenant, uid).await
    };
    // contents and texts of objects created earlier in the order count as well
    let resolve = |id: String| async move { shared.session.lock().await.as_ref().and_then(|s| s.resolve_id(&id)) };
    Ok(match op {
        Op::Create { types, .. } => Some(("create", types.keys().cloned().collect(), None, None)),
        Op::Update { id, .. } | Op::Delete { id } => {
            let Ok(uid) = id_types(id) else { return Ok(None) };
            Some((if matches!(op, Op::Delete { .. }) { "delete" } else { "update" }, types_of(uid).await?, Some(uid), None))
        }
        Op::Content { id, .. } | Op::SetContent { id, .. } => {
            let Some(uid) = resolve(id.clone()).await else { return Ok(None) };
            Some(("content", types_of(uid).await?, Some(uid), None))
        }
        Op::Text { id, text_type, .. } | Op::SetText { id, text_type, .. } => {
            let Some(uid) = resolve(id.clone()).await else { return Ok(None) };
            Some(("text", types_of(uid).await?, Some(uid), Some(text_type.clone())))
        }
        _ => None,
    })
}

fn hooks_for<'a>(model: &'a Model, on: &str, phase: &str, types: &[String], text_type: Option<&str>) -> Vec<(&'a str, &'a HookDef)> {
    let mut out = vec![];
    for m in &model.catalog.modules {
        for h in &m.hooks {
            if h.filter.is_some() || h.on != on || h.phase != phase {
                continue;
            }
            if let (Some(want), Some(tt)) = (&h.text_type, text_type)
                && want != tt
            {
                continue;
            }
            if types.iter().any(|t| t == &h.type_name || model.is_a(t, &h.type_name)) {
                out.push((m.name.as_str(), h));
            }
        }
    }
    out
}

/// The object an operation changes, if it already exists (for filter transitions).
fn target_of(op: &Op) -> Option<&str> {
    match op {
        Op::Update { id, .. }
        | Op::Attach { id, .. }
        | Op::Detach { id, .. }
        | Op::Delete { id }
        | Op::Move { id, .. }
        | Op::Content { id, .. }
        | Op::SetContent { id, .. }
        | Op::Text { id, .. }
        | Op::SetText { id, .. } => Some(id),
        _ => None,
    }
}

/// The bound filters object `id` belongs to now, as the open transaction sees it.
async fn membership(rt: &Inner, shared: &Shared, ctx: &Ctx, id: Uuid) -> IResult<BTreeSet<usize>> {
    let g = shared.session.lock().await;
    let s = g.as_ref().ok_or_else(|| BaseError::internal("order lost its session"))?;
    let mut out = BTreeSet::new();
    for (i, b) in rt.bound.iter().enumerate() {
        if shared.store.object_matches(s.client(), ctx, &b.type_name, &b.filter, id).await? {
            out.insert(i);
        }
    }
    Ok(out)
}

/// Before an operation changes an existing object for the first time in the order: remember
/// which bound filters it belongs to.
async fn note_before(rt: &Inner, shared: &Shared, ctx: &Ctx, op: &Op) -> IResult<()> {
    if rt.bound.is_empty() {
        return Ok(());
    }
    let Some(id) = target_of(op) else { return Ok(()) };
    let Some(uid) = shared.session.lock().await.as_ref().and_then(|s| s.resolve_id(id)) else { return Ok(()) };
    if shared.trans.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&uid) {
        return Ok(());
    }
    let m = membership(rt, shared, ctx, uid).await?;
    shared.trans.lock().unwrap_or_else(|e| e.into_inner()).entry(uid).or_insert(m);
    Ok(())
}

/// At the end of an order: run the filter hooks of every object written in it that entered or
/// left a bound filter (state at first touch against now); what they write is evaluated again,
/// up to the depth limit.
async fn settle(rt: &Arc<Inner>, shared: &Arc<Shared>, ctx: &mut Ctx) -> IResult<()> {
    if rt.bound.is_empty() {
        return Ok(());
    }
    for round in 0..MAX_DEPTH {
        let ids: Vec<(Uuid, BTreeSet<usize>)> = shared.trans.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(k, v)| (*k, v.clone())).collect();
        let mut fired = false;
        for (id, before) in ids {
            let now = membership(rt, shared, ctx, id).await?;
            if now == before {
                continue;
            }
            shared.trans.lock().unwrap_or_else(|e| e.into_inner()).insert(id, now.clone());
            for (on, set) in [("enter", now.difference(&before).copied().collect::<Vec<_>>()), ("leave", before.difference(&now).copied().collect::<Vec<_>>())] {
                for i in set {
                    let b = &rt.bound[i];
                    let run: Vec<&(String, HookDef)> = b.hooks.iter().filter(|(m, h)| h.on == on && !ctx.inactive.contains(m)).collect();
                    for (module, h) in run {
                        fired = true;
                        let params = json!({ "phase": "after", "on": on, "filter": b.name, "type": b.type_name, "id": id });
                        hook_frame(rt, shared, ctx, module, &h.op, round, true, params).await?;
                        shared.rooms_in(ctx);
                    }
                }
            }
        }
        if !fired {
            return Ok(());
        }
    }
    Err(BaseError::invalid("module.depth", "filter hooks keep changing objects (depth limit)"))
}

/// Apply one operation of an order, with the hooks around it, in the identity of the order.
/// Before hooks get read-only handles and may reject the order; after hooks may write in the
/// same order. Any failing hook aborts the order.
async fn apply_with_hooks(rt: &Arc<Inner>, shared: &Arc<Shared>, ctx: &mut Ctx, op: &Op, depth: u32) -> IResult<Option<Uuid>> {
    shared.rooms_in(ctx);
    let ev = hook_event(shared, ctx, op).await?;
    let op_json = serde_json::to_value(op).unwrap_or(Value::Null);
    if let Some((on, types, id, tt)) = &ev {
        for (module, h) in hooks_for(&rt.model, on, "before", types, tt.as_deref()) {
            let params = json!({ "phase": "before", "on": on, "type": h.type_name, "id": id, "textType": tt, "op": op_json });
            hook_frame(rt, shared, ctx, module, &h.op, depth, false, params).await?;
        }
    }
    note_before(rt, shared, ctx, op).await?;
    let created = {
        let mut g = shared.session.lock().await;
        let s = g.as_mut().ok_or_else(|| BaseError::internal("order lost its session"))?;
        let r = s.apply(&shared.store, ctx, op).await;
        // also after a failed operation: what it did stays in the open transaction
        shared.rooms_out(ctx);
        r?
    };
    if let Some(c) = created {
        shared.trans.lock().unwrap_or_else(|e| e.into_inner()).entry(c).or_default();
    }
    if let Some((on, types, id, tt)) = &ev {
        let id = id.or(created);
        for (module, h) in hooks_for(&rt.model, on, "after", types, tt.as_deref()) {
            let params = json!({ "phase": "after", "on": on, "type": h.type_name, "id": id, "textType": tt, "op": op_json });
            hook_frame(rt, shared, ctx, module, &h.op, depth, true, params).await?;
            shared.rooms_in(ctx);
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
    /// The handle, with the order's current room tree (rooms created by other frames, hooks or
    /// handles of the same order are known to reads and writes alike).
    fn conn(&self, r: &Resource<ConnRep>) -> wasmtime::Result<ConnRep> {
        let mut c = self.table.get(r)?.clone();
        self.order.rooms_in(&mut c.ctx);
        Ok(c)
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
            let (items, next) = match self.order.store.query_in(sess.client(), &ctx, &t, w, after, limit, "en").await {
                Ok(r) => r,
                Err(e) => return Ok(Err(err(e))),
            };
            let mut out = vec![];
            for v in items {
                let mut o = serde_json::to_value(&v).unwrap_or(Value::Null);
                if f.get("references").and_then(|r| r.as_bool()).unwrap_or(false) {
                    match self.order.store.references_in(sess.client(), &ctx, v.id, REFS_PER_END).await {
                        Ok((refs, truncated)) => {
                            if let Some(m) = o.as_object_mut() {
                                m.insert("references".into(), json!(refs));
                                if !truncated.is_empty() {
                                    m.insert("referencesTruncated".into(), json!(truncated));
                                }
                            }
                        }
                        Err(e) => return Ok(Err(err(e))),
                    }
                }
                out.push(o);
            }
            return Ok(Ok(json!({ "items": out, "next": next }).to_string()));
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
        // rooms created or moved anywhere in this order (other calls, hooks, other handles)
        self.order.rooms_in(&mut ctx);
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
                Err(e) => {
                    self.table.get_mut(&s)?.ctx.rooms = ctx.rooms.clone();
                    return Ok(Err(err(e.with("op_index", i))));
                }
            }
            let mut g = self.order.session.lock().await;
            if let Some(sess) = g.as_mut() {
                let target = serde_json::to_value(op).ok().and_then(|v| v.get("op").and_then(|x| x.as_str()).map(String::from));
                let _ = sess.audit(&ctx, depth as i32, Some(&module), Some(&op_name), "order", target.as_deref()).await;
            }
        }
        // rooms created or moved by this call stay known to later calls of the same operation
        self.table.get_mut(&s)?.ctx.rooms = ctx.rooms.clone();
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
        // on the order's own connection: the order holds its tenant's write turn, and a second
        // connection could wait behind writers that wait for that turn
        let mut g = self.order.session.lock().await;
        let looked_up = match g.as_ref() {
            Some(sess) => match store.module_carrier_on(sess.client(), tenant, &module, &carrier).await {
                Ok(a) => store.ctx_on(sess.client(), tenant, a).await,
                Err(e) => Err(e),
            },
            None => match store.module_carrier(tenant, &module, &carrier).await {
                Ok(a) => store.ctx(tenant, a).await,
                Err(e) => Err(e),
            },
        };
        let ctx = match looked_up {
            Ok(c) => c,
            Err(e) => return Ok(Err(err(e))),
        };
        if let Some(sess) = g.as_mut() {
            let _ = sess.audit(&from, depth as i32, Some(&module), Some(&op), "impersonate", Some(&carrier)).await;
        }
        drop(g);
        Ok(Ok(self.table.push(ConnRep { ctx, module, op, depth, writable })?))
    }

    async fn drop(&mut self, rep: Resource<ConnRep>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}
