"""Local prefab matrices for byte-equivalent metadata captures."""
import math


def matrix(fields):
    """Build the writer's local matrix without per-element generators."""
    px, py, pz = _xyz(fields["m_LocalPosition"])
    sx, sy, sz = _xyz(fields["m_LocalScale"])
    q = fields["m_LocalRotation"]
    x, y, z, w = float(q["x"]), float(q["y"]), float(q["z"]), float(q["w"])
    if not all(-1_000_000 <= value <= 1_000_000 for value in (x, y, z, w)):
        raise ValueError("Nonfinite or excessive transform")
    # CPython 3.12 sum uses compensated float summation. Keep the original
    # products and addition order so cached geometry stays byte-identical.
    norm = math.sqrt(sum((x*x, y*y, z*z, w*w)))
    if abs(norm - 1.) > 0.001:
        raise ValueError("Invalid transform quaternion")
    x, y, z, w = x/norm, y/norm, z/norm, w/norm
    return (
        ((1 - 2*(y*y + z*z))*sx, (2*(x*y - z*w))*sy, (2*(x*z + y*w))*sz, px),
        ((2*(x*y + z*w))*sx, (1 - 2*(x*x + z*z))*sy, (2*(y*z - x*w))*sz, py),
        ((2*(x*z - y*w))*sx, (2*(y*z + x*w))*sy, (1 - 2*(x*x + y*y))*sz, pz),
        (0., 0., 0., 1.),
    )


def _xyz(value):
    x, y, z = float(value["x"]), float(value["y"]), float(value["z"])
    if not (-1_000_000 <= x <= 1_000_000
            and -1_000_000 <= y <= 1_000_000
            and -1_000_000 <= z <= 1_000_000):
        raise ValueError("Nonfinite or excessive transform")
    return x, y, z
