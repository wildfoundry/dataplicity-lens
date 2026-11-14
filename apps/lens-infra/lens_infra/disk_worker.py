"""Offline filesystem tooling in a network-disabled libguestfs appliance.

Only a controller-created standalone working copy is writable. The host kernel
never mounts customer filesystems. No native error/file content is returned.
"""

import json
import os
from pathlib import Path
import sys

BIOS = "21686148-6449-6e6f-744e-656564454649"
EFI = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"


class Unsupported(Exception):
    pass


def need(value, reason):
    if not value:
        raise Unsupported(reason)


def handle(path, workdir, readonly=False):
    import guestfs
    g = guestfs.GuestFS(python_return_dict=True)
    g.set_backend("direct")
    g.set_network(False)
    g.set_memsize(768)
    g.set_cachedir(str(workdir))
    g.set_tmpdir(str(workdir))
    g.add_drive_opts(str(path), format="qcow2", readonly=readonly)
    g.launch()
    return g


def layout(g, target_bytes):
    need(g.list_devices() == ["/dev/sda"], "unsupported_multi_disk_layout")
    need(g.part_get_parttype("/dev/sda") == "gpt", "unsupported_partition_table")
    need(not g.pvs() and not g.lvs() and not g.list_md_devices(), "incompatible_storage_layer")
    parts = g.part_list("/dev/sda")
    need(1 <= len(parts) <= 16, "unsupported_partition_layout")
    filesystems = g.list_filesystems()
    allowed = set()
    for p in parts:
        device = "/dev/sda" + str(p["part_num"])
        fstype = filesystems.get(device, "unknown")
        kind = g.part_get_gpt_type("/dev/sda", p["part_num"]).lower()
        if fstype == "ext4":
            allowed.add(device)
        elif kind == EFI and fstype == "vfat":
            allowed.add(device)
        elif kind == BIOS and fstype == "unknown" and p["part_size"] <= 8 * 2**20:
            allowed.add(device)
        else:
            raise Unsupported("incompatible_filesystem")
    need(set(filesystems) <= allowed, "incompatible_storage_layer")
    root = max(parts, key=lambda p: p["part_end"])
    device = "/dev/sda" + str(root["part_num"])
    need(filesystems.get(device) == "ext4", "unsupported_root_filesystem")
    g.mount_ro(device, "/")
    need(g.is_file("/etc/os-release", followsymlinks=True), "unsupported_root_layout")
    g.umount_all()
    end_sector = target_bytes // 512 - 34
    root_bytes = (end_sector + 1) * 512 - root["part_start"]
    need(root_bytes > 0 and all(p == root or p["part_end"] < root["part_start"] for p in parts), "destination_too_small")
    minimum = g.vfs_minimum_size(device)
    need(minimum <= root_bytes // 4096 * 4096, "contents_do_not_fit")
    return {"parts": parts, "filesystems": filesystems, "root": device, "root_number": root["part_num"],
            "root_bytes": root_bytes // 4096 * 4096, "end_sector": end_sector,
            "uuids": {d: g.vfs_uuid(d) for d, t in filesystems.items() if t != "unknown"},
            "disk_guid": g.part_get_disk_guid("/dev/sda"),
            "partition_guids": {str(p["part_num"]): g.part_get_gpt_guid("/dev/sda", p["part_num"]) for p in parts},
            "partition_types": {str(p["part_num"]): g.part_get_gpt_type("/dev/sda", p["part_num"]) for p in parts}}


def manifest(g, spec, workdir):
    """Checksum every regular file on every supported filesystem, privately."""
    result = {}
    for device, fstype in spec["filesystems"].items():
        if fstype == "unknown":
            continue
        g.mount_ro(device, "/")
        output = workdir / (device.rsplit("/", 1)[-1] + ".checksums")
        g.checksums_out("sha256", "/", str(output))
        # Compare sorted records because filesystem iteration order can change.
        result[device] = sorted(output.read_bytes().splitlines())
        output.unlink()
        g.umount_all()
    return result


def prepare(image, target_bytes, workdir):
    """Shrink filesystems before partition boundaries, on the disposable copy."""
    g = handle(image, workdir)
    try:
        spec = layout(g, target_bytes)
        before = manifest(g, spec, workdir)
        for device, fstype in spec["filesystems"].items():
            if fstype == "ext4":
                g.e2fsck_f(device)
        if target_bytes < g.blockdev_getsize64("/dev/sda"):
            g.resize2fs_size(spec["root"], spec["root_bytes"])
            g.e2fsck_f(spec["root"])
            original = next(p for p in spec["parts"] if p["part_num"] == spec["root_number"])
            start = original["part_start"] // 512
            length = spec["end_sector"] - start + 1
            # Current parted prompts even through libguestfs's non-interactive
            # resizepart API. sfdisk -N changes only this existing GPT entry.
            g.sfdisk_N("/dev/sda", spec["root_number"], 0, 0, 0, f"start={start}, size={length}")
        need(manifest(g, spec, workdir) == before, "content_verification_failed")
        g.sync()
        return spec
    finally:
        g.shutdown()
        g.close()


def verify(image, target_bytes, spec, workdir):
    g = handle(image, workdir)
    try:
        need(g.blockdev_getsize64("/dev/sda") == target_bytes, "virtual_size_verification_failed")
        # Relocate the backup GPT to the new end, from the intact primary table.
        g.part_expand_gpt("/dev/sda")
        need(g.part_get_disk_guid("/dev/sda") == spec["disk_guid"], "disk_identity_changed")
        need(g.list_filesystems() == spec["filesystems"], "filesystem_identity_changed")
        parts = g.part_list("/dev/sda")
        for original in spec["parts"]:
            current = next(p for p in parts if p["part_num"] == original["part_num"])
            need(current["part_start"] == original["part_start"], "partition_start_changed")
            number = original["part_num"]
            need(g.part_get_gpt_guid("/dev/sda", number) == spec["partition_guids"][str(number)], "partition_uuid_changed")
            need(g.part_get_gpt_type("/dev/sda", number) == spec["partition_types"][str(number)], "partition_type_changed")
            if original["part_num"] != spec["root_number"]:
                need(current == original, "boot_partition_changed")
            need(current["part_end"] < target_bytes - 33 * 512, "partition_outside_destination")
        for device, uid in spec["uuids"].items():
            need(g.vfs_uuid(device) == uid, "filesystem_uuid_changed")
            if spec["filesystems"][device] == "ext4":
                g.e2fsck_f(device)
        g.sync()
    finally:
        g.shutdown()
        g.close()


def main():
    # Paths come only from the controller's owned VM storage directory.
    mode, image_name, size, work_name = sys.argv[1:]
    image, workdir = Path(image_name), Path(work_name)
    need(mode in ("prepare", "verify"), "invalid_tool_request")
    need(image.is_file() and not image.is_symlink() and image.parent == workdir.parent, "invalid_working_copy")
    need(workdir.is_dir() and not workdir.is_symlink(), "invalid_working_directory")
    specfile = workdir / "layout.json"
    if mode == "prepare":
        spec = prepare(image, int(size), workdir)
        specfile.write_text(json.dumps(spec));specfile.chmod(0o600)
    else:
        verify(image, int(size), json.loads(specfile.read_text()), workdir)
    print(json.dumps({"ok": True}))


if __name__ == "__main__":
    try:
        main()
    except ImportError:
        print(json.dumps({"ok": False, "reason": "offline_filesystem_tools_unavailable"}))
    except Unsupported as exc:
        print(json.dumps({"ok": False, "reason": str(exc)}))
    except Exception:
        print(json.dumps({"ok": False, "reason": "filesystem_check_or_resize_failed"}))
