"""Runner coordination must work with the installed service's read-only /etc."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


@unittest.skipUnless(sys.platform == "linux" and os.geteuid() == 0, "requires Linux root mount namespaces")
class AdmissionSandboxTests(unittest.TestCase):
    def test_readonly_lock_mount_still_coordinates_exclusive_admission(self):
        code = '''import errno,fcntl,os,pathlib,subprocess,sys
from lens_infra import runner_provider
assert pathlib.Path('/proc/self/ns/mnt').stat().st_ino != pathlib.Path('/proc/1/ns/mnt').stat().st_ino
path=sys.argv[1]
subprocess.run(['/usr/bin/mount','--bind',path,path],check=True)
subprocess.run(['/usr/bin/mount','-o','remount,bind,ro',path],check=True)
try:
 fd=os.open(path,os.O_RDWR)
except OSError as exc:
 assert exc.errno==errno.EROFS
else:
 os.close(fd); raise AssertionError('fixture lock mount was writable')
runner_provider.BOOT_LOCK=pathlib.Path(path)
probe='import fcntl,os,sys;f=os.open(sys.argv[1],os.O_RDONLY)\\ntry: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)\\nexcept BlockingIOError: sys.exit(7)'
with runner_provider.admission_lock({'runner_controller':{'provider':'wildfoundry-runner-v1'}}):
 assert subprocess.run([sys.executable,'-c',probe,path]).returncode==7
assert subprocess.run([sys.executable,'-c',probe,path]).returncode==0
'''
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "boot.lock"
            path.write_bytes(b"")
            path.chmod(0o600)
            result = subprocess.run(["/usr/bin/unshare", "--mount", "--propagation", "private", sys.executable, "-c", code, str(path)],
                                    capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
