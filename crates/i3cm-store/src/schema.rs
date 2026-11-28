//! Database schema: system tables plus one table per type and per reference (generated
//! from the catalog). All statements are idempotent.

use i3cm_core::catalog::{Model, RefDef, TypeDef};
use i3cm_core::values::Col;

pub fn ident(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
}

pub fn type_table(t: &TypeDef) -> String {
    format!("t_{}_{}", ident(&t.ns), ident(&t.local))
}

pub fn ref_table(r: &RefDef) -> String {
    format!("r_{}", ident(&r.name.replace([':', '.'], "_")))
}

/// Quoted column name of an attribute.
pub fn col(name: &str) -> String {
    format!("\"{}\"", name.replace('"', ""))
}

pub const SYSTEM: &str = r#"
CREATE TABLE IF NOT EXISTS tenant (id uuid PRIMARY KEY, name text UNIQUE NOT NULL, root_room uuid, created_at timestamptz NOT NULL DEFAULT now());
DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'tenant' AND column_name = 'development') THEN ALTER TABLE tenant ADD COLUMN development boolean NOT NULL DEFAULT false; END IF; END $$;
CREATE SEQUENCE IF NOT EXISTS txn_seq;
CREATE TABLE IF NOT EXISTS txn (tenant_id uuid NOT NULL, id bigint NOT NULL, at timestamptz NOT NULL DEFAULT now(), account uuid, PRIMARY KEY (tenant_id, id));
CREATE TABLE IF NOT EXISTS object (tenant_id uuid NOT NULL, id uuid NOT NULL, created_by uuid, created_at timestamptz NOT NULL DEFAULT now(), created_txn bigint NOT NULL, PRIMARY KEY (tenant_id, id));
CREATE TABLE IF NOT EXISTS object_state (tenant_id uuid NOT NULL, object_id uuid NOT NULL, room_id uuid NOT NULL, deleted_at timestamptz, valid_from bigint NOT NULL, valid_to bigint, PRIMARY KEY (tenant_id, object_id, valid_from));
CREATE UNIQUE INDEX IF NOT EXISTS object_state_cur ON object_state (tenant_id, object_id) WHERE valid_to IS NULL;
CREATE INDEX IF NOT EXISTS object_state_room ON object_state (tenant_id, room_id) WHERE valid_to IS NULL AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS object_state_room_id ON object_state (tenant_id, room_id, object_id) WHERE valid_to IS NULL AND deleted_at IS NULL;
CREATE TABLE IF NOT EXISTS object_type (tenant_id uuid NOT NULL, object_id uuid NOT NULL, type_name text NOT NULL, implied boolean NOT NULL, deleted_at timestamptz, valid_from bigint NOT NULL, valid_to bigint, PRIMARY KEY (tenant_id, object_id, type_name, valid_from));
CREATE INDEX IF NOT EXISTS object_type_cur ON object_type (tenant_id, object_id) WHERE valid_to IS NULL;
CREATE TABLE IF NOT EXISTS blob (sha256 text PRIMARY KEY, data bytea NOT NULL);
CREATE TABLE IF NOT EXISTS upload (id text PRIMARY KEY, tenant_id uuid NOT NULL, account uuid NOT NULL, state text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(), expires_at timestamptz NOT NULL, sha256 text, size bigint, expect_sha256 text, consumed_txn bigint);
CREATE INDEX IF NOT EXISTS upload_owner ON upload (tenant_id, account, state);
CREATE INDEX IF NOT EXISTS upload_expiry ON upload (expires_at);
CREATE TABLE IF NOT EXISTS upload_part (upload_id text NOT NULL, n int NOT NULL, sha256 text NOT NULL, size bigint NOT NULL, PRIMARY KEY (upload_id, n));
CREATE TABLE IF NOT EXISTS idempotency (tenant_id uuid NOT NULL, account uuid NOT NULL, key text NOT NULL, scope text NOT NULL, body_sha text NOT NULL, state text NOT NULL, status int, response jsonb, created_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (tenant_id, account, key));
CREATE INDEX IF NOT EXISTS idempotency_age ON idempotency (created_at);
CREATE TABLE IF NOT EXISTS content (tenant_id uuid NOT NULL, object_id uuid NOT NULL, sha256 text NOT NULL, media_type text NOT NULL, size bigint NOT NULL, filename text, valid_from bigint NOT NULL, valid_to bigint, PRIMARY KEY (tenant_id, object_id, valid_from));
CREATE TABLE IF NOT EXISTS text_part (tenant_id uuid NOT NULL, object_id uuid NOT NULL, text_type text NOT NULL, content text NOT NULL, media_type text NOT NULL, valid_from bigint NOT NULL, valid_to bigint, PRIMARY KEY (tenant_id, object_id, text_type, valid_from));
CREATE TABLE IF NOT EXISTS profile (tenant_id uuid NOT NULL, name text NOT NULL, guardian boolean NOT NULL, PRIMARY KEY (tenant_id, name));
CREATE TABLE IF NOT EXISTS profile_right (tenant_id uuid NOT NULL, profile text NOT NULL, idx int NOT NULL, sign text NOT NULL, arts text[] NOT NULL, types text[] NOT NULL, room_id uuid NOT NULL, PRIMARY KEY (tenant_id, profile, idx));
CREATE TABLE IF NOT EXISTS profile_include (tenant_id uuid NOT NULL, profile text NOT NULL, includes text NOT NULL, PRIMARY KEY (tenant_id, profile, includes));
CREATE TABLE IF NOT EXISTS assignment (tenant_id uuid NOT NULL, id uuid NOT NULL, profile text NOT NULL, carrier uuid NOT NULL, valid_from bigint NOT NULL, valid_to bigint, PRIMARY KEY (tenant_id, id, valid_from));
CREATE INDEX IF NOT EXISTS assignment_cur ON assignment (tenant_id, carrier) WHERE valid_to IS NULL;
CREATE TABLE IF NOT EXISTS token (hash text PRIMARY KEY, tenant_id uuid NOT NULL, account uuid NOT NULL, created_at timestamptz NOT NULL DEFAULT now());
DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'token' AND column_name = 'id') THEN ALTER TABLE token ADD COLUMN id uuid NOT NULL DEFAULT gen_random_uuid(); END IF; END $$;
DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'token' AND column_name = 'label') THEN ALTER TABLE token ADD COLUMN label text; END IF; END $$;
DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'token' AND column_name = 'expires_at') THEN ALTER TABLE token ADD COLUMN expires_at timestamptz; END IF; END $$;
DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'token' AND column_name = 'revoked_at') THEN ALTER TABLE token ADD COLUMN revoked_at timestamptz; END IF; END $$;
DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'token' AND column_name = 'last_used') THEN ALTER TABLE token ADD COLUMN last_used timestamptz; END IF; END $$;
CREATE UNIQUE INDEX IF NOT EXISTS token_id ON token (id);
CREATE INDEX IF NOT EXISTS token_account ON token (tenant_id, account);
DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'profile_right' AND column_name = 'cond') THEN ALTER TABLE profile_right ADD COLUMN cond text; END IF; END $$;
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'txn' AND column_name = 'module') THEN
    ALTER TABLE txn ADD COLUMN module text, ADD COLUMN function text, ADD COLUMN tool text;
  END IF;
END $$;
CREATE TABLE IF NOT EXISTS txn_audit (tenant_id uuid NOT NULL, txn bigint NOT NULL, seq int NOT NULL, depth int NOT NULL, module text, function text, identity uuid NOT NULL, action text NOT NULL, target text, PRIMARY KEY (tenant_id, txn, seq));
CREATE TABLE IF NOT EXISTS module_install (name text PRIMARY KEY, version text NOT NULL, sha256 text NOT NULL, installed_at timestamptz NOT NULL DEFAULT now(), catalog jsonb NOT NULL);
CREATE TABLE IF NOT EXISTS module_active (tenant_id uuid NOT NULL, name text NOT NULL, activated_at timestamptz NOT NULL DEFAULT now(), txn bigint, PRIMARY KEY (tenant_id, name));
CREATE TABLE IF NOT EXISTS module_carrier (tenant_id uuid NOT NULL, module text NOT NULL, name text NOT NULL, account uuid NOT NULL, PRIMARY KEY (tenant_id, name));
CREATE TABLE IF NOT EXISTS schedule_run (tenant_id uuid NOT NULL, module text NOT NULL, op text NOT NULL, due timestamptz NOT NULL, done_at timestamptz, worker text, result text, PRIMARY KEY (tenant_id, module, op, due));
CREATE INDEX IF NOT EXISTS schedule_run_open ON schedule_run (due) WHERE done_at IS NULL;
CREATE TABLE IF NOT EXISTS module_room (tenant_id uuid NOT NULL, name text NOT NULL, room_id uuid NOT NULL, PRIMARY KEY (tenant_id, name));
"#;

/// Trigram index for the search prefilter of `grep` (needs the extension pg_trgm; without it
/// the prefilter query scans the texts).
pub const TRIGRAM: &str = "CREATE EXTENSION IF NOT EXISTS pg_trgm; \
CREATE INDEX IF NOT EXISTS text_part_trgm ON text_part USING gin (content gin_trgm_ops) WHERE valid_to IS NULL;";

/// DDL for all catalog types and references.
/// Add a column only if it is missing: `ALTER TABLE` takes an exclusive lock even with
/// `IF NOT EXISTS`, which would queue behind open orders when another container starts.
fn add_column(tb: &str, name: &str, sql: &str) -> String {
    format!(
        "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = '{}' AND column_name = '{}') THEN ALTER TABLE {tb} ADD COLUMN {} {sql}; END IF; END $$",
        tb.trim_matches('"').replace('\'', ""),
        name.replace('\'', ""),
        col(name)
    )
}

pub fn catalog_ddl(m: &Model) -> Vec<String> {
    let mut out = Vec::new();
    for t in &m.catalog.types {
        let tb = type_table(t);
        out.push(format!(
            "CREATE TABLE IF NOT EXISTS {tb} (tenant_id uuid NOT NULL, object_id uuid NOT NULL, valid_from bigint NOT NULL, valid_to bigint, deleted_at timestamptz, PRIMARY KEY (tenant_id, object_id, valid_from))"
        ));
        out.push(format!("CREATE UNIQUE INDEX IF NOT EXISTS {tb}_cur ON {tb} (tenant_id, object_id) WHERE valid_to IS NULL"));
        // default order of lists (and keyset paging over it)
        out.push(format!("CREATE INDEX IF NOT EXISTS {tb}_order ON {tb} (tenant_id, valid_from, object_id) WHERE valid_to IS NULL AND deleted_at IS NULL"));
        for a in &t.attributes {
            let sql = Col::of(&a.vt).sql(a.list);
            out.push(add_column(&tb, &a.name, sql));
            if a.unique && !a.list {
                out.push(format!(
                    "CREATE UNIQUE INDEX IF NOT EXISTS {tb}_{}_u ON {tb} (tenant_id, {}) WHERE valid_to IS NULL AND deleted_at IS NULL",
                    ident(&a.name), col(&a.name)
                ));
            } else if a.indexed && !a.list && Col::of(&a.vt) != Col::Json {
                out.push(format!(
                    "CREATE INDEX IF NOT EXISTS {tb}_{}_i ON {tb} (tenant_id, {}, object_id) WHERE valid_to IS NULL AND deleted_at IS NULL",
                    ident(&a.name), col(&a.name)
                ));
            }
        }
    }
    for r in &m.catalog.references {
        let tb = ref_table(r);
        out.push(format!(
            "CREATE TABLE IF NOT EXISTS {tb} (tenant_id uuid NOT NULL, id uuid NOT NULL, a_id uuid NOT NULL, b_id uuid NOT NULL, rank int, valid_from bigint NOT NULL, valid_to bigint, PRIMARY KEY (tenant_id, id, valid_from))"
        ));
        out.push(add_column(&tb, "seq", "bigserial"));
        out.push(format!("CREATE INDEX IF NOT EXISTS {tb}_a ON {tb} (tenant_id, a_id) WHERE valid_to IS NULL"));
        out.push(format!("CREATE INDEX IF NOT EXISTS {tb}_b ON {tb} (tenant_id, b_id) WHERE valid_to IS NULL"));
    }
    out
}

/// Split a script into statements (lines ending with `;`, `DO $$ … $$;` blocks kept whole).
pub fn statements(script: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut dollars = 0;
    for line in script.lines() {
        if line.trim().is_empty() && cur.is_empty() {
            continue;
        }
        cur.push_str(line);
        cur.push('\n');
        dollars += line.matches("$$").count();
        if dollars % 2 == 0 && line.trim_end().ends_with(';') {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}
