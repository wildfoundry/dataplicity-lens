"""Entry point for the separately installed local infrastructure extension."""

from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from lens_infra.__main__ import cli

raise SystemExit(cli())
