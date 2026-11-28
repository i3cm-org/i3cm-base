# Profiles, rooms, rights paths

## Rooms

Every object lives in exactly one room. Rooms form a tree under the root room; modules bring their own
rooms below the root, created on activation (`GET /v1/rooms` lists them by name). Rights given on a room
apply to its sub rooms. Objects change rooms with the order operation `move`.

## Profiles

A profile is a collection of rights; the word "role" is kept for business roles in a context (a team
leader of one team). Profiles come with the modules (`profiles` in `module.ts`) and are listed by
`GET /v1/profiles`. A right names a sign (`+`, `-` or `block`), kinds (`read`, `write` (includes read),
`create`, `place`, `remove`, `delete`, `grant`), types (or `*`) and a room.

```sh
curl -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
     -d '{"profile": "dms:clerk", "carrier": "<group or account id>"}' $SERVER/v1/assignments
```

assigns a profile to a group or an account. Assigning needs the right to grant rights.

## Conditions

A right can carry a condition `when`: it applies only to objects for which the condition holds. The
condition uses the same filter language as views (states, dates, paths to the caller, named filters
of the module), so a step of a workflow can be a state that both a view and a right ask for. Paths to
the caller turn business roles into rights without a separate mechanism:

| Case | Right | Condition |
|---|---|---|
| team leader | `write` on `hr:LeaveRequest` | `applicant.team.leader = %me` |
| project lead | `write` on `pm:Milestone` | `project.lead = %me` |
| assignee | `read`, `write` on `tasks:Task` | `assignees = %me` |
| approval step | `write` on `hr:LeaveRequest` | `status = 'submitted' and applicant.team.leader = %me` |

`%me` is the calling account and every group it belongs to. A path has at most three steps and may run
over any reference; conditions give `read`, `write` and `delete` on existing objects, never `create`,
`place` or `remove`. A right on a type covers that type's part of every object carrying it, also of
objects of a subtype: a right to read documents reads mails as documents (they show in document
lists, with their document attributes, texts and content), but not what only mails have (sender,
recipients, headers); that needs a right on mails. Moving an object needs `remove` and `place` on
each of its types (or on `*`), and so does linking a part to a whole in another room, since the part
moves with it (a part created in the same order needs `create` or `place` where the whole is).
Deleting an object needs `delete` on `*`.

Writing data never grants rights: setting a reference a path runs over (making someone a team
leader, assigning a task) needs only write on one object and read on the other, like any reference.
Rights come from profiles alone. The module's profiles decide who may write which object (who may
change a team's leader is whoever may write the team), the tenant administrator decides who gets a
profile, and the data (who leads which team) decides where it applies. A module author should not
give write on objects that the module's own conditions depend on to the people those conditions
favour; otherwise they could put themselves into a condition by editing data.

Objects a caller may not read do not exist for them: the API answers `not found`, never `forbidden`.
