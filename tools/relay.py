#!/usr/bin/env python3
"""penryn-relay: the HTTPS half of Penryn, on a computer newer than the Mac.

Mac OS X 10.6 cannot make the TLS 1.2 connections Microsoft, Xbox Live and Mojang require, so
Penryn asks this relay, over plain HTTP on the local network, to do them:

  * sign in with your Microsoft account the way Horizon does: when Horizon's launcher is
    installed here and signed in, the relay asks it for a session (`horizon-launcher account
    session`, the read-once file it hands the games it starts);
  * or, without it, sign in with a Microsoft account (the device-code flow: Penryn shows a code, you enter it
    at https://www.microsoft.com/link on any device), check the account owns Minecraft: Java
    Edition, and hand Penryn the Minecraft token and profile;
  * refresh that sign-in later without asking again;
  * make the session-server "join" call an online-mode server checks;
  * serve the game files Penryn draws with (blocks.bin, atlas.bin, gui.bin, entities.bin, signs.bin, and sounds.bin), made from your own copy
    of Minecraft 26.3: an existing install (Horizon's launcher, the official launcher, Prism),
    or the 26.3 client downloaded from Mojang for your signed-in account (as Horizon does).

    PENRYN_MSA_CLIENT_ID=<your Azure app's client id> tools/relay.py [--port 25580] [--data DIR]

The client id is your own Azure app registration ("Personal Microsoft accounts only", public
client flows allowed), approved by Mojang for the Minecraft API: see README.md. Never use
another application's client id.

Tokens cross your local network unencrypted between the Mac and the relay: run it only on a
network you trust. The relay keeps nothing on disk but the game files and the skins and capes
it fetched for Penryn (GET /skin?url=..., only from textures.minecraft.net, cached in DATA/skins).
"""
import hashlib
import http.server
import ipaddress
import json
import os
import re
import secrets
import socketserver
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

MS = "https://login.microsoftonline.com/consumers/oauth2/v2.0"
SCOPE = "XboxLive.signin offline_access"
VERSION = "26.3"
USER_AGENT = "penryn-relay/0.1"

HOME = os.path.expanduser("~")
JAR_PLACES = [
    os.path.join(HOME, ".local/share/horizon-launcher/minecraft/versions", VERSION, VERSION + ".jar"),
    os.path.join(HOME, ".minecraft/versions", VERSION, VERSION + ".jar"),
    os.path.join(HOME, "Library/Application Support/minecraft/versions", VERSION, VERSION + ".jar"),
    os.path.join(HOME, "AppData/Roaming/.minecraft/versions", VERSION, VERSION + ".jar"),
]
PREPARED_PLACES = [
    os.path.join(HOME, ".local/share/horizon-launcher/versions"),
]


def call(url, data=None, form=None, token=None, method=None):
    headers = {"User-Agent": USER_AGENT, "Accept": "application/json"}
    body = None
    if form is not None:
        body = urllib.parse.urlencode(form).encode()
        headers["Content-Type"] = "application/x-www-form-urlencoded"
    elif data is not None:
        body = json.dumps(data).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read()
            return r.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw)
        except ValueError:
            return e.code, {"error": raw.decode(errors="replace")[:300]}


class Relay:
    def __init__(self, client_id, data_dir):
        self.client_id = client_id
        self.data_dir = data_dir
        self.pending = {}  # session -> device code reply
        self.lock = threading.Lock()

    # ---------------------------------------------------------- Microsoft -> Minecraft

    def start(self):
        if not self.client_id:
            raise RuntimeError("the relay has no Microsoft client id: set PENRYN_MSA_CLIENT_ID (README.md)")
        st, r = call(MS + "/devicecode", form={"client_id": self.client_id, "scope": SCOPE})
        if st != 200:
            raise RuntimeError("Microsoft refused the sign-in: %s" % r.get("error_description", r))
        sid = secrets.token_hex(16)
        r["at"] = time.time()
        with self.lock:
            self.pending[sid] = r
        return sid, r["user_code"], r["verification_uri"], int(r.get("interval", 5))

    def poll(self, sid):
        with self.lock:
            p = self.pending.get(sid)
        if not p:
            raise RuntimeError("unknown or finished sign-in")
        st, r = call(MS + "/token", form={
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "client_id": self.client_id, "device_code": p["device_code"]})
        if st != 200:
            err = r.get("error")
            if err in ("authorization_pending", "slow_down"):
                return None
            with self.lock:
                self.pending.pop(sid, None)
            raise RuntimeError({"authorization_declined": "the sign-in was declined",
                                "expired_token": "the code expired: start again",
                                "bad_verification_code": "the code was not accepted"}.get(err, str(err)))
        with self.lock:
            self.pending.pop(sid, None)
        return self.minecraft(r["access_token"], r.get("refresh_token", ""))

    def refresh(self, refresh_token):
        st, r = call(MS + "/token", form={"grant_type": "refresh_token", "client_id": self.client_id,
                                          "refresh_token": refresh_token, "scope": SCOPE})
        if st != 200:
            raise RuntimeError("the saved sign-in expired: sign in again")
        return self.minecraft(r["access_token"], r.get("refresh_token", refresh_token))

    def minecraft(self, ms_token, refresh_token):
        st, xbl = call("https://user.auth.xboxlive.com/user/authenticate", data={
            "Properties": {"AuthMethod": "RPS", "SiteName": "user.auth.xboxlive.com", "RpsTicket": "d=" + ms_token},
            "RelyingParty": "http://auth.xboxlive.com", "TokenType": "JWT"})
        if st != 200:
            raise RuntimeError("Xbox Live refused the sign-in")
        uhs = xbl["DisplayClaims"]["xui"][0]["uhs"]
        st, xsts = call("https://xsts.auth.xboxlive.com/xsts/authorize", data={
            "Properties": {"SandboxId": "RETAIL", "UserTokens": [xbl["Token"]]},
            "RelyingParty": "rp://api.minecraftservices.com/", "TokenType": "JWT"})
        if st != 200:
            code = xsts.get("XErr")
            raise RuntimeError({2148916233: "this Microsoft account has no Xbox profile yet",
                                2148916238: "child account: an adult must add it to a family"}.get(code, "XSTS refused the sign-in (%s)" % code))
        st, mc = call("https://api.minecraftservices.com/authentication/login_with_xbox",
                      data={"identityToken": "XBL3.0 x=%s;%s" % (uhs, xsts["Token"])})
        if st != 200:
            if st == 403:
                raise RuntimeError("this app is not approved by Mojang yet (README.md: Minecraft API access)")
            raise RuntimeError("Minecraft refused the sign-in (%d)" % st)
        token = mc["access_token"]
        st, ent = call("https://api.minecraftservices.com/entitlements/mcstore", token=token)
        owned = st == 200 and any(i.get("name") in ("game_minecraft", "product_minecraft") for i in ent.get("items", []))
        st, prof = call("https://api.minecraftservices.com/minecraft/profile", token=token)
        if not owned or st != 200:
            raise RuntimeError("this account does not own Minecraft: Java Edition")
        return {"name": prof["name"], "uuid": prof["id"], "token": token, "refresh": refresh_token}

    # ---------------------------------------------------------- through Horizon's launcher

    def launcher_accounts(self):
        """The accounts Horizon's launcher has signed in (names, ids, which plays), or []."""
        try:
            out = subprocess.run(["horizon-launcher", "account", "list", "--json"], capture_output=True,
                                 timeout=60, check=False).stdout
            return json.loads(out).get("accounts", [])
        except (OSError, ValueError, subprocess.TimeoutExpired):
            return []

    def launcher_session(self, who):
        """A fresh session for `who` (name or id; empty: the account that plays) from Horizon's
        launcher: the read-once file it hands the games it starts. Horizon signs in with its own
        app registration; the relay never sees a Microsoft token, only the Minecraft session."""
        accounts = self.launcher_accounts()
        if not accounts:
            raise RuntimeError("Horizon's launcher has no signed-in account here (horizon-launcher account sign-in)")
        if not who:
            sel = [a for a in accounts if a.get("selected")] or accounts
            who = sel[0]["uuid"]
        r = subprocess.run(["horizon-launcher", "account", "session", who, "--json"], capture_output=True,
                           timeout=120, check=False)
        try:
            reply = json.loads(r.stdout)
        except ValueError:
            raise RuntimeError("Horizon's launcher did not answer: %s" % r.stderr.decode(errors="replace")[:200])
        path = reply.get("session_file")
        if not path:
            auth = reply.get("authentication") or {}
            raise RuntimeError(auth.get("message") or "Horizon's launcher could not renew the session")
        try:
            with open(path) as f:
                s = json.load(f)
        finally:
            try:
                os.remove(path)  # read once, as the game does
            except OSError:
                pass
        return {"name": s["name"], "uuid": s["uuid"].replace("-", ""), "token": s["access_token"],
                "refresh": "launcher:" + s["uuid"].replace("-", "")}

    def join(self, token, uuid, server_hash):
        st, r = call("https://sessionserver.mojang.com/session/minecraft/join",
                     data={"accessToken": token, "selectedProfile": uuid, "serverId": server_hash})
        return st, r

    # ---------------------------------------------------------- the game's files

    def find_jar(self):
        for p in JAR_PLACES:
            if os.path.isfile(p):
                return p
        p = os.path.join(self.data_dir, "client-%s.jar" % VERSION)
        return p if os.path.isfile(p) else None

    def download_jar(self, token):
        """The 26.3 client from Mojang, for a signed-in account that owns the game."""
        st, ent = call("https://api.minecraftservices.com/entitlements/mcstore", token=token)
        if st != 200 or not ent.get("items"):
            raise RuntimeError("only an account that owns the game can download it")
        st, manifest = call("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")
        v = next((v for v in manifest.get("versions", []) if v["id"] == VERSION), None)
        if not v:
            raise RuntimeError("Mojang lists no %s" % VERSION)
        st, meta = call(v["url"])
        client = meta["downloads"]["client"]
        os.makedirs(self.data_dir, exist_ok=True)
        dest = os.path.join(self.data_dir, "client-%s.jar" % VERSION)
        req = urllib.request.Request(client["url"], headers={"User-Agent": USER_AGENT})
        h = hashlib.sha1()
        with urllib.request.urlopen(req, timeout=120) as r, open(dest + ".part", "wb") as f:
            while True:
                chunk = r.read(1 << 16)
                if not chunk:
                    break
                h.update(chunk)
                f.write(chunk)
        if h.hexdigest() != client["sha1"]:
            os.remove(dest + ".part")
            raise RuntimeError("the download did not match Mojang's checksum")
        os.replace(dest + ".part", dest)
        return dest

    def find_prepared(self):
        for base in PREPARED_PLACES:
            if not os.path.isdir(base):
                continue
            for v in sorted(os.listdir(base), reverse=True):
                p = os.path.join(base, v, ".local", "horizon-client", VERSION)
                if os.path.isfile(os.path.join(p, "gameplay-facts.json")):
                    return p
        return None

    def build_assets(self, token=None):
        out = os.path.join(self.data_dir, "assets")
        names = ("blocks.bin", "atlas.bin", "gui.bin", "entities.bin", "weather.bin", "signs.bin")
        tool = os.path.join(os.path.dirname(os.path.abspath(__file__)), "make_assets.py")
        # Made again when the tool is newer (another format) than the files.
        if all(os.path.isfile(os.path.join(out, n)) and os.path.getmtime(os.path.join(out, n)) >= os.path.getmtime(tool)
               for n in names):
            return out
        prepared = self.find_prepared()
        if not prepared:
            raise RuntimeError("no prepared 26.3 data: install a Horizon release with its launcher (it runs horizon-prepare)")
        jar = self.find_jar()
        if not jar and token:
            jar = self.download_jar(token)
        tool = os.path.join(os.path.dirname(os.path.abspath(__file__)), "make_assets.py")
        args = [sys.executable, "-I", tool, prepared] + (["--jar", jar] if jar else []) + [out]
        subprocess.run(args, check=True)
        return out


TEXTURE_URL = re.compile(r"^https?://textures\.minecraft\.net/texture/([0-9a-f]{16,128})$")


def skin_texture(data_dir, url):
    """A player's skin or cape (the textures property of their profile) as PNG bytes: fetched
    once over HTTPS from textures.minecraft.net, which Mac OS X 10.6 cannot reach, and kept in
    DATA/skins by its hash. Any other host is refused."""
    m = TEXTURE_URL.match(url)
    if not m:
        raise ValueError("only textures.minecraft.net textures")
    h = m.group(1)
    cache = os.path.join(data_dir, "skins")
    path = os.path.join(cache, h + ".png")
    if os.path.isfile(path):
        with open(path, "rb") as f:
            return f.read()
    req = urllib.request.Request("https://textures.minecraft.net/texture/" + h, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=20) as r:
        body = r.read((1 << 21) + 1)
    if len(body) > 1 << 21:
        raise ValueError("texture too big")
    if not body.startswith(b"\x89PNG\r\n\x1a\n"):
        raise ValueError("not a PNG")
    os.makedirs(cache, exist_ok=True)
    part = "%s.%s.part" % (path, secrets.token_hex(4))  # two requests at once write apart
    with open(part, "wb") as f:
        f.write(body)
    os.replace(part, path)
    return body


def private(addr):
    try:
        ip = ipaddress.ip_address(addr)
        return ip.is_private or ip.is_loopback or ip.is_link_local
    except ValueError:
        return False


def make_handler(relay):
    class H(http.server.BaseHTTPRequestHandler):
        server_version = USER_AGENT

        def reply(self, status, body=b"", kind="text/plain; charset=utf-8"):
            if isinstance(body, str):
                body = body.encode()
            self.send_response(status)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, fmt, *args):
            # Never log bodies (they hold tokens): the path and status only.
            sys.stderr.write("%s %s\n" % (self.address_string(), fmt % args))

        def lines(self):
            n = int(self.headers.get("Content-Length", "0") or 0)
            return self.rfile.read(min(n, 1 << 16)).decode(errors="replace").split("\n")

        def do_GET(self):
            if not private(self.client_address[0]):
                return self.reply(403, "local network only")
            if self.path in ("/assets/blocks.bin", "/assets/atlas.bin", "/assets/gui.bin", "/assets/entities.bin",
                             "/assets/weather.bin", "/assets/signs.bin"):
                try:
                    d = relay.build_assets()
                    with open(os.path.join(d, self.path.rsplit("/", 1)[1]), "rb") as f:
                        return self.reply(200, f.read(), "application/octet-stream")
                except Exception as e:  # noqa: BLE001 - reported to the client
                    return self.reply(500, str(e))
            if self.path == "/assets/sounds.bin":
                # Optional and large (hundreds of MB): streamed, never read into memory whole.
                try:
                    path = os.path.join(relay.build_assets(), "sounds.bin")
                    if not os.path.isfile(path):
                        return self.reply(404, "no sounds: the game's sound assets were not found when the files were made")
                    size = os.path.getsize(path)
                    self.send_response(200)
                    self.send_header("Content-Type", "application/octet-stream")
                    self.send_header("Content-Length", str(size))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    with open(path, "rb") as f:
                        while True:
                            chunk = f.read(1 << 20)
                            if not chunk:
                                break
                            self.wfile.write(chunk)
                    return
                except (BrokenPipeError, ConnectionResetError):
                    return
                except Exception as e:  # noqa: BLE001 - reported to the client
                    return self.reply(500, str(e))
            if self.path.startswith("/skin?"):
                q = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
                try:
                    return self.reply(200, skin_texture(relay.data_dir, q.get("url", [""])[0]), "image/png")
                except ValueError as e:
                    return self.reply(400, str(e))
                except urllib.error.HTTPError as e:
                    return self.reply(404 if e.code == 404 else 502, "textures.minecraft.net: %d" % e.code)
                except Exception as e:  # noqa: BLE001 - reported to the client
                    return self.reply(502, str(e))
            if self.path == "/":
                return self.reply(200, "penryn-relay for Minecraft %s\n" % VERSION)
            self.reply(404, "no such thing")

        def do_POST(self):
            if not private(self.client_address[0]):
                return self.reply(403, "local network only")
            try:
                a = self.lines()
                if self.path == "/login/start":
                    sid, code, url, interval = relay.start()
                    return self.reply(200, "%s\n%s\n%s\n%d\n" % (sid, code, url, interval))
                if self.path == "/login/poll":
                    acc = relay.poll(a[0].strip())
                    if acc is None:
                        return self.reply(202, "pending\n")
                    return self.reply(200, "%(name)s\n%(uuid)s\n%(token)s\n%(refresh)s\n" % acc)
                if self.path == "/login/launcher":
                    acc = relay.launcher_session(a[0].strip() if a else "")
                    return self.reply(200, "%(name)s\n%(uuid)s\n%(token)s\n%(refresh)s\n" % acc)
                if self.path == "/login/accounts":
                    lines = ["%s %s%s" % (x["uuid"], x["name"], " *" if x.get("selected") else "")
                             for x in relay.launcher_accounts()]
                    return self.reply(200, "\n".join(lines) + "\n")
                if self.path == "/login/refresh":
                    token = a[0].strip()
                    if token.startswith("launcher:"):
                        acc = relay.launcher_session(token[len("launcher:"):])
                        return self.reply(200, "%(name)s\n%(uuid)s\n%(token)s\n%(refresh)s\n" % acc)
                    acc = relay.refresh(token)
                    return self.reply(200, "%(name)s\n%(uuid)s\n%(token)s\n%(refresh)s\n" % acc)
                if self.path == "/join":
                    st, r = relay.join(a[0].strip(), a[1].strip(), a[2].strip())
                    return self.reply(204 if st == 204 else 502, "" if st == 204 else json.dumps(r))
                if self.path == "/assets/download":
                    relay.build_assets(token=a[0].strip())
                    return self.reply(200, "ready\n")
                self.reply(404, "no such thing")
            except Exception as e:  # noqa: BLE001 - reported to the client
                self.reply(400, str(e))

    return H


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    port, data = 25580, os.path.join(HOME, ".local/share/penryn-relay")
    args = sys.argv[1:]
    if "--port" in args:
        port = int(args[args.index("--port") + 1])
    if "--data" in args:
        data = args[args.index("--data") + 1]
    relay = Relay(os.environ.get("PENRYN_MSA_CLIENT_ID", "").strip(), data)
    if relay.launcher_accounts():
        print("Microsoft sign-in: through Horizon's launcher (%s)" % ", ".join(a["name"] for a in relay.launcher_accounts()))
    elif not relay.client_id:
        print("note: no Horizon launcher account and no PENRYN_MSA_CLIENT_ID: only offline play")
    srv = Server(("0.0.0.0", port), make_handler(relay))
    print("penryn-relay on port %d (local network only); game files in %s" % (port, data))
    srv.serve_forever()


if __name__ == "__main__":
    main()
