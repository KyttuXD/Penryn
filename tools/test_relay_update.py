#!/usr/bin/env python3
"""Checks the relay's update platform names: python3 -I tools/test_relay_update.py (run by the test workflow)."""
import io
import os
import sys
import tarfile
import zipfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import relay  # noqa: E402

MACHO = {"macos-x86_64": b"\xcf\xfa\xed\xfe\x07\x00\x00\x01", "macos-arm64": b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01"}


def elf(machine):
    return b"\x7fELF\x02\x01\x01" + bytes(11) + machine.to_bytes(2, "little") + bytes(40)


def mac_zip(cpu):
    b = io.BytesIO()
    with zipfile.ZipFile(b, "w") as z:
        z.writestr("Penryn.app/Contents/MacOS/penryn-bin", MACHO[cpu])
        z.writestr("Penryn.app/Contents/Info.plist", "x")
    return b.getvalue()


def linux_tar(machine):
    b = io.BytesIO()
    with tarfile.open(fileobj=b, mode="w:gz") as t:
        data = elf(machine)
        i = tarfile.TarInfo("penryn-linux/penryn")
        i.size, i.mode = len(data), 0o755
        t.addfile(i, io.BytesIO(data))
    return b.getvalue()


def raises(f, *a):
    try:
        f(*a)
    except ValueError:
        return True
    return False


# Names: the old clients' "macos" and "linux" are the x86_64 builds, the new ones are exact, Windows is unknown.
assert relay.update_platform("macos") == "macos-x86_64"
assert relay.update_platform("linux") == "linux-x86_64"
for p in ("macos-x86_64", "macos-arm64", "linux-x86_64", "linux-aarch64"):
    assert relay.update_platform(p) == p
for p in ("windows-x86_64", "windows-arm64", "windows", "macos-arm", "", "linux-arm64"):
    assert raises(relay.update_platform, p), p
assert relay.UPDATE_FILES["macos-arm64"] == "penryn-macos-arm64.zip"
assert relay.UPDATE_FILES["linux-aarch64"] == "penryn-linux-aarch64.tar.gz"
assert relay.UPDATE_FILES[relay.update_platform("macos")] == "penryn-macos-x86_64.zip"
assert relay.UPDATE_FILES[relay.update_platform("linux")] == "penryn-linux-x86_64.tar.gz"

# The file that is unpacked must be for the asked processor: an Intel app is never sent to Apple Silicon.
assert relay.update_entries("macos", mac_zip("macos-x86_64"))
assert relay.update_entries("macos-arm64", mac_zip("macos-arm64"))
assert raises(relay.update_entries, "macos-arm64", mac_zip("macos-x86_64"))
assert raises(relay.update_entries, "macos-x86_64", mac_zip("macos-arm64"))
assert raises(relay.update_entries, "macos", mac_zip("macos-arm64"))
assert relay.update_entries("linux", linux_tar(0x3E))
assert relay.update_entries("linux-aarch64", linux_tar(0xB7))
assert raises(relay.update_entries, "linux-aarch64", linux_tar(0x3E))
assert raises(relay.update_entries, "linux-x86_64", linux_tar(0xB7))
assert [e[1] for e in relay.update_entries("linux-x86_64", linux_tar(0x3E))] == ["penryn"]
print("relay update names ok")
