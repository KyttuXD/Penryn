#!/usr/bin/env python3
"""Checks of tools/setup.py without any real launcher: fake folder trees and a fake horizon-launcher program.
    python3 -I tools/test_setup.py"""
import io
import os
import shutil
import stat
import sys
import tempfile
import contextlib

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import setup  # noqa: E402

ok = True


def check(what, got, want):
    global ok
    good = got == want
    ok &= good
    print("%s %s: %s" % ("ok  " if good else "FAIL", what, got if good else "%r (wanted %r)" % (got, want)))


base = os.path.join(os.path.expanduser("~"), ".cache", "claude-work", "setup-test")
shutil.rmtree(base, ignore_errors=True)
os.makedirs(base)


def jar(path, size=1_200_000):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.truncate(size)
    return path


try:
    # Layouts of the launchers.
    off = jar(os.path.join(base, "mc", "versions", "26.3", "26.3.jar"))
    prism = jar(os.path.join(base, "prism", "libraries", "com", "mojang", "minecraft", "26.3", "minecraft-26.3-client.jar"))
    modrinth = jar(os.path.join(base, "mr", "meta", "versions", "26.3", "26.3.jar"))
    jar(os.path.join(base, "mc", "versions", "1.21", "1.21.jar"))          # another version: ignored
    jar(os.path.join(base, "mc", "mods", "versions", "26.3", "26.3.jar"))   # inside a skipped folder: ignored
    jar(os.path.join(base, "tiny", "versions", "26.3", "26.3.jar"), 100)    # a stub: ignored
    roots = [("Minecraft (official)", os.path.join(base, "mc")), ("Prism Launcher", os.path.join(base, "prism")),
             ("Modrinth App", os.path.join(base, "mr")), ("Nothing", os.path.join(base, "none")), ("Tiny", os.path.join(base, "tiny"))]
    found = setup.scan(roots=roots)
    check("scan finds the three real layouts", sorted((i["launcher"], os.path.basename(i["jar"])) for i in found),
          sorted([("Minecraft (official)", "26.3.jar"), ("Prism Launcher", "minecraft-26.3-client.jar"), ("Modrinth App", "26.3.jar")]))
    check("a folder named by the user comes first", setup.scan(extra=os.path.join(base, "prism"), roots=[])[0]["launcher"], "the folder you named")
    check("depth is limited", len(setup.find_jars(os.path.join(base, "mc"), depth=1)), 0)
    check("data folders per OS", setup.penryn_data_dir() == (os.environ.get("PENRYN_DATA") or setup.penryn_data_dir()), True)

    # The whole flow with a fake launcher: it logs its calls and makes prepared data appear when asked to prepare.
    home = os.path.join(base, "hhome")
    log = os.path.join(base, "calls.txt")
    fake = os.path.join(base, "horizon-launcher")
    with open(fake, "w") as f:
        f.write("""#!/bin/sh
echo "$@" >> '%s'
case "$1" in
  versions) exit 0 ;;                       # no version installed
  install) mkdir -p '%s/versions/v1/.local/horizon-client/26.3'; touch '%s/versions/v1/.local/horizon-client/26.3/gameplay-facts.json'; exit 0 ;;
  prepare) [ "$2" = "--download" ] && exit 0; [ "$3" = "%s" ] && exit 0; exit 1 ;;
  account) echo '{"accounts": []}'; exit 0 ;;
esac
exit 0
""" % (log, home, home, os.path.join(base, "prism")))
    os.chmod(fake, os.stat(fake).st_mode | stat.S_IEXEC)
    os.environ["HORIZON_LAUNCHER"] = fake
    os.environ["HORIZON_LAUNCHER_HOME"] = home
    os.environ["PENRYN_DATA"] = os.path.join(base, "data")
    def run_main(args, scan_roots, fresh=True):
        """setup.main with a fake folder scan and a recording generator step; returns (code, stdout, stderr, builds)."""
        if fresh:
            shutil.rmtree(home, ignore_errors=True)
            if os.path.exists(log):
                os.remove(log)
        builds = []
        o, e = io.StringIO(), io.StringIO()
        saved = (setup.data_roots, setup.build_game_files)
        setup.data_roots = lambda: scan_roots
        setup.build_game_files = lambda prepared, data, dry: builds.append((prepared, data)) or True
        try:
            with contextlib.redirect_stdout(o), contextlib.redirect_stderr(e):
                code = setup.main(args)
        finally:
            setup.data_roots, setup.build_game_files = saved
        calls = open(log).read().splitlines() if os.path.exists(log) else []
        return code, o.getvalue(), e.getvalue(), builds, calls

    mc_dir, prism_dir = os.path.join(base, "mc"), os.path.join(base, "prism")
    code, text, _, builds, calls = run_main(["--dry-run", "--yes"], roots)
    check("dry run: installs a version, then prepares from the first install (a dry run assumes success)",
          [l.strip() for l in text.splitlines() if l.strip().startswith("$")],
          ["$ %s install latest" % fake, "$ %s prepare --minecraft %s" % (fake, mc_dir)])
    check("dry run ends in the generator step", (code, len(builds)), (0, 1))
    check("a dry run only asked what is installed", calls, ["versions --installed"])

    # A real run: the first install fails to prepare, the second works, then the generator runs on the prepared data.
    code, text, _, builds, calls = run_main(["--yes"], roots)
    check("real run: install, first prepare fails, second works", calls,
          ["versions --installed", "install latest", "prepare --minecraft " + mc_dir, "prepare --minecraft " + prism_dir])
    check("real run builds from the prepared data into the data folder",
          (code, [d for _, d in builds], [p.endswith("26.3") for p, _ in builds]), (0, [os.path.join(base, "data")], [True]))

    # No install anywhere and no --download: a clear failure, nothing downloaded.
    code, _, err, builds, calls = run_main(["--yes"], [])
    check("nothing found and no --download", (code, "prepare --download" in calls, "--download" in err, builds), (4, False, True, []))

    # With --download: sign in first (no account), then the download.
    code, _, _, builds, calls = run_main(["--yes", "--download"], [])
    check("--download signs in, then downloads", (calls[-3:], code, len(builds)), (["account --json", "account sign-in", "prepare --download"], 0, 1))
finally:
    shutil.rmtree(base, ignore_errors=True)
print("all ok" if ok else "FAILED")
sys.exit(0 if ok else 1)
