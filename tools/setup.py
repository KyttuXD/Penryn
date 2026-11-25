#!/usr/bin/env python3
"""penryn setup: make Penryn's game files from your own copy of Minecraft 26.3.

    penryn setup [--list] [--minecraft DIR] [--download] [--yes] [--data DIR] [--force] [--dry-run]

Penryn's game files (blocks.bin, atlas.bin, ...) are made on your machine from Minecraft's own files: the
generator (tools/make_assets.py) needs the 26.3 client AND the tables Horizon's preparer exports from it (block
behaviours, collision, entity models...). This tool finds a copy of the game, has the Horizon launcher
(`horizon-launcher`, https://github.com/horizon-rust/horizon) prepare it, then runs the generator into Penryn's data
folder. Where the copy comes from:

  1. Prepared data from an earlier Horizon install is used as it is.
  2. An install of another launcher: the official launcher, Prism Launcher, MultiMC / PolyMC, the Modrinth App,
     ATLauncher, GDLauncher, CurseForge, TLauncher and anything else that keeps `<version>.jar` files: found in their
     usual folders (--list shows what was found), or the folder named with --minecraft.
  3. No install: --download signs in with your Microsoft account (the launcher's device code: you open a web page and
     type a code) and downloads Minecraft 26.3 from Mojang itself, for an account that owns Minecraft: Java Edition.

Nothing from Mojang is kept in this repository or sent anywhere else: the files stay on your computer.
"""
import argparse
import glob
import json
import os
import shutil
import subprocess
import sys

VERSION = "26.3"
HERE = os.path.dirname(os.path.abspath(__file__))
HOME = os.path.expanduser("~")
APPDATA = os.environ.get("APPDATA") or os.path.join(HOME, "AppData", "Roaming")
LOCALAPPDATA = os.environ.get("LOCALAPPDATA") or os.path.join(HOME, "AppData", "Local")
XDG_DATA = os.environ.get("XDG_DATA_HOME") or os.path.join(HOME, ".local", "share")
MAC_SUPPORT = os.path.join(HOME, "Library", "Application Support")


def data_roots():
    """The folders where launchers keep their data on this system: (launcher, folder)."""
    flatpak = os.path.join(HOME, ".var", "app")
    out = []

    def add(name, *paths):
        for p in paths:
            out.append((name, p))

    if sys.platform.startswith("win"):
        add("Minecraft (official)", os.path.join(APPDATA, ".minecraft"))
        add("Prism Launcher", os.path.join(APPDATA, "PrismLauncher"), os.path.join(LOCALAPPDATA, "Programs", "PrismLauncher"))
        add("MultiMC / PolyMC", os.path.join(APPDATA, "MultiMC"), os.path.join(APPDATA, "PolyMC"), os.path.join(HOME, "MultiMC"))
        add("Modrinth App", os.path.join(APPDATA, "ModrinthApp"), os.path.join(APPDATA, "com.modrinth.theseus"))
        add("ATLauncher", os.path.join(APPDATA, "ATLauncher"), os.path.join(HOME, "ATLauncher"))
        add("GDLauncher", os.path.join(APPDATA, "gdlauncher_carbon"), os.path.join(APPDATA, "gdlauncher_next"))
        add("CurseForge", os.path.join(HOME, "curseforge", "minecraft", "Install"))
        add("TLauncher", os.path.join(APPDATA, ".tlauncher"))
        add("Lunar Client", os.path.join(HOME, ".lunarclient"))
    elif sys.platform == "darwin":
        add("Minecraft (official)", os.path.join(MAC_SUPPORT, "minecraft"))
        add("Prism Launcher", os.path.join(MAC_SUPPORT, "PrismLauncher"))
        add("MultiMC / PolyMC", os.path.join(MAC_SUPPORT, "MultiMC"), os.path.join(MAC_SUPPORT, "PolyMC"))
        add("Modrinth App", os.path.join(MAC_SUPPORT, "ModrinthApp"), os.path.join(MAC_SUPPORT, "com.modrinth.theseus"))
        add("ATLauncher", os.path.join(MAC_SUPPORT, "ATLauncher"))
        add("GDLauncher", os.path.join(MAC_SUPPORT, "gdlauncher_carbon"))
        add("CurseForge", os.path.join(HOME, "Documents", "curseforge", "minecraft", "Install"), os.path.join(HOME, "curseforge", "minecraft", "Install"))
        add("TLauncher", os.path.join(MAC_SUPPORT, ".tlauncher"))
        add("Lunar Client", os.path.join(HOME, ".lunarclient"))
    else:
        add("Minecraft (official)", os.path.join(HOME, ".minecraft"), os.path.join(flatpak, "com.mojang.Minecraft", ".minecraft"))
        add("Prism Launcher", os.path.join(XDG_DATA, "PrismLauncher"), os.path.join(flatpak, "org.prismlauncher.PrismLauncher", "data", "PrismLauncher"))
        add("MultiMC / PolyMC", os.path.join(XDG_DATA, "multimc"), os.path.join(XDG_DATA, "PolyMC"), os.path.join(HOME, ".local", "share", "polymc"), os.path.join(HOME, "MultiMC"))
        add("Modrinth App", os.path.join(XDG_DATA, "ModrinthApp"), os.path.join(XDG_DATA, "com.modrinth.theseus"), os.path.join(flatpak, "com.modrinth.ModrinthApp", "data", "ModrinthApp"))
        add("ATLauncher", os.path.join(XDG_DATA, "ATLauncher"), os.path.join(HOME, ".config", "ATLauncher"), os.path.join(HOME, "ATLauncher"), os.path.join(flatpak, "com.atlauncher.ATLauncher", "data", "ATLauncher"))
        add("GDLauncher", os.path.join(XDG_DATA, "gdlauncher_carbon"), os.path.join(HOME, ".config", "gdlauncher_carbon"))
        add("CurseForge", os.path.join(HOME, "curseforge", "minecraft", "Install"))
        add("TLauncher", os.path.join(HOME, ".tlauncher"))
        add("Lunar Client", os.path.join(HOME, ".lunarclient"))
    return out


# Folders that are big and never hold the game's own jar.
SKIP_DIRS = {"mods", "saves", "screenshots", "logs", "crash-reports", "resourcepacks", "shaderpacks", "datapacks",
             "node_modules", "cache", "backups", "worlds", "profile", "profiles", "java", "runtime", "objects"}


def find_jars(root, version=VERSION, depth=7):
    """Client jars of `version` under a launcher's folder: <version>.jar (the official layout) and
    minecraft-<version>-client.jar (Maven layout: Prism, MultiMC), up to `depth` folders down."""
    names = {version + ".jar", "minecraft-%s-client.jar" % version}
    found = []
    root = os.path.abspath(root)
    base_depth = root.rstrip(os.sep).count(os.sep)
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        if dirpath.count(os.sep) - base_depth >= depth:
            dirnames[:] = []
            continue
        dirnames[:] = [d for d in dirnames if d.lower() not in SKIP_DIRS]
        for f in filenames:
            if f in names:
                p = os.path.join(dirpath, f)
                try:
                    if os.path.getsize(p) > 1_000_000:  # a real client, not a stub
                        found.append(p)
                except OSError:
                    pass
    return sorted(found)


def scan(extra=None, roots=None):
    """Installs of the version: [{launcher, folder, jar}]. `extra` is a folder the user named."""
    installs = []
    seen = set()
    cands = list(roots if roots is not None else data_roots())
    if extra:
        cands.insert(0, ("the folder you named", os.path.abspath(os.path.expanduser(extra))))
    for name, folder in cands:
        if not os.path.isdir(folder):
            continue
        for jar in find_jars(folder):
            key = os.path.realpath(jar)
            if key in seen:
                continue
            seen.add(key)
            installs.append({"launcher": name, "folder": folder, "jar": jar})
    return installs


# ------------------------------------------------------------------ Penryn's data folder and Horizon's prepared data

def penryn_data_dir():
    d = os.environ.get("PENRYN_DATA")
    if d:
        return d
    if sys.platform.startswith("win"):
        return os.path.join(APPDATA, "Penryn")
    if sys.platform == "darwin":
        return os.path.join(MAC_SUPPORT, "Penryn")
    return os.path.join(XDG_DATA, "penryn")


def horizon_home():
    """Where the Horizon launcher keeps its versions (HORIZON_LAUNCHER_HOME, else the platform's data folder)."""
    h = os.environ.get("HORIZON_LAUNCHER_HOME")
    if h:
        return h
    if sys.platform.startswith("win"):
        return os.path.join(APPDATA, "horizon-launcher")
    if sys.platform == "darwin":
        return os.path.join(MAC_SUPPORT, "horizon-launcher")
    return os.path.join(XDG_DATA, "horizon-launcher")


def prepared_dirs():
    """Prepared data of Horizon installs: <home>/versions/<v>/.local/horizon-client/26.3 with gameplay-facts.json."""
    out = []
    for v in sorted(glob.glob(os.path.join(horizon_home(), "versions", "*")), reverse=True):
        p = os.path.join(v, ".local", "horizon-client", VERSION)
        if os.path.isfile(os.path.join(p, "gameplay-facts.json")):
            out.append(p)
    return out


def find_launcher():
    """The Horizon launcher program: HORIZON_LAUNCHER, PATH, then the usual places."""
    env = os.environ.get("HORIZON_LAUNCHER")
    if env and os.path.isfile(env):
        return env
    exe = shutil.which("horizon-launcher")
    if exe:
        return exe
    names = ["horizon-launcher.exe"] if sys.platform.startswith("win") else ["horizon-launcher"]
    places = [os.path.join(HOME, ".local", "bin"), "/usr/local/bin", "/opt/horizon", os.path.join(LOCALAPPDATA, "Programs", "Horizon"),
              "/Applications/Horizon.app/Contents/MacOS", os.path.join(HOME, "Applications", "Horizon.app", "Contents", "MacOS")]
    for d in places:
        for n in names:
            p = os.path.join(d, n)
            if os.path.isfile(p):
                return p
    return None


REQUIRED = ["blocks.bin", "atlas.bin", "gui.bin", "entities.bin"]


def have_game_files(data):
    return all(os.path.isfile(os.path.join(data, n)) for n in REQUIRED)


# ------------------------------------------------------------------ steps

def run(cmd, dry, check=False):
    print("  $ " + " ".join(('"%s"' % c if " " in c else c) for c in cmd))
    if dry:
        return 0
    r = subprocess.run(cmd)
    if check and r.returncode:
        raise SystemExit(r.returncode)
    return r.returncode


def launcher_has_version(launcher):
    """Whether a Horizon version is installed (prepare needs one to put its data in)."""
    try:
        r = subprocess.run([launcher, "versions", "--installed"], capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return None
    if r.returncode != 0:
        # an older launcher without --installed: fall back to looking at the folder
        return bool(glob.glob(os.path.join(horizon_home(), "versions", "*")))
    return any(line.strip() for line in r.stdout.splitlines())


def signed_in(launcher):
    try:
        r = subprocess.run([launcher, "account", "--json"], capture_output=True, text=True, timeout=60)
        data = json.loads(r.stdout or "[]")
    except (OSError, subprocess.SubprocessError, ValueError):
        return False
    if isinstance(data, dict):
        data = data.get("accounts", [])
    return bool(data)


def build_game_files(prepared, data, dry):
    """Runs the generator (the relay's --build-assets does the same) into `data`."""
    print("Making Penryn's game files in %s from %s ..." % (data, prepared))
    if dry:
        return True
    sys.path.insert(0, HERE)
    import relay  # noqa: E402 - the relay's lookup and builder
    os.makedirs(data, exist_ok=True)
    r = relay.Relay("", data)
    r.find_prepared = lambda: prepared  # exactly this prepared data
    out = r.build_assets(out=data)
    print("Game files made in", out)
    return True


def main(argv=None):
    ap = argparse.ArgumentParser(prog="penryn setup", description=__doc__.split("\n\n")[0])
    ap.add_argument("--list", action="store_true", help="show what was found and what would be done; change nothing")
    ap.add_argument("--minecraft", metavar="DIR", help="a launcher's folder to look in first (an instance, a launcher's data folder)")
    ap.add_argument("--download", action="store_true", help="download Minecraft 26.3 from Mojang for your Microsoft account if no install is found")
    ap.add_argument("--yes", action="store_true", help="do not ask questions (a missing install is a failure unless --download)")
    ap.add_argument("--data", metavar="DIR", help="Penryn's data folder (default: PENRYN_DATA, else the platform's)")
    ap.add_argument("--force", action="store_true", help="make the game files again even when they exist")
    ap.add_argument("--dry-run", action="store_true", help="print the commands without running them")
    a = ap.parse_args(argv)
    data = a.data or penryn_data_dir()
    dry = a.dry_run or a.list

    installs = scan(a.minecraft)
    prepared = prepared_dirs()
    launcher = find_launcher()
    print("Penryn's data folder: %s%s" % (data, " (game files present)" if have_game_files(data) else ""))
    print("Horizon launcher: %s" % (launcher or "not found"))
    print("Horizon prepared data: %s" % (prepared[0] if prepared else "none"))
    if installs:
        print("Minecraft %s found:" % VERSION)
        for i in installs:
            print("  %-22s %s" % (i["launcher"], i["jar"]))
    else:
        print("No Minecraft %s found in the usual launcher folders." % VERSION)
    if a.list:
        return 0
    if have_game_files(data) and not a.force:
        print("The game files are already there (use --force to make them again).")
        return 0

    if not prepared:
        if not launcher:
            print("\nThe Horizon launcher is needed once: it prepares the game's tables from your Minecraft. Install it from\n"
                  "https://github.com/horizon-rust/horizon (or set HORIZON_LAUNCHER to its program), then run this again.", file=sys.stderr)
            return 2
        if launcher_has_version(launcher) is False:
            print("Installing a Horizon version (the launcher's `install latest`) ...")
            if run([launcher, "install", "latest"], dry):
                print("Could not install a Horizon version.", file=sys.stderr)
                return 3
        ok = False
        tried = set()
        for i in installs:
            if i["folder"] in tried:
                continue
            tried.add(i["folder"])
            print("Preparing from %s (%s) ..." % (i["launcher"], i["folder"]))
            if run([launcher, "prepare", "--minecraft", i["folder"]], dry) == 0:
                ok = True
                break
        if not ok and not installs:
            # the launcher also looks in every launcher's default place by itself
            print("Preparing from whatever install the launcher can find ...")
            ok = run([launcher, "prepare"], dry) == 0
        if not ok:
            want = a.download
            if not want and not a.yes and sys.stdin.isatty():
                want = input("No usable install. Download Minecraft %s from Mojang for your Microsoft account? [y/N] " % VERSION).strip().lower().startswith("y")
            if not want:
                print("\nNothing to prepare from. Install Minecraft %s with any launcher, or run again with --download." % VERSION, file=sys.stderr)
                return 4
            if not signed_in(launcher):
                print("Signing in with your Microsoft account (a code to type at a web page) ...")
                if run([launcher, "account", "sign-in"], dry):
                    print("Sign-in failed.", file=sys.stderr)
                    return 5
            print("Downloading Minecraft %s from Mojang (an account that owns it) ..." % VERSION)
            if run([launcher, "prepare", "--download"], dry):
                print("Download or preparing failed.", file=sys.stderr)
                return 6
        prepared = prepared_dirs()
        if not prepared and not dry:
            print("The launcher finished but no prepared data appeared under %s." % horizon_home(), file=sys.stderr)
            return 7
    build_game_files(prepared[0] if prepared else "<prepared data>", data, dry)
    return 0


if __name__ == "__main__":
    sys.exit(main())
