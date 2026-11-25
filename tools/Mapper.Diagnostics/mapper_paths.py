"""Shared Mapper storage and atomic cache publication."""
import os
import time
from pathlib import Path


def data_root():
    local = Path(os.environ.get("LOCALAPPDATA", "."))
    return local / "Mapper"


def publish_directory(stage, destination):
    """Publish a completed private directory; retry only transient Windows locks."""
    stage, destination = Path(stage), Path(destination)
    root = destination.parent.resolve(strict=True)
    source = stage.resolve(strict=True)
    if (not stage.is_dir() or stage.is_symlink()
            or getattr(stage, 'is_junction', lambda: False)()
            or source == root or not source.is_relative_to(root)
            or source == destination.resolve(strict=False)):
        raise ValueError('Publication must move a private staging directory within its cache root')
    for attempt in range(8):
        if destination.exists() or destination.is_symlink():
            raise FileExistsError('Existing cache snapshot was retained')
        try:
            stage.rename(destination)
            return destination
        except OSError as error:
            if (getattr(error, 'winerror', None) not in (5, 32, 33)
                    or attempt == 7 or destination.exists()):
                raise
            time.sleep(min(0.025 * 2**attempt, 0.25))
