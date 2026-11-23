#!/usr/bin/python3
"""Fixed runner registrations, with a new disposable KVM guest per job."""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess as sp
import time
import uuid
from contextlib import contextmanager
import fcntl
import hashlib
import selectors
import shlex
from . import runner_state
from .policy import require

ROOT=Path('/data/github-runner')
PRIVATE=Path('/etc/wildfoundry-runners')
# Host identity and addressing are local configuration, not embedded chassis IDs.
import socket
import ipaddress
import xml.etree.ElementTree as ET
HOST=socket.gethostname().split('.')[0]

def host_id():
    return settings().get('host_id', HOST)

def network():
    return settings().get('libvirt_network', 'wf-runners')

def guest_address(slot):
    xml=ET.fromstring(sp.check_output(['virsh','net-dumpxml',network()]))
    node=xml.find('ip')
    subnet=ipaddress.ip_network(node.get('address')+'/'+node.get('netmask',node.get('prefix')),strict=False)
    return str(subnet.network_address+settings().get('slot_address_offset',10)+slot)

def guest_mac(slot):
    address=ipaddress.ip_address(guest_address(slot))
    return f'52:54:00:81:{address.packed[2]:02x}:{slot:02x}'

def settings():
    return json.loads((PRIVATE/'capacity.json').read_text())

def resources(slot):
    cfg=settings()
    return cfg['slots'].get(str(slot), cfg['default'])

@contextmanager
def boot_lock():
    with (PRIVATE/'boot.lock').open('a') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        yield

def admission(memory_mib, replacing_slot=None):
    cfg=settings()
    mem={k:int(v.split()[0]) for k,v in (line.split(':',1) for line in Path('/proc/meminfo').read_text().splitlines())}
    if mem['MemAvailable']//1024 < cfg['reserve_host_mib']:
        raise RuntimeError('Capacity deferred: host memory reserve')
    stats=sp.check_output(['virsh','domstats','--balloon','--list-running'],text=True)
    allocations={}
    domain=None
    for line in stats.splitlines():
        if line.startswith("Domain: '"):
            domain=line.split("'",2)[1]
        if line.strip().startswith('balloon.maximum='):
            if domain is None: raise RuntimeError('Missing VM identity in memory statistics')
            allocations[domain]=int(line.split('=',1)[1])//1024
    marker=PRIVATE/'capacity-rollout-complete'
    # Preserve coexistence with old processes during a per-slot rollout. Their
    # source and rollout marker must not be overwritten while they still run.
    legacy=Path('/usr/local/lib/wildfoundry-runners/controller.py')
    expected=hashlib.sha256((legacy if legacy.exists() else Path(__file__)).read_bytes()).hexdigest()
    if not marker.exists() or marker.read_text().strip()!=expected:
        # Old controllers do not use the boot lock. Reserve their full old
        # allocation even while a guest is temporarily absent during recycling.
        for slot in range(1,9):
            name=f'{host_id()}-{slot:02}'
            allocations[name]=max(allocations.get(name,0),12288)
    if replacing_slot is not None:
        allocations.pop(f'{host_id()}-{replacing_slot:02}',None)
    committed=sum(allocations.values())
    if committed+memory_mib > mem['MemTotal']//1024-cfg['reserve_host_mib']:
        raise RuntimeError('Capacity deferred: VM memory budget')

def run(args, **kwargs):
    return sp.run(args, check=True, **kwargs)

def slot_info(slot):
    assert 1 <= slot <= settings()['max_slots']
    name=f'{host_id()}-{slot:02}'
    directory=ROOT/'vms'/name
    directory.mkdir(parents=True,exist_ok=True)
    return name,directory,guest_address(slot)

def ssh_args(slot):
    _,directory,ip=slot_info(slot)
    return ['ssh','-i',str(PRIVATE/'admin_ed25519'),'-o','BatchMode=yes',
            '-o','StrictHostKeyChecking=accept-new','-o',f'UserKnownHostsFile={directory}/known_hosts',
            '-o','ConnectTimeout=5','-o','ServerAliveInterval=20','-o','ServerAliveCountMax=6',f'vmadmin@{ip}']

def guest(slot, command, **kwargs):
    return run(ssh_args(slot)+[command],**kwargs)

def destroy(slot):
    require(not runner_state.held(slot), 'runner slot is held; VM and disk must be preserved')
    name,_,_=slot_info(slot)
    if sp.run(['virsh','dominfo',name],stdout=sp.DEVNULL,stderr=sp.DEVNULL).returncode==0:
        sp.run(['virsh','destroy',name],stdout=sp.DEVNULL,stderr=sp.DEVNULL)
        run(['virsh','undefine',name,'--nvram'],stdout=sp.DEVNULL)

def prepare(slot):
    if not os.path.ismount('/data'):
        raise RuntimeError('/data is not mounted')
    if shutil.disk_usage(ROOT).free < 50*1024**3:
        raise RuntimeError('Less than 50 GiB free: refusing another runner boot')
    with boot_lock():
        require(not runner_state.held(slot), 'runner slot is held; no replacement may be prepared')
        name,directory,ip=slot_info(slot)
        destroy(slot)
        limits=resources(slot)
        admission(limits['memory_mib'], replacing_slot=slot)
        for filename in ['disk.qcow2','seed.iso','user-data','meta-data','known_hosts']:
            (directory/filename).unlink(missing_ok=True)
        generation=str(uuid.uuid4())
        run(['qemu-img','create','-f','qcow2','-F','qcow2','-b',str((ROOT/'images/runner-base.qcow2').resolve()),str(directory/'disk.qcow2')],stdout=sp.DEVNULL)
        key=(PRIVATE/'admin_ed25519.pub').read_text().strip()
        (directory/'user-data').write_text(f'''#cloud-config
hostname: {name}
users:
  - name: vmadmin
    groups: [sudo]
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    lock_passwd: true
    ssh_authorized_keys:
      - {key}
ssh_pwauth: false
''')
        (directory/'meta-data').write_text(f'instance-id: {name}-{generation}\nlocal-hostname: {name}\n')
        run(['cloud-localds',str(directory/'seed.iso'),str(directory/'user-data'),str(directory/'meta-data')],stdout=sp.DEVNULL)
        run(['virt-install','--connect','qemu:///system','--name',name,'--memory',str(limits['memory_mib']),'--vcpus',str(limits['vcpus']),
             '--cpu','host-passthrough','--import','--disk',f'path={directory}/disk.qcow2,format=qcow2,bus=virtio,discard=unmap',
             '--disk',f'path={directory}/seed.iso,device=cdrom','--network',
             f'network={network()},model=virtio,mac={guest_mac(slot)}',
             '--channel','unix,target.type=virtio,target.name=org.qemu.guest_agent.0',
             '--osinfo','ubuntu24.04','--graphics','none','--noautoconsole'],stdout=sp.DEVNULL)
    for _ in range(90):
        p=sp.run(ssh_args(slot)+['sudo -n test -f /var/lib/cloud/instance/boot-finished'],stdout=sp.DEVNULL,stderr=sp.DEVNULL)
        if p.returncode==0:
            break
        time.sleep(2)
    else:
        raise RuntimeError('Guest did not become ready')
    (directory/'generation.json').write_text(json.dumps({'generation':generation,'created':time.time(),'ip':ip}))
    return generation

def inject(slot):
    # Registration keys are slot-specific and never embedded in the base image.
    files=PRIVATE/'slots'/f'{slot:02}'
    payload={f.name:f.read_text() for f in files.iterdir() if f.name=='.runner' or f.name.startswith('.credentials')}
    if '.runner' not in payload or '.credentials' not in payload:
        raise RuntimeError('Slot has no saved registration')
    code='''import json,sys,os,pwd
from pathlib import Path
u=pwd.getpwnam('runner')
for name,content in json.load(sys.stdin).items():
    assert name=='.runner' or name.startswith('.credentials')
    p=Path('/opt/actions-runner')/name
    p.write_text(content); p.chmod(0o600); os.chown(p,u.pw_uid,u.pw_gid)
'''
    import shlex
    guest(slot,'sudo -n python3 -c '+shlex.quote(code),input=json.dumps(payload).encode(),stdout=sp.DEVNULL)

def collect_logs(slot,generation):
    _,directory,_=slot_info(slot)
    target=ROOT/'logs'/f'{directory.name}-{generation}-diag.tgz'
    p=sp.Popen(ssh_args(slot)+['sudo -n tar -C /opt/actions-runner -czf - _diag'],stdout=sp.PIPE,stderr=sp.DEVNULL)
    try:
        with target.open('wb') as f:
            total=0
            while data:=p.stdout.read(65536):
                remaining=max(0,50*1024**2-total)
                f.write(data[:remaining]); total+=len(data)
                if total>=50*1024**2:
                    p.terminate(); break
        p.wait(timeout=30)
    except Exception:
        p.kill()
    # Retain only the newest 20 archives/logs for this slot.
    for suffix in ['*-diag.tgz','*.log']:
        matches=sorted((ROOT/'logs').glob(directory.name+'-'+suffix),key=lambda x:x.stat().st_mtime,reverse=True)
        for old in matches[20:]: old.unlink()

def park(slot):
    intent=runner_state.status(slot)
    require(intent and intent['mode']=='hold', 'runner has no hold intent')
    name,directory,_=slot_info(slot)
    identity=sp.check_output(['virsh','domuuid',name],text=True).strip()
    require(identity==intent['expected_uuid'], 'runner VM changed; refusing to park another generation')
    state=sp.check_output(['virsh','domstate',name],text=True).strip()
    if state!='shut off':
        result=stop_idle_admission(slot)
        if result.get('busy',True):
            runner_state.update(slot,intent,'waiting-for-job')
            return False
        # A stop request is not proof that the listener has exited.
        for _ in range(50):
            listener=sp.run(ssh_args(slot)+['pgrep -x Runner.Listener >/dev/null'],stdout=sp.DEVNULL,stderr=sp.DEVNULL,timeout=10)
            if listener.returncode==1:break
            if listener.returncode!=0:
                runner_state.update(slot,intent,'inspection-unavailable')
                return False
            time.sleep(.2)
        else:return False
        worker=sp.run(ssh_args(slot)+['pgrep -x Runner.Worker >/dev/null'],stdout=sp.DEVNULL,stderr=sp.DEVNULL,timeout=10)
        if worker.returncode!=1:
            runner_state.update(slot,intent,'waiting-for-job')
            return False
        run(['virsh','shutdown',name],stdout=sp.DEVNULL)
        for _ in range(120):
            if sp.check_output(['virsh','domstate',name],text=True).strip()=='shut off':break
            time.sleep(1)
        require(sp.check_output(['virsh','domstate',name],text=True).strip()=='shut off', 'runner graceful shutdown timed out; no force was attempted')
    require((directory/'disk.qcow2').exists(), 'held runner disk is missing')
    runner_state.update(slot,intent,'held',vm_uuid=identity,disks_preserved=True)
    return True


def stop_idle_admission(slot):
    # Freeze only the listener before the second worker check, so an idle
    # observation cannot immediately turn into a newly admitted job.
    code='''import json,os,pathlib,signal,time
def scan():
 r={}
 for p in pathlib.Path('/proc').glob('[0-9]*/comm'):
  try:
   c=p.read_text().strip()
   if c in ('Runner.Listener','Runner.Worker'):r[int(p.parent.name)]=c
  except OSError:pass
 return r
p=scan(); listeners=[pid for pid,c in p.items() if c=='Runner.Listener']
if 'Runner.Worker' in p.values():print(json.dumps({'busy':True}));raise SystemExit(0)
try:
 for pid in listeners:os.kill(pid,signal.SIGSTOP)
 time.sleep(.2)
 if 'Runner.Worker' in scan().values():print(json.dumps({'busy':True}));raise SystemExit(0)
 for pid in listeners:os.kill(pid,signal.SIGINT)
 print(json.dumps({'busy':False,'admission_stop_requested':True}))
finally:
 for pid in listeners:
  try:os.kill(pid,signal.SIGCONT)
  except ProcessLookupError:pass
'''
    result=guest(slot,'sudo -n python3 -I -c '+shlex.quote(code),stdout=sp.PIPE,stderr=sp.DEVNULL,timeout=15)
    return json.loads(result.stdout)


def resume_existing(slot,intent):
    name,directory,_=slot_info(slot)
    with boot_lock():
        identity=sp.check_output(['virsh','domuuid',name],text=True).strip()
        require(identity==intent['expected_uuid'], 'held runner identity changed')
        xml=ET.fromstring(sp.check_output(['virsh','dumpxml',name,'--inactive']))
        node=xml.find('memory')
        require(node.get('unit','KiB')=='KiB', 'unsupported runner memory unit')
        actual=int(node.text)//1024
        admission(actual,replacing_slot=slot)
        if sp.check_output(['virsh','domstate',name],text=True).strip()=='shut off':
            run(['virsh','start',name],stdout=sp.DEVNULL)
    for _ in range(90):
        if sp.run(ssh_args(slot)+['sudo -n test -f /var/lib/cloud/instance/boot-finished'],stdout=sp.DEVNULL,stderr=sp.DEVNULL).returncode==0:break
        time.sleep(2)
    else:raise RuntimeError('Preserved runner did not become ready')
    runner_state.update(slot,intent,'running',vm_uuid=identity,disks_preserved=True)
    return json.loads((directory/'generation.json').read_text())['generation']


def once(slot,resume=None):
    generation=resume_existing(slot,resume) if resume else prepare(slot)
    inject(slot)
    if runner_state.held(slot):
        park(slot)
        return 0
    if Path('/run/wf-runner-drain').exists():
        destroy(slot)
        return 0
    name,_,_=slot_info(slot)
    status_path=ROOT/'vms'/name/'status.json'
    status_path.write_text(json.dumps({'state':'listening','generation':generation,'started':time.time()}))
    print(f'{name}: fresh VM ready, listening for one job',flush=True)
    command='sudo -n -u runner bash -c '+__import__('shlex').quote('cd /opt/actions-runner && exec ./run.sh --once')
    p=sp.Popen(ssh_args(slot)+[command],stdout=sp.PIPE,stderr=sp.STDOUT)
    total=0
    with (ROOT/'logs'/f'{name}-{generation}.log').open('wb') as log:
        selector=selectors.DefaultSelector()
        selector.register(p.stdout,selectors.EVENT_READ)
        stop_requested=False
        try:
            while True:
                if runner_state.held(slot) and not stop_requested:
                    result=stop_idle_admission(slot)
                    stop_requested=not result.get('busy',True)
                ready=selector.select(timeout=1)
                if ready:
                    data=os.read(p.stdout.fileno(),4096)
                    if not data:break
                    log.write(data[:max(0,20*1024**2-total)])
                    log.flush(); total+=len(data)
                elif p.poll() is not None:break
        finally:selector.close()
    rc=p.wait()
    if runner_state.held(slot):
        park(slot)
        return rc
    status_path.write_text(json.dumps({'state':'recycling','generation':generation,'exit_code':rc,'finished':time.time()}))
    collect_logs(slot,generation)
    destroy(slot)
    print(f'{name}: listener exited {rc}; guest discarded',flush=True)
    return rc

def main(argv=None):
    p=argparse.ArgumentParser()
    p.add_argument('slot',type=int)
    p.add_argument('--prepare',action='store_true')
    args=p.parse_args(argv)
    _,directory,_=slot_info(args.slot)
    (directory/'controller-version.json').write_text(json.dumps({'pid':os.getpid(),'sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'started':time.time(),'resources':resources(args.slot)}))
    if args.prepare:
        print(prepare(args.slot)); return
    while True:
        intent=runner_state.status(args.slot)
        if intent and intent['mode']=='hold':
            if intent['stage']!='held':park(args.slot)
            time.sleep(1)
            continue
        if Path('/run/wf-runner-drain').exists():
            time.sleep(10)
            continue
        try:
            resume=intent if intent and intent['mode']=='resume' and intent['stage']=='requested' else None
            rc=once(args.slot,resume=resume)
            time.sleep(5 if rc==0 else 30)
        except Exception as e:
            print(f'Slot {args.slot}: {type(e).__name__}: {e}',flush=True)
            time.sleep(60)

if __name__=='__main__': main()

