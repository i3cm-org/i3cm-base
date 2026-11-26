#!/usr/bin/env python3
"""Fill a demo tenant through the public API: mails over several days (one day with many mails
to show paging), attachments, contents and texts, plus persons, companies and documents.

    python3 deploy/ui/seed-demo.py http://localhost:8090 <token>
"""
import json
import sys
import urllib.request

BASE, TOKEN = sys.argv[1].rstrip("/"), sys.argv[2]


def call(method, path, body=None, ctype="application/json", headers=None):
    data = json.dumps(body).encode() if ctype == "application/json" and body is not None else body
    h = {"Authorization": f"Bearer {TOKEN}", "Content-Type": ctype, **(headers or {})}
    req = urllib.request.Request(BASE + path, data=data, method=method, headers=h)
    with urllib.request.urlopen(req) as r:
        raw = r.read()
        return json.loads(raw) if raw else None


def order(ops):
    return call("POST", "/v1/orders", {"ops": ops})["created"]


PDF = (b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n"
       b"3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 144]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj\n"
       b"4 0 obj<</Length 44>>stream\nBT /F1 18 Tf 40 70 Td (Demo attachment) Tj ET\nendstream endobj\n"
       b"5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n")

senders = ["anna@example.org", "ben@example.org", "carla@example.com", "dora@example.net", "emil@example.org"]
subjects = ["Offer", "Invoice", "Meeting notes", "Delivery date", "Question about the contract", "Reminder"]
days = [("2026-11-20", 4), ("2026-11-21", 6), ("2026-11-23", 3), ("2026-11-24", 130), ("2026-11-25", 8)]

n = 0
for day, count in days:
    for start in range(0, count, 25):
        ops, meta = [], []
        for i in range(start, min(count, start + 25)):
            n += 1
            s, subj = senders[n % len(senders)], f"{subjects[n % len(subjects)]} {n}"
            ref = f"m{i}"
            rcpt = [x for x in senders if x != s][: 1 + n % 3]
            ops.append({"op": "create", "ref": ref, "types": {"base:Mail": {
                "receivedAt": f"{day}T{8 + i % 10:02d}:{i % 60:02d}:00+01:00", "sender": s, "subject": subj, "recipients": rcpt}}})
            atts = n % 3
            for j in range(atts):
                ops.append({"op": "create", "ref": f"{ref}a{j}", "types": {"base:MailAttachment": {"filename": f"attachment-{n}-{j}.pdf"}}})
                ops.append({"op": "link", "reference": "base:MailAttachment.mail", "from": f"{ref}a{j}", "to": ref})
            meta.append((ref, s, subj, rcpt, atts))
        created = order(ops)
        for ref, s, subj, rcpt, atts in meta:
            mid = created[ref]
            eml = f"From: {s}\r\nTo: {', '.join(rcpt)}\r\nSubject: {subj}\r\n\r\nHello,\r\n\r\nthis is demo mail {subj}.\r\n".encode()
            call("PUT", f"/v1/objects/{mid}/content", eml, "message/rfc822", {"x-filename": "mail.eml"})
            call("PUT", f"/v1/objects/{mid}/texts/base:mail.md", f"# {subj}\n\nFrom **{s}**\n\nHello,\n\nthis is demo mail *{subj}*.\n".encode(), "text/plain")
            if n % 2 == 0:
                call("PUT", f"/v1/objects/{mid}/texts/base:body.html", f"<h1>{subj}</h1><p>Hello from {s}.</p>".encode(), "text/plain")
            for j in range(atts):
                call("PUT", f"/v1/objects/{created[f'{ref}a{j}']}/content", PDF, "application/pdf", {"x-filename": f"attachment-{j}.pdf"})
    print(f"{day}: {count} mails")

ops = [{"op": "create", "ref": f"p{i}", "types": {"base:Person": {"lastName": ln, "firstName": fn}}}
       for i, (fn, ln) in enumerate([("Anna", "Adler"), ("Ben", "Berg"), ("Carla", "Conrad"), ("Dora", "Dietz")])]
ops += [{"op": "create", "ref": f"c{i}", "types": {"base:Company": {"name": nm}}} for i, nm in enumerate(["Example Ltd", "Sample GmbH"])]
ops += [{"op": "create", "ref": f"d{i}", "types": {"base:Document": {"title": t}}} for i, t in enumerate(["Statutes", "Minutes 2026"])]
docs = order(ops)
for i in range(2):
    call("PUT", f"/v1/objects/{docs[f'd{i}']}/texts/base:plain", f"Demo document {i}.\n".encode(), "text/plain")
print("persons, companies, documents: done")

# one mail with many attachments: a grid with several pages
ops = [{"op": "create", "ref": "scan", "types": {"base:Mail": {"receivedAt": "2026-11-25T18:00:00+01:00", "sender": "scanner@example.org", "subject": "Scan batch", "recipients": ["anna@example.org"]}}}]
for j in range(60):
    ops.append({"op": "create", "ref": f"s{j}", "types": {"base:MailAttachment": {"filename": f"scan-{j:03d}.pdf"}}})
    ops.append({"op": "link", "reference": "base:MailAttachment.mail", "from": f"s{j}", "to": "scan"})
order(ops)
print("scan batch with 60 attachments: done")
