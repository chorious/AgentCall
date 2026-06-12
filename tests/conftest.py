from __future__ import annotations

import os
from pathlib import Path


def pytest_configure(config) -> None:
    if getattr(config.option, "basetemp", None):
        return
    config.option.basetemp = str(Path.cwd() / f".pytest_tmp_{os.getpid()}")
