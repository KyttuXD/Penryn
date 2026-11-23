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
  * download servers' resource packs and bake them, with the player's own packs from
    DATA/resourcepacks, into the game files they change (GET /pack/list, POST /pack/download,
    POST /pack/bake, GET /pack/files/...);
  * fetch the account's chat key pair (the services' player certificates), which Penryn signs
    its chat with on servers that enforce secure chat;
  * serve the game files Penryn draws with (blocks.bin, atlas.bin, gui.bin, entities.bin, signs.bin, weather.bin, effects.bin, particles.bin, menu.bin, serverui.bin, containers.bin and sounds.bin), made from your own copy
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


# ---------------------------------------------------------- updates
#
# Penryn updates itself from the GitHub "nightly" release (CI replaces it on every push to main). The
# program cannot speak HTTPS, so the relay reads the release and unpacks the build for it: the client
# asks GET /update/check?platform=macos|linux (the release file's SHA-256) and, when that differs from
# what it installed, GET /update/download?platform=...&sha=... (the files, as one "PNU1" archive).
# The SHA-256 of the release file is the one GitHub itself reports for it, and is checked here.

UPDATE_REPO = "KyttuXD/Penryn"
UPDATE_FILES = {"macos": "penryn-macos-x86_64.zip", "linux": "penryn-linux-x86_64.tar.gz"}
UPDATE_LIMIT = 64 << 20
_update_cache = {}  # platform -> (time, (sha256, size)) | sha256 -> archive bytes
_update_lock = threading.Lock()


def update_release_host_ok(url):
    parts = urllib.parse.urlsplit(url)
    host = parts.hostname or ""
    return parts.scheme == "https" and (host == "github.com" or host.endswith(".github.com")
                                        or host.endswith(".githubusercontent.com"))


class _GithubOnly(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not update_release_host_ok(newurl):
            raise urllib.error.URLError("the download moved to %s" % newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def update_release(platform):
    """(sha256, size, download url) of the nightly release's file for `platform`."""
    name = UPDATE_FILES.get(platform)
    if not name:
        raise ValueError("unknown platform")
    req = urllib.request.Request("https://api.github.com/repos/%s/releases/tags/nightly" % UPDATE_REPO,
                                 headers={"User-Agent": USER_AGENT, "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(req, timeout=15) as r:
        release = json.loads(r.read(4 << 20))
    for a in release.get("assets", []):
        if a.get("name") == name:
            digest = str(a.get("digest") or "")
            if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
                raise ValueError("the release file has no checksum")
            return digest[7:], int(a.get("size") or 0), a["browser_download_url"]
    raise ValueError("the nightly release has no %s" % name)


def update_check(platform):
    """The nightly file's "SHA-256 size" (looked up at most once a minute)."""
    with _update_lock:
        hit = _update_cache.get(platform)
        if hit and time.time() - hit[0] < 60:
            return hit[1]
    sha, size, _ = update_release(platform)
    with _update_lock:
        _update_cache[platform] = (time.time(), (sha, size))
    return sha, size


def _update_path_ok(path):
    return bool(path) and not path.startswith("/") and ".." not in path.split("/") and "\0" not in path \
        and "\\" not in path


def update_entries(platform, data):
    """[(kind, path, mode, bytes)] of the release file: kind 0 file, 1 folder, 2 link (bytes: target).
    The top folder (Penryn.app/, penryn-linux/) is dropped; the Mac build keeps the whole app, the
    Linux build the program and tools/."""
    import io
    import stat
    import tarfile
    import zipfile
    out = []
    if platform == "macos":
        with zipfile.ZipFile(io.BytesIO(data)) as z:
            for i in z.infolist():
                path = i.filename.rstrip("/")
                if not path or path.startswith("__MACOSX/") or "/._" in "/" + path:
                    continue
                if not _update_path_ok(path):
                    raise ValueError("unsafe path in the release file: " + path)
                path = path.partition("/")[2]
                if not path:
                    continue
                mode = (i.external_attr >> 16) & 0o7777
                kind = stat.S_IFMT(i.external_attr >> 16)
                if i.is_dir():
                    out.append((1, path, mode or 0o755, b""))
                elif kind == stat.S_IFLNK:
                    out.append((2, path, mode, z.read(i)))
                else:
                    out.append((0, path, mode or 0o644, z.read(i)))
    else:
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as t:
            for m in t.getmembers():
                path = m.name.rstrip("/")
                top, _, rest = path.partition("/")
                if not rest:
                    continue
                if not _update_path_ok(path):
                    raise ValueError("unsafe path in the release file: " + path)
                if m.isdir():
                    out.append((1, rest, 0o755, b""))
                elif m.issym():
                    out.append((2, rest, 0o777, m.linkname.encode()))
                elif m.isfile():
                    out.append((0, rest, m.mode & 0o7777, t.extractfile(m).read()))
    return out


def update_archive(platform, want):
    """The PNU1 archive of the nightly file with SHA-256 `want` (it is verified against the download):
    "PNU1", u32 count, then for each entry: u8 kind, u16 mode, u16 path length, path, u32 length, data."""
    import struct
    with _update_lock:
        hit = _update_cache.get(("archive", platform, want))
    if hit:
        return hit
    sha, size, url = update_release(platform)
    if sha != want:
        raise ValueError("the release changed: ask again")
    if size > UPDATE_LIMIT:
        raise ValueError("the release file is too large")
    opener = urllib.request.build_opener(_GithubOnly)
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    data = bytearray()
    # Right after CI replaced the file, GitHub's file host may still serve the old copy for a moment: tried again.
    for attempt in range(4):
        h = hashlib.sha256()
        data = bytearray()
        with opener.open(req, timeout=120) as r:
            while True:
                chunk = r.read(1 << 16)
                if not chunk:
                    break
                data += chunk
                if len(data) > UPDATE_LIMIT:
                    raise ValueError("the release file is too large")
                h.update(chunk)
        if h.hexdigest() == want:
            break
        time.sleep(5)
    else:
        raise ValueError("checksum mismatch: the release file was not used")
    entries = update_entries(platform, bytes(data))
    out = bytearray(b"PNU1" + struct.pack("<I", len(entries)))
    for kind, path, mode, body in entries:
        pb = path.encode()
        out += struct.pack("<BHH", kind, mode, len(pb)) + pb + struct.pack("<I", len(body)) + body
    out = bytes(out)
    with _update_lock:
        for k in [k for k in _update_cache if isinstance(k, tuple)]:
            del _update_cache[k]  # one archive is kept
        _update_cache[("archive", platform, want)] = out
    return out


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

    def certificates(self, token):
        """UserApiService.getKeyPair: the profile key pair for secure chat, as lines: the
        private key and the public key (DER, base64, without their PEM armour), the services'
        signature (publicKeySignatureV2), expiresAt and refreshedAfter (epoch milliseconds)."""
        req = urllib.request.Request(
            "https://api.minecraftservices.com/player/certificates", data=b"", method="POST",
            headers={"User-Agent": USER_AGENT, "Accept": "application/json",
                     "Content-Type": "application/json; charset=utf-8", "Authorization": "Bearer " + token})
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                body = json.loads(r.read())
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode(errors="replace")[:300]

        def der(pem):
            return "".join(l.strip() for l in pem.splitlines() if l.strip() and not l.startswith("-----"))

        def millis(text):
            # Instant.parse: 2022-07-30T23:05:24.548227Z
            from datetime import datetime, timezone
            t = text.rstrip("Z")
            whole, _, frac = t.partition(".")
            d = datetime.strptime(whole, "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc)
            return int(d.timestamp()) * 1000 + int((frac + "000")[:3])

        k = body["keyPair"]
        lines = [der(k["privateKey"]), der(k["publicKey"]), body["publicKeySignatureV2"],
                 str(millis(body["expiresAt"])), str(millis(body["refreshedAfter"]))]
        return 200, "\n".join(lines) + "\n"

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
        names = ("blocks.bin", "atlas.bin", "gui.bin", "entities.bin", "weather.bin", "signs.bin", "particles.bin", "menu.bin", "serverui.bin",
                 "containers.bin", "items.bin")
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


# ---------------------------------------------------------- resource packs
#
# Penryn cannot fetch a server's resource pack itself (HTTPS) nor read zips and JSON quickly on
# a 2008 machine, so the relay downloads the pack (as vanilla's DownloadQueue: the player's name
# and version headers, at most 250 MB, SHA-1 checked, kept by its hash) and bakes the pack stack
# into Penryn's game files with make_assets.py --packs. Only the files the stack changes are
# served; Penryn keeps the rest.

PACK_LIMIT = 262144000          # vanilla's limit for a server pack
PACK_FILES = ("blocks.bin", "atlas.bin", "gui.bin", "entities.bin", "weather.bin", "signs.bin", "particles.bin",
              "effects.bin", "packsounds.bin", "items.bin", "containers.bin", "menu.bin", "serverui.bin")
PACK_JOBS = {}                  # key -> {"state": "working" | "done" | "failed", "result": text}
PACK_LOCK = threading.Lock()


def pack_job(key, work, keep=True):
    """Runs work() once in a thread for `key`: (status, text) for the client to poll."""
    with PACK_LOCK:
        j = PACK_JOBS.get(key)
        if j is None:
            j = PACK_JOBS[key] = {"state": "working", "result": "", "progress": ""}

            def run():
                try:
                    r = work(j)
                    j["result"], j["state"] = r, "done"
                except Exception as e:  # noqa: BLE001 - reported to the client
                    j["result"], j["state"] = str(e) or e.__class__.__name__, "failed"
            threading.Thread(target=run, daemon=True).start()
    if j["state"] == "working":
        return 202, "working %s\n" % j.get("progress", "")
    if j["state"] == "failed":
        with PACK_LOCK:
            PACK_JOBS.pop(key, None)
        return 502, j["result"] + "\n"
    if not keep:
        with PACK_LOCK:
            PACK_JOBS.pop(key, None)
    return 200, j["result"]


def download_pack(data_dir, url, want, name, uuid, job):
    """The pack at `url` (http or https) in DATA/packs/<sha1>.zip: its SHA-1, checked against
    `want` when the server gave one."""
    want = (want or "").strip().lower()
    folder = os.path.join(data_dir, "packs")
    os.makedirs(folder, exist_ok=True)
    if re.fullmatch(r"[0-9a-f]{40}", want) and os.path.isfile(os.path.join(folder, want + ".zip")):
        return want
    if urllib.parse.urlsplit(url).scheme not in ("http", "https"):
        raise ValueError("invalid url")
    headers = {"User-Agent": "Minecraft Java/" + VERSION, "X-Minecraft-Username": name, "X-Minecraft-UUID": uuid,
               "X-Minecraft-Version": VERSION, "X-Minecraft-Version-ID": VERSION, "X-Minecraft-Pack-Format": "97"}
    req = urllib.request.Request(url, headers={k: v for k, v in headers.items() if v})
    part = os.path.join(folder, "download.%s.part" % secrets.token_hex(4))
    h = hashlib.sha1()
    got = 0
    try:
        with urllib.request.urlopen(req, timeout=60) as r, open(part, "wb") as f:
            size = int(r.headers.get("Content-Length") or 0)
            if size > PACK_LIMIT:
                raise ValueError("the pack is larger than %d bytes" % PACK_LIMIT)
            while True:
                chunk = r.read(1 << 16)
                if not chunk:
                    break
                got += len(chunk)
                if got > PACK_LIMIT:
                    raise ValueError("the pack is larger than %d bytes" % PACK_LIMIT)
                h.update(chunk)
                f.write(chunk)
                job["progress"] = "%d %d" % (got, size)
        sha = h.hexdigest()
        if re.fullmatch(r"[0-9a-f]{40}", want) and sha != want:
            raise ValueError("the pack's SHA-1 is %s, the server said %s" % (sha, want))
        os.replace(part, os.path.join(folder, sha + ".zip"))
        return sha
    finally:
        if os.path.exists(part):
            os.remove(part)


def local_packs(data_dir):
    """DATA/resourcepacks: zips and folders with a pack.mcmeta, as vanilla's FolderRepositorySource.
    (id, compat word (make_assets.Pack.compat_word), description) sorted by id."""
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    sys.dont_write_bytecode = True
    import make_assets  # noqa: E402 - the same reader the bake uses
    folder = os.path.join(data_dir, "resourcepacks")
    out = []
    if os.path.isdir(folder):
        for n in sorted(os.listdir(folder)):
            p = os.path.join(folder, n)
            if not (os.path.isdir(p) or n.endswith(".zip")):
                continue
            try:
                pk = make_assets.Pack(p)
            except Exception:  # noqa: BLE001 - a broken zip is left out, as vanilla does
                continue
            if not pk.meta:
                continue
            d = pk.description
            if isinstance(d, dict):
                d = d.get("text", "") or d.get("translate", "")
            elif isinstance(d, list):
                d = "".join(x if isinstance(x, str) else x.get("text", "") for x in d)
            out.append(("file/" + n, pk.compat_word(), str(d).replace("\n", " ").replace("\t", " ")))
    return out


def pack_source(data_dir, ref):
    """A pack reference from Penryn: sha1:<hex> (a server pack downloaded here) or file/<name>."""
    if ref.startswith("sha1:") and re.fullmatch(r"[0-9a-f]{40}", ref[5:]):
        p = os.path.join(data_dir, "packs", ref[5:] + ".zip")
    elif ref.startswith("file/") and "/" not in ref[5:] and ref[5:] not in ("", ".", ".."):
        p = os.path.join(data_dir, "resourcepacks", ref[5:])
    else:
        raise ValueError("unknown pack %r" % ref)
    if not os.path.exists(p):
        raise ValueError("no pack %s" % ref)
    return p


def bake_packs(relay, refs, job):
    """Bakes the stack `refs` (bottom to top): its key and the files that differ from the game's."""
    base = relay.build_assets()
    paths = [pack_source(relay.data_dir, r) for r in refs]
    sig = hashlib.sha1()
    tool = os.path.join(os.path.dirname(os.path.abspath(__file__)), "make_assets.py")
    sig.update(str(os.path.getmtime(tool)).encode())
    for r, p in zip(refs, paths):
        sig.update(r.encode() + b"\0")
        if r.startswith("file/"):
            # A folder or zip that may change: its files' sizes and times are its identity.
            for root, _, files in (os.walk(p) if os.path.isdir(p) else [(os.path.dirname(p), [], [os.path.basename(p)])]):
                for fn in sorted(files):
                    st = os.stat(os.path.join(root, fn))
                    sig.update(("%s %d %d\n" % (os.path.join(root, fn), st.st_size, st.st_mtime_ns)).encode())
    key = sig.hexdigest()[:20]
    out = os.path.join(relay.data_dir, "packs", "stack-" + key)
    if not os.path.isfile(os.path.join(out, "changed.txt")):
        prepared = relay.find_prepared()
        jar = relay.find_jar()
        if not prepared:
            raise RuntimeError("no prepared 26.3 data")
        tmp = out + ".part"
        os.makedirs(tmp, exist_ok=True)
        args = [sys.executable, "-I", tool, prepared] + (["--jar", jar] if jar else []) + ["--packs", ",".join(paths), tmp]
        subprocess.run(args, check=True, stdout=subprocess.DEVNULL)
        changed = []
        for n in PACK_FILES:
            p = os.path.join(tmp, n)
            if not os.path.isfile(p):
                continue
            b = os.path.join(base, n)
            with open(p, "rb") as f1:
                mine = f1.read()
            same = False
            if os.path.isfile(b) and os.path.getsize(b) == len(mine):
                with open(b, "rb") as f2:
                    same = f2.read() == mine
            if same:
                os.remove(p)
            else:
                changed.append("%s %d" % (n, len(mine)))
        with open(os.path.join(tmp, "changed.txt"), "w") as f:
            f.write("\n".join(changed) + "\n")
        if os.path.isdir(out):
            import shutil
            shutil.rmtree(out)
        os.replace(tmp, out)
    with open(os.path.join(out, "changed.txt")) as f:
        return key + "\n" + f.read()


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
            m = re.fullmatch(r"/update/(check|download)\?platform=(macos|linux)(?:&sha=([0-9a-f]{64}))?", self.path)
            if m:
                try:
                    if m.group(1) == "check":
                        sha, size = update_check(m.group(2))
                        return self.reply(200, "%s %d\n" % (sha, size))
                    if not m.group(3):
                        return self.reply(400, "sha is missing")
                    return self.reply(200, update_archive(m.group(2), m.group(3)), "application/octet-stream")
                except Exception as e:  # noqa: BLE001 - reported to the client
                    return self.reply(502, "update: %s" % e)
            if self.path == "/assets/effects.bin":
                # Optional (the status effect icons): 404 when the files were made without it.
                try:
                    path = os.path.join(relay.build_assets(), "effects.bin")
                    if not os.path.isfile(path):
                        return self.reply(404, "no effects.bin")
                    with open(path, "rb") as f:
                        return self.reply(200, f.read(), "application/octet-stream")
                except Exception as e:  # noqa: BLE001 - reported to the client
                    return self.reply(500, str(e))
            if self.path in ("/assets/blocks.bin", "/assets/atlas.bin", "/assets/gui.bin", "/assets/entities.bin",
                             "/assets/weather.bin", "/assets/signs.bin", "/assets/particles.bin", "/assets/menu.bin", "/assets/serverui.bin", "/assets/containers.bin", "/assets/items.bin"):
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
            if self.path == "/pack/list":
                try:
                    rows = local_packs(relay.data_dir)
                    return self.reply(200, "".join("%s\t%s\t%s\n" % (i, c, d) for i, c, d in rows))
                except Exception as e:  # noqa: BLE001 - reported to the client
                    return self.reply(500, str(e))
            m = re.fullmatch(r"/pack/files/([0-9a-f]{20})/([a-z]+\.bin)", self.path)
            if m and m.group(2) in PACK_FILES:
                p = os.path.join(relay.data_dir, "packs", "stack-" + m.group(1), m.group(2))
                if not os.path.isfile(p):
                    return self.reply(404, "no such file")
                with open(p, "rb") as f:
                    return self.reply(200, f.read(), "application/octet-stream")
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
                if self.path == "/pack/download":
                    # url, hash, player name, player uuid: 202 while it downloads, then the SHA-1.
                    url, want = a[0].strip(), (a[1].strip() if len(a) > 1 else "")
                    who = (a[2].strip() if len(a) > 2 else "", a[3].strip() if len(a) > 3 else "")
                    st, text = pack_job("dl " + url + " " + want,
                                        lambda job: download_pack(relay.data_dir, url, want, who[0], who[1], job))
                    return self.reply(st, text)
                if self.path == "/pack/bake":
                    refs = [x.strip() for x in a if x.strip()]
                    st, text = pack_job("bake " + " ".join(refs), lambda job: bake_packs(relay, refs, job), keep=False)
                    return self.reply(st, text)
                if self.path == "/certificates":
                    st, text = relay.certificates(a[0].strip())
                    return self.reply(200 if st == 200 else 502, text)
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
