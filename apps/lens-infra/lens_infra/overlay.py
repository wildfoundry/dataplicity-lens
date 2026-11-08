"""Bounded software VXLAN fabric. Physical interfaces and existing routes are untouched."""

import hashlib
import ipaddress
import json
import socket
import struct

from .backend import run
from .policy import require


def vxlan_name(network):
    return "lv" + hashlib.sha256(network["libvirt_network"].encode()).hexdigest()[:10]


def member(policy):
    return next(m for m in policy["fabric"]["members"] if m["host_id"] == policy["host_id"])


def alias(policy, key):
    return "lens-infra:" + policy["host_id"] + ":" + key


def attributes(data):
    result, offset = {}, 0
    while offset + 4 <= len(data):
        length, kind = struct.unpack_from("=HH", data, offset)
        require(4 <= length <= len(data) - offset, "invalid kernel link attribute")
        result[kind & 0x3fff] = data[offset + 4:offset + length]
        offset += (length + 3) & ~3
    return result


def links():
    """Read RTM_GETLINK, avoiding old iproute2 VXLAN JSON/kernel ABI bugs.

    Linux UAPI: include/uapi/linux/{rtnetlink,if_link}.h. This socket only
    requests a dump; all mutations still use the narrow, explicit ip commands.
    """
    result = []
    with socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, socket.NETLINK_ROUTE) as channel:
        channel.settimeout(3)
        channel.bind((0, 0))
        payload = struct.pack("=BxhiII", socket.AF_UNSPEC, 0, 0, 0, 0)
        channel.sendto(struct.pack("=IHHII", 16 + len(payload), 18, 0x301, 1, 0) + payload, (0, 0))
        while True:
            data, _, flags, sender = channel.recvmsg(256 * 1024)
            require(sender[0] == 0 and not flags & socket.MSG_TRUNC, "incomplete kernel link response")
            offset = 0
            while offset + 16 <= len(data):
                length, kind, flags, sequence, _ = struct.unpack_from("=IHHII", data, offset)
                require(16 <= length <= len(data) - offset and sequence == 1, "invalid kernel link response")
                require(not flags & 0x10, "network state changed during verification; retry")
                body = data[offset + 16:offset + length]
                offset += (length + 3) & ~3
                if kind == 3:  # NLMSG_DONE
                    require(len(body) < 4 or struct.unpack_from("=i", body)[0] == 0, "kernel link dump failed")
                    return result
                if kind == 2:  # NLMSG_ERROR
                    require(len(body) >= 4 and struct.unpack_from("=i", body)[0] == 0, "kernel rejected link query")
                    continue
                if kind != 16:  # RTM_NEWLINK
                    continue
                require(len(body) >= 16 and len(result) < 4096, "invalid or excessive kernel link inventory")
                _, _, index, state, _ = struct.unpack_from("=BxhiII", body)
                values = attributes(body[16:])
                string = lambda value: value.rstrip(b"\0").decode(errors="replace")
                info = attributes(values.get(18, b""))  # IFLA_LINKINFO
                info_kind, details = string(info.get(1, b"")), {}
                if info_kind == "vxlan":
                    vxlan = attributes(info.get(2, b""))
                    if 1 in vxlan: details["id"] = struct.unpack("=I", vxlan[1])[0]
                    if 4 in vxlan: details["local"] = socket.inet_ntop(socket.AF_INET, vxlan[4])
                    if 15 in vxlan: details["dstport"] = struct.unpack("!H", vxlan[15])[0]
                result.append({"ifname": string(values.get(3, b"")), "ifindex": index,
                    "ifalias": string(values.get(20, b"")), "mtu": struct.unpack("=I", values[4])[0],
                    "flags": ["UP"] if state & 1 else [],
                    "linkinfo": {"info_kind": info_kind, "info_data": details}})


def link(name):
    return next((value for value in links() if value["ifname"] == name), None)


def preflight(policy):
    if not any(n["mode"] == "vxlan" for n in policy["networks"].values()):
        return
    local = member(policy)
    addresses = json.loads(run(["/usr/sbin/ip", "-j", "address", "show"]))
    require(any(a.get("local") == local["endpoint"] for iface in addresses for a in iface.get("addr_info", [])), "fabric endpoint is not assigned to this host")
    for peer in policy["fabric"]["members"]:
        if peer["host_id"] == policy["host_id"]:
            continue
        routes = json.loads(run(["/usr/sbin/ip", "-j", "route", "get", peer["endpoint"]]))
        require(routes and routes[0].get("prefsrc", routes[0].get("src")) == local["endpoint"], "peer route does not use the approved fabric endpoint")
    for item in links():
        if item.get("linkinfo", {}).get("info_kind") == "vxlan":
            require(item.get("ifalias", "").startswith("lens-infra:" + policy["host_id"] + ":"), "unmanaged VXLAN links exist; do not take over their transport firewall")


def ensure(policy, key, primary_verified=False):
    from .network import bridge_name
    network = policy["networks"][key]
    if network["mode"] != "vxlan":
        return
    local, fabric = member(policy), policy["fabric"]
    primary = policy["host_id"] == fabric["gateway_host"]
    bridge, tunnel = bridge_name(network), vxlan_name(network)
    marker, mtu = alias(policy, key), fabric.get("mtu", 1350)
    current = link(bridge)
    if current:
        require(current.get("linkinfo", {}).get("info_kind") == "bridge", "overlay bridge name belongs to another link type")
        require(current.get("ifalias") == marker or (primary and primary_verified and not current.get("ifalias")), "overlay bridge is not owned by this host/network")
    else:
        require(not primary, "gateway bridge must be created by its owned libvirt network first")
        run(["/usr/sbin/ip", "link", "add", bridge, "type", "bridge"])
    run(["/usr/sbin/ip", "link", "set", bridge, "alias", marker])
    run(["/usr/sbin/ip", "link", "set", bridge, "mtu", str(mtu), "up"])
    subnet = ipaddress.ip_network(network["cidr"])
    address = str(subnet.network_address + local["node_id"] + 1) + "/" + str(subnet.prefixlen)
    assigned = json.loads(run(["/usr/sbin/ip", "-j", "address", "show", "dev", bridge]))
    if not any(v.get("local") == address.split("/")[0] for v in assigned[0].get("addr_info", [])):
        run(["/usr/sbin/ip", "address", "add", address, "dev", bridge])
    current = link(tunnel)
    if current:
        info = current.get("linkinfo", {})
        require(current.get("ifalias") == marker and info.get("info_kind") == "vxlan", "refusing to change an unowned overlay link")
        attrs = info.get("info_data", {})
        require(attrs.get("id") == network["vni"] and attrs.get("local") == local["endpoint"] and attrs.get("dstport") == 4789, "overlay link does not match the approved network")
    else:
        run(["/usr/sbin/ip", "link", "add", tunnel, "type", "vxlan", "id", str(network["vni"]), "local", local["endpoint"], "dstport", "4789"])
        run(["/usr/sbin/ip", "link", "set", tunnel, "alias", marker])
    run(["/usr/sbin/ip", "link", "set", tunnel, "mtu", str(mtu), "master", bridge, "up"])
    entries = json.loads(run(["/usr/sbin/bridge", "-j", "fdb", "show", "dev", tunnel]))
    for peer in fabric["members"]:
        if peer["host_id"] == policy["host_id"]:
            continue
        if not any(e.get("mac") == "00:00:00:00:00:00" and e.get("dst") == peer["endpoint"] for e in entries):
            run(["/usr/sbin/bridge", "fdb", "append", "00:00:00:00:00:00", "dev", tunnel, "dst", peer["endpoint"]])


def verify(policy, key):
    from .network import bridge_name
    network = policy["networks"][key]
    if network["mode"] != "vxlan":
        return
    bridge, tunnel = link(bridge_name(network)), link(vxlan_name(network))
    require(bridge and tunnel and bridge.get("ifalias") == alias(policy, key) and tunnel.get("ifalias") == alias(policy, key), "software network is absent or ownership changed")
    attrs = tunnel.get("linkinfo", {}).get("info_data", {})
    require(attrs.get("id") == network["vni"] and attrs.get("local") == member(policy)["endpoint"], "software network transport does not match policy")
    require(bridge.get("mtu") == policy["fabric"].get("mtu", 1350) and tunnel.get("mtu") == bridge.get("mtu"), "overlay MTU does not match policy")
