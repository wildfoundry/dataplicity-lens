# Optional host diagnostics

The cockpit header shows the CPU model, CPU sockets, physical cores and logical threads
visible to the OS. A VM reports virtual topology; a container may see the host topology,
which is not its CPU quota or affinity. Unknown counts remain `?`.

The OS/kernel header also shows a server service tag or serial when locally readable.
It gives that identity priority over kernel/uptime fields on narrower terminals.
Root-only DMI serials remain unavailable when Lens runs without the required permissions.

Open Hardware (`lens-hardware`) to see optional cloud and server context. JSON includes
`host.cpu` and `hardware.server` when available; the additions remain in schema version 2.

On AWS, Lens uses only the local EC2 IMDSv2 instance identity document and the ECS v3/v4
`/task` endpoint. It shows instance ID/type, AMI, region/AZ, private IP, cluster, task and
launch type when exposed. No IAM key or AWS SDK is required. It does not read IAM
credentials, user-data, task tags, container environments, or query the AWS control plane.
`curl` is optional. Requests bypass proxies and curl configuration, do not follow
redirects, and have short timeouts. ECS endpoints must use the known link-local address.
IMDS disabled by host policy or blocked by a container hop limit simply leaves fields
unavailable; Lens never changes those settings. Fargate does not expose its physical host.

On supported Dell, HPE, IBM and Lenovo servers, Lens reads the DMI service tag/system
serial. `ipmitool -I open lan print` can supply the BMC management IP via the local kernel
IPMI interface. SMBIOS type 42 can advertise a Redfish service address. Lens never
connects to the controller or needs its password. These local interfaces may require
administrator access; Lens does not invoke sudo or change permissions. Missing tools,
permissions or firmware data are optional and do not cause a health failure. Vendor
names identify the controller family, not a verified remote controller model.

Storage details (`lens-disk`, select a device and press Enter) show available health:

- `smartctl`: explicit SMART pass/fail, NVMe critical warning, media errors, endurance
  percentage used and temperature. No self-test or write is requested; standby drives
  are skipped when the device supports a power-mode check. SMART passing is not a guarantee against failure.
- An existing Dell PERC/Broadcom `perccli` or `storcli` installation: RAID volume state
  and physical member states, associated using the controller's explicit OS device name.
  Optimal, Degraded, Offline, Rebuilding and member states remain distinct from SMART.

Health is also in `storage_details.<device>.health`, with a source and observation time.
Explicit failures, degraded/rebuilding arrays, NVMe warnings, reported media errors and exhausted
endurance also appear as health findings. Missing health means unknown, never OK. Kernel link/device state and filesystem fullness
are separate checks. Hardware RAID may hide SMART and physical disks from the OS;
controller tools can still expose the member states. The initial implementation supports
PERC/StorCLI member reports; other controller-specific utilities are not automatically
queried. Lens uses installed tools and current privileges only. Optional host and drive
health probes are cached for up to 60 seconds and run outside rendering.

References: [EC2 metadata](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/configuring-instance-metadata-service.html),
[ECS task metadata](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/task-metadata-endpoint-v4.html).
