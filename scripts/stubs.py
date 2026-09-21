#!/usr/bin/env python3
"""Stand-ins for Degen Builders, Discord and Jev, for running Degen Guard locally.

    python3 scripts/stubs.py 3121

Degen Builders: /api/v1/sso/authorize answers a silent ask with
`login_required` until you press the sign-in once, then signs you in as
"localdev" (dev@example.com, the operator) for the rest of the run.
Discord: /oauth2/authorize sends the browser straight back as "localdev", who
manages the server "Local Builders" (g-local); the bot's REST calls are
answered and printed. Jev: POST /jev answers by keywords ("free nitro" = scam,
"buy my course" = spam, "check out my server" = borderline)."""

import json, sys, urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 3121
GUILD = {"id": "g-local", "name": "Local Builders", "icon": None, "owner": True, "permissions": "8"}
IDENTITY = {"subject": "localdev", "email": "dev@example.com", "handle": "localdev", "avatar_url": None}
SIGNED_IN = False


def verdict(text):
    t = text.lower()
    if "free nitro" in t:
        return "scam_or_phishing", 0.03, 0.95, 0.0, 0.97
    if "buy my course" in t:
        return "spam", 0.95, 0.01, 0.02, 0.6
    if "check out my server" in t:
        return "self_promo_off_topic", 0.2, 0.0, 0.5, 0.4
    return "legit", 0.02, 0.01, 0.02, 0.05


class Handler(BaseHTTPRequestHandler):
    def reply(self, status, body=None, headers=()):
        data = b"" if body is None else json.dumps(body).encode()
        self.send_response(status)
        for k, v in headers:
            self.send_header(k, v)
        if body is not None:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def body(self):
        length = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(length) if length else b""

    def do_GET(self):
        url = urllib.parse.urlparse(self.path)
        q = dict(urllib.parse.parse_qsl(url.query))
        if url.path == "/api/v1/sso/authorize":
            global SIGNED_IN
            back = q["redirect_uri"] + ("&" if "?" in q["redirect_uri"] else "?")
            if not SIGNED_IN and q.get("prompt") == "none":
                return self.reply(302, headers=[("Location", back + urllib.parse.urlencode({"error": "login_required", "state": q.get("state", "")}))])
            SIGNED_IN = True  # the sign-in screen over there, pressed
            return self.reply(302, headers=[("Location", back + urllib.parse.urlencode({"code": "sso-x", "state": q.get("state", "")}))])
        if url.path == "/oauth2/authorize":
            install = "bot" in q.get("scope", "")
            back = {"code": ("install-" if install else "code-") + "x", "state": q["state"]}
            if install:
                back["guild_id"] = GUILD["id"]
            return self.reply(302, headers=[("Location", q["redirect_uri"] + "?" + urllib.parse.urlencode(back))])
        path = url.path.removeprefix("/api/v10")
        if path == "/users/@me":
            return self.reply(200, {"id": "4242", "username": "localdev", "global_name": "Local Dev", "email": "dev@example.com", "verified": True, "avatar": None})
        if path == "/users/@me/guilds":
            return self.reply(200, [GUILD])
        if path.endswith("/channels") and path.startswith("/guilds/"):
            return self.reply(200, [{"id": "c-general", "name": "general", "type": 0, "position": 1}, {"id": "c-promo", "name": "self-promo", "type": 0, "position": 2}, {"id": "c-log", "name": "mod-log", "type": 0, "position": 3}])
        if path.endswith("/roles"):
            return self.reply(200, [{"id": "r-mods", "name": "Mods", "position": 3}, {"id": "r-members", "name": "Members", "position": 2}])
        if path.startswith("/channels/"):
            return self.reply(200, {"name": "general"})
        return self.reply(404, {"message": "Unknown"})

    def do_POST(self):
        raw = self.body()
        path = urllib.parse.urlparse(self.path).path
        if path == "/api/v1/sso/token":
            return self.reply(200, {"identity": IDENTITY})
        if path == "/jev":
            text = json.loads(raw)["state"]["text"]
            kind, spam, scam, promo, lure = verdict(text)
            return self.reply(200, {"model": "local-stub", "usage": {"input_tokens": 100, "output_tokens": 5}, "answers": {
                "kind": {"type": "choice", "choice": kind, "confidence": 0.9, "probabilities": {"legit": round(1 - spam - scam - promo, 3), "spam": spam, "scam_or_phishing": scam, "self_promo_off_topic": promo}},
                "lure": {"type": "noul", "noul": lure}}})
        path = path.removeprefix("/api/v10")
        if path == "/oauth2/token":
            form = dict(urllib.parse.parse_qsl(raw.decode()))
            guild = {"id": GUILD["id"], "name": GUILD["name"]} if form.get("code", "").startswith("install") else None
            return self.reply(200, {"access_token": "local", "guild": guild})
        print("discord POST", path, raw.decode()[:200], flush=True)
        return self.reply(200, {"id": "posted"})

    def other(self):
        print("discord", self.command, self.path, self.body().decode()[:200], flush=True)
        self.reply(204)

    do_PUT = do_PATCH = do_DELETE = other

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    print(f"Discord + Jev stand-ins on http://127.0.0.1:{PORT}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
