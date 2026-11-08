"""Entry point for the separately installed local infrastructure extension."""

from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from lens_infra.__main__ import main
from lens_infra.policy import Rejected

try:
    raise SystemExit(main())
except (Rejected, OSError, ValueError) as exc:
    import json
    message = str(exc) if isinstance(exc, Rejected) else type(exc).__name__ + ": local operation failed"
    print(json.dumps({"schema_version": 1, "error": message}), file=sys.stderr)
    raise SystemExit(1)
