"""Bounded software VXLAN fabric. Physical interfaces and existing routes are untouched."""

import hashlib
import ipaddress
import json

from .backend import run
from .policy import require


def vxlan_name(network):
    return "lv" + hashlib.sha256(network["libvirt_network"].encode()).hexdigest()[:10]


def member(policy):
    return next(m for m in policy["fabric"]["members"] if m["host_id"] == policy["host_id"])


def alias(policy, key):
    return "lens-infra:" + policy["host_id"] + ":" + key


def link(name):
    result = run(["/usr/sbin/ip", "-d", "-j", "link", "show", "dev", name], check=False)
    return json.loads(result)[0] if result else None


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
    links = json.loads(run(["/usr/sbin/ip", "-d", "-j", "link", "show"]))
    for item in links:
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
