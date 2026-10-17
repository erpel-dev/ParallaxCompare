#!/usr/bin/env python3
"""Writes the e-mail fixtures with Python's email package.

order.eml                     multipart/mixed: text/plain + HTML alternative (quoted-printable),
                              a CSV and a PDF attachment, RFC 2047 encoded headers
order-base64.eml              the same message with base64 bodies, other boundaries and Received headers
order-attachment-changed.eml  one byte of the PDF changed
newsletter-html.eml           HTML only
reply.eml                     a reply with In-Reply-To and References

order.msg is written by the ignored test `regenerate_msg_fixture` in src/convert/email.rs
(cargo test -p parallax-core regenerate_msg -- --ignored); this script then reads it back with
extract_msg (pip install extract-msg), an independent Outlook .msg reader, as a check.
"""
import email.charset
import email.policy
from email.message import EmailMessage
from email.utils import format_datetime
import datetime

CSV = b"sku,qty\nA-1,12\nB-7,3\nC-9,100\nD-2,4\n"
PDF = b"%PDF-1.4\n" + bytes(range(256)) * 8
BODY = "Hallo Team,\n\nbitte die Menge auf 12 Stück ändern.\n\nGrüße\nJürgen\n"
DATE = datetime.datetime(2026, 10, 1, 9, 30, tzinfo=datetime.timezone(datetime.timedelta(hours=2)))


def order(cte, pdf=PDF, received=False):
    m = EmailMessage(policy=email.policy.SMTP)
    if received:
        m["Received"] = "from mx1.example.org (mx1.example.org [192.0.2.1]) by mail.example.org; Thu, 1 Oct 2026 09:30:05 +0200"
        m["Received"] = "from client (unknown [198.51.100.7]) by mx1.example.org; Thu, 1 Oct 2026 09:30:02 +0200"
    m["From"] = "Jürgen Müller <juergen@example.com>"
    m["To"] = "Shop Team <team@example.org>, sales@example.org"
    m["Cc"] = "audit@example.org"
    m["Subject"] = "Bestellung Nr. 4711 – Änderung"
    m["Date"] = format_datetime(DATE)
    m["Message-ID"] = "<order-4711@example.com>"
    m.set_content(BODY, cte=cte)
    m.add_alternative("<p>Hallo Team,</p><p>bitte die Menge auf <b>12</b> Stück ändern.</p>", subtype="html", cte=cte)
    m.add_attachment(CSV, maintype="text", subtype="csv", filename="order-4711.csv")
    m.add_attachment(pdf, maintype="application", subtype="pdf", filename="invoice.pdf")
    return m


def write(name, m):
    open(name, "wb").write(m.as_bytes())


write("order.eml", order("quoted-printable"))
write("order-base64.eml", order("base64", received=True))
pdf = bytearray(PDF)
pdf[100] ^= 1
write("order-attachment-changed.eml", order("quoted-printable", pdf=bytes(pdf)))

n = EmailMessage(policy=email.policy.SMTP)
n["From"] = "Shop <news@example.org>"
n["To"] = "customer@example.com"
n["Subject"] = "Autumn sale"
n["Date"] = format_datetime(DATE)
n.set_content("<html><body><h1>Autumn sale</h1><table><tr><td>Bolts</td><td>-20%</td></tr></table></body></html>", subtype="html")
write("newsletter-html.eml", n)

r = EmailMessage(policy=email.policy.SMTP)
r["From"] = "Shop Team <team@example.org>"
r["To"] = "Jürgen Müller <juergen@example.com>"
r["Subject"] = "Re: Bestellung Nr. 4711 – Änderung"
r["Date"] = format_datetime(DATE + datetime.timedelta(hours=1))
r["Message-ID"] = "<reply-4711@example.org>"
r["In-Reply-To"] = "<order-4711@example.com>"
r["References"] = "<order-4711@example.com>"
r.set_content("Erledigt.\n\n> bitte die Menge auf 12 Stück ändern.\n")
write("reply.eml", r)

try:
    import extract_msg
    msg = extract_msg.openMsg("order.msg")
    print("order.msg:", msg.subject, "|", msg.sender, "|", msg.to, "|", msg.cc, "|", [a.longFilename for a in msg.attachments])
except ImportError:
    print("extract_msg not installed, order.msg not checked")
