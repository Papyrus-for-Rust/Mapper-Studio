"""Small live progress protocol for Mapper's isolated metadata workers."""
import json
import time
_last = 0.0

def report(stage, item, completed=0, total=None, unit="items", force=False):
    global _last
    now = time.monotonic()
    if not force and now - _last < 0.15:
        return
    _last = now
    print("MAPPER_PROGRESS " + json.dumps({"schema": 1, "stage": str(stage)[:180],
          "item": str(item)[:180], "completed": completed, "total": total, "unit": unit}), flush=True)
