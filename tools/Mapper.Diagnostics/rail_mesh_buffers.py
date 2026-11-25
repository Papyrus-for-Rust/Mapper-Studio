"""Validate exact rail template buffers. No simplification or collider cooking."""
import math


def geometry(vertices, normals, tangents, uv, triangles, center, extents):
    count = len(vertices)
    if not 3 <= count <= 65536 or not 1 <= len(triangles) <= 131072:
        raise ValueError('Rail template exceeds mesh bounds')
    if any(len(values) != count for values in (normals, tangents, uv)):
        raise ValueError('Rail template vertex streams differ in length')
    def vectors(values, width, maximum):
        if any(len(row) != width or any(type(v) not in (int, float)
               or not math.isfinite(v) or abs(v) > maximum for v in row) for row in values):
            raise ValueError('Invalid rail template vertex stream')
        return [list(row) for row in values]
    vertices = vectors(vertices, 3, 10000)
    normals = vectors(normals, 3, 2)
    tangents = vectors(tangents, 4, 2)
    uv = vectors(uv, 2, 10000)
    center = vectors([center], 3, 10000)[0]
    extents = vectors([extents], 3, 10000)[0]
    if extents[0] <= 0 or extents[2] <= 0 or extents[1] < 0:
        raise ValueError('Degenerate rail template bounds')
    if any(abs(p[i] - center[i]) > extents[i] + 0.0001 for p in vertices for i in range(3)):
        raise ValueError('Rail template vertex outside authored bounds')
    if any(len(t) != 3 or any(type(i) is not int or not 0 <= i < count for i in t) for t in triangles):
        raise ValueError('Invalid rail template triangle indices')
    return dict(vertices=vertices, normals=normals, tangents=tangents, uv=uv,
                triangles=[list(t) for t in triangles], center=center, extents=extents)
