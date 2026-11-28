# Tenant administration

A tenant administrator manages one tenant: which modules are active, who may do what, and which tokens
agents get. All of it goes through the same API as everything else; `init-tenant` gives the first login
(`admin`, member of the group `admins` with the guardian profile) and a start token valid for one day.

| Page | Content |
|---|---|
| this page | modules, groups, tokens for agents |
| [profiles-and-rights.md](profiles-and-rights.md) | profiles, rooms, rights paths |

## Modules

```sh
curl -H "Authorization: Bearer $TOKEN" $SERVER/v1/modules
curl -X POST -H "Authorization: Bearer $TOKEN" $SERVER/v1/modules/mail/activate
```

`GET /v1/modules` lists the modules the operator installed, with `mandatory`, `developmentOnly`, whether
they are active in the tenant, and their operations. Activating needs the right to grant rights at the
root room. It checks that the required modules are active, creates the module's rooms, profiles and
carriers (agent accounts of the module), applies the module's imprinting rules to existing objects, all
or nothing, and reports the rights it brings: `red` where they reach the root room or types of other
modules, `green` otherwise. Activated modules stay active; there is no deactivation. Types and views of
modules that are not active do not exist for the tenant.

The mandatory modules `tasks` (tasks and deadlines) and `dms` (documents) are active from the start.

## Groups

Groups are objects of type `base:Group`; membership is an object `base:GroupMembership` between a group
and a carrier (an account or another group). Writing a membership needs the right to grant rights,
because it changes rights.

```json
{"ops": [
  {"op": "create", "ref": "g", "room": "<root room id>", "types": {"base:Group": {"name": "accounting"}}},
  {"op": "create", "ref": "m", "room": "<root room id>", "types": {"base:GroupMembership": {}}},
  {"op": "link", "reference": "base:GroupMembership.group", "from": "m", "to": "g"},
  {"op": "link", "reference": "base:GroupMembership.member", "from": "m", "to": "<account id>"}
]}
```

`GET /v1/me/groups` and `GET /v1/groups/{id}/members?transitive=true` show the result.

## Profile assignments

`POST /v1/assignments` gives a profile to a group or an account; `GET /v1/assignments?carrier=<id>`
lists the current assignments, `DELETE /v1/assignments/{id}` ends one. All three need the right to grant
rights.

## Deactivating accounts and the last administrator

An account is deactivated by setting `deactivated: true` on it (an ordinary update); it keeps its data
and memberships, but its tokens are refused (`auth.account_inactive`) until `deactivated` is set back to
`false`.

```json
{"ops": [{"op": "update", "id": "<account id>", "types": {"base:PersonAccount": {"deactivated": true}}}]}
```

A tenant always keeps at least one active account that can grant rights at the root room. An order that
would remove the last one (deleting or deactivating the account, deleting its group or membership,
ending its profile assignment) is refused with `tenant.last_admin`; make another account an
administrator first, for example by adding it to the group `admins`.

## Tokens for agents

An agent gets an account of its own (`base:AgentAccount`) with small profiles cut to its task, and a
token for that account:

```sh
curl -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
     -d '{"account": "<agent account id>", "label": "invoice agent", "ttl": 30}' $SERVER/v1/tokens
```

The answer shows the token once. `ttl` is in days (default 90, at most 365); a token unused for 90 days
lapses. `GET /v1/tokens` lists tokens without their secrets (`?account=` filters), `DELETE /v1/tokens/{id}`
revokes one at once. Anyone may issue a token for themselves without `account`; it never outlives the token
used to issue it.

The agent stores it with `i3cm-init --server … --tenant … < token.txt` and finds everything else in the
server's self-description (`GET /guide/agents`).
