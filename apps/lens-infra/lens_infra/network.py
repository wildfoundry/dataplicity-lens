"""Administrator-only network creation and scoped, default-deny environment rules."""

from __future__ import annotations

import hashlib
import ipaddress
import json
import os
import tempfile
import uuid
import xml.etree.ElementTree as ET
from pathlib import Path

from .backend import NS, run
from .policy import digest, require


def bridge_name(network):
    return network.get("bridge") or "li" + hashlib.sha256(network["libvirt_network"].encode()).hexdigest()[:10]


def network_xml(policy, key):
    n = policy["networks"][key]
    require(n.get("managed") and n["mode"] in ("isolated", "nat", "vxlan"), "only new Lens-owned networks can be provisioned")
    subnet = ipaddress.ip_network(n["cidr"])
    node = ET.Element("network")
    ET.SubElement(node, "name").text = n["libvirt_network"]
    ET.SubElement(node, "uuid").text = str(uuid.uuid5(uuid.NAMESPACE_URL, policy["host_id"] + "/network/" + key))
    metadata = ET.SubElement(node, "metadata")
    ET.SubElement(metadata, f"{{{NS}}}network", {"host": policy["host_id"], "environment": n["environment"], "binding": key})
    ET.SubElement(node, "bridge", {"name": bridge_name(n), "stp": "on", "delay": "0"})
    if n["mode"] == "vxlan":
        ET.SubElement(node, "mtu", {"size": str(policy["fabric"].get("mtu", 1350))})
        if policy["host_id"] != policy["fabric"]["gateway_host"]:
            ET.SubElement(node, "forward", {"mode": "bridge"})
            return ET.tostring(node)
    if n["mode"] in ("nat", "vxlan"):
        ET.SubElement(node, "forward", {"mode": "nat"})
    ip = ET.SubElement(node, "ip", {"address": str(subnet.network_address + 1), "netmask": str(subnet.netmask)})
    dhcp = ET.SubElement(ip, "dhcp")
    # Reserve .1 for the gateway and .2 through .33 for stable fabric node IDs.
    start = 34 if n["mode"] == "vxlan" else 2
    ET.SubElement(dhcp, "range", {"start": str(subnet.network_address + start), "end": str(subnet.broadcast_address - 1)})
    return ET.tostring(node)


def firewall(policy):
    networks = list(policy["networks"].values())
    require(networks, "no environment networks configured")
    pairs = [(n, bridge_name(n)) for n in networks]
    bridges = ", ".join(json.dumps(b) for _, b in pairs)
    marker = firewall_marker(policy)
    lines = ["table inet lens_infra {", f' comment "{marker}"', " chain host_input {",
             "  type filter hook input priority -10; policy accept;"]
    if policy.get("fabric") and any(n["mode"] == "vxlan" for n in networks):
        peers = [json.dumps(m["endpoint"]) for m in policy["fabric"]["members"] if m["host_id"] != policy["host_id"]]
        lines += [f"  iifname {{ {bridges} }} udp dport 4789 drop",
                  "  udp dport 4789 ip saddr { " + ", ".join(v.strip('"') for v in peers) + " } accept",
                  "  udp dport 4789 drop"]
    lines += [
             f"  iifname {{ {bridges} }} ct state established,related accept",
             f"  iifname {{ {bridges} }} udp dport {{ 53, 67, 68 }} accept",
             f"  iifname {{ {bridges} }} tcp dport 53 accept",
             f"  iifname {{ {bridges} }} icmpv6 type {{ nd-neighbor-solicit, nd-neighbor-advert }} accept",
             f"  iifname {{ {bridges} }} drop", " }", " chain forwarding {",
             "  type filter hook forward priority -10; policy accept;"]
    for net, bridge in pairs:
        if policy.get("fabric"):
            lines.append(f"  iifname {json.dumps(bridge)} ip daddr {{ {', '.join(policy['fabric']['protected_cidrs'])} }} drop")
            lines.append(f"  iifname {json.dumps(bridge)} udp dport 4789 drop")
        others = [json.dumps(b) for n, b in pairs if n["environment"] != net["environment"]]
        subnets = [n["cidr"] for n, b in pairs if n["environment"] != net["environment"]]
        if others:
            lines.append(f"  iifname {json.dumps(bridge)} oifname {{ {', '.join(others)} }} drop")
        if subnets:
            lines.append(f"  iifname {json.dumps(bridge)} ip daddr {{ {', '.join(subnets)} }} drop")
        if net.get("egress"):
            lines.append(f"  iifname {json.dumps(bridge)} ip daddr {{ {', '.join(net['egress'])} }} accept")
        lines.append(f"  iifname {json.dumps(bridge)} oifname != {json.dumps(bridge)} drop")
    lines += [" }", "}", "table bridge lens_infra {", f' comment "{marker}"', " chain tagged_guests {",
              "  type filter hook forward priority -10; policy accept;",
              f'  meta ibrname {{ {bridges} }} iifname "vnet*" ether type {{ 0x8100, 0x88a8 }} drop',
              f'  meta ibrname {{ {bridges} }} iifname "vnet*" ether type ip udp sport 67 drop',
              f'  meta ibrname {{ {bridges} }} iifname "vnet*" ether type ip6 udp sport 547 drop',
              f'  meta ibrname {{ {bridges} }} iifname "vnet*" ether type ip6 icmpv6 type {{ nd-router-advert, nd-redirect }} drop',
              " }", "}"]
    return "\n".join(lines) + "\n"


def apply_firewall(policy):
    marker = firewall_marker(policy)
    prefix = []
    for family in ("inet", "bridge"):
        existing = run(["/usr/sbin/nft", "-j", "list", "table", family, "lens_infra"], check=False)
        if existing:
            data = json.loads(existing)
            tables = [v["table"] for v in data.get("nftables", []) if "table" in v]
            require(len(tables) == 1 and tables[0].get("comment", "").startswith("lens-infra:" + policy["host_id"] + ":"), "refusing to modify an unowned firewall table")
            prefix.append(f"delete table {family} lens_infra")
    payload = ("\n".join(prefix) + "\n" + firewall(policy)).encode()
    # Both tables are replaced as one nftables transaction; no global flush or service restart.
    run(["/usr/sbin/nft", "--check", "--file", "-"], data=payload)
    run(["/usr/sbin/nft", "--file", "-"], data=payload)


def firewall_marker(policy):
    return "lens-infra:" + policy["host_id"] + ":" + digest({"networks": policy["networks"], "fabric": policy.get("fabric")})[:24]


def verify_firewall(policy):
    for family in ("inet", "bridge"):
        result = run(["/usr/sbin/nft", "-j", "list", "table", family, "lens_infra"], check=False)
        require(result is not None, "environment firewall is not installed; run the reviewed local network apply first")
        tables = [r["table"] for r in json.loads(result).get("nftables", []) if "table" in r]
        require(len(tables) == 1 and tables[0].get("comment") == firewall_marker(policy), "environment firewall does not match the active network configuration")


def provision(policy, backend, apply=False):
    from . import overlay
    overlay.preflight(policy)
    plans = []
    routes = json.loads(run(["/usr/sbin/ip", "-j", "route", "show", "table", "all"]))
    for key, net in policy["networks"].items():
        existing = backend.virsh("net-dumpxml", net["libvirt_network"], check=False)
        if existing:
            x = ET.fromstring(existing)
            ownership = x.find(f"metadata/{{{NS}}}network")
            require((not net.get("managed") and net["mode"] == "vlan") or
                    (ownership is not None and ownership.get("host") == policy["host_id"] and ownership.get("binding") == key),
                    "existing networks are protected; do not overwrite or adopt their configuration")
            backend.verify_network(net, policy)
            plans.append({"network": key, "action": "verify"})
            continue
        subnet = ipaddress.ip_network(net["cidr"])
        for route in routes:
            destination = route.get("dst")
            if destination and destination != "default":
                try:
                    other = ipaddress.ip_network(destination, strict=False)
                except ValueError:
                    continue
                require(other.version != subnet.version or not subnet.overlaps(other), "new network overlaps an existing host route")
        xml = network_xml(policy, key)
        require(overlay.link(bridge_name(net)) is None, "new managed bridge name already exists; existing links are protected")
        plans.append({"network": key, "action": "create", "bridge": bridge_name(net), "cidr": net["cidr"]})
        if apply:
            require(os.geteuid() == 0, "network provisioning requires local administrator authority")
            with tempfile.NamedTemporaryFile(suffix=".xml") as f:
                f.write(xml)
                f.flush()
                backend.virsh("net-define", f.name)
    if apply:
        apply_firewall(policy)
        for key, net in policy["networks"].items():
            # Defining a network and its protection precedes making it available to guests.
            if net["mode"] == "vxlan" and policy["host_id"] != policy["fabric"]["gateway_host"]:
                overlay.ensure(policy, key)
            if net.get("managed"):
                backend.virsh("net-start", net["libvirt_network"], check=False)
                backend.virsh("net-autostart", net["libvirt_network"])
            if net["mode"] == "vxlan":
                overlay.ensure(policy, key, primary_verified=True)
                overlay.verify(policy, key)
            backend.verify_network(net, policy)
    return {"networks": plans, "firewall": firewall(policy), "applied": apply,
            "notice": "Creates only declared Lens-owned bridges/networks. Changes host routing and scoped firewall rules; review before applying on live build hosts."}


def restore_firewall(policy, backend):
    """Restore local protection before starting any recorded VM after host boot."""
    for key, net in policy["networks"].items():
        if net.get("managed"):
            xml = ET.fromstring(backend.virsh("net-dumpxml", net["libvirt_network"]))
            owner = xml.find(f"metadata/{{{NS}}}network")
            require(owner is not None and owner.get("host") == policy["host_id"] and owner.get("binding") == key,
                    "network ownership changed; refusing automatic recovery")
        backend.verify_network(net, policy)
    apply_firewall(policy)
    from . import overlay
    overlay.preflight(policy)
    for key, net in policy["networks"].items():
        if net["mode"] != "vxlan":
            continue
        if policy["host_id"] != policy["fabric"]["gateway_host"]:
            overlay.ensure(policy, key)
        backend.virsh("net-start", net["libvirt_network"], check=False)
        overlay.ensure(policy, key, primary_verified=True)
        overlay.verify(policy, key)
    verify_firewall(policy)
