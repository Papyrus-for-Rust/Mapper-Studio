"""Deterministic vertex-cluster LOD for untextured inspection, never map collision."""
import math


class PreviewMesh:
    """Bounded streaming inspection mesh; later pieces are never budget-skipped.

    Each source mesh is validated/simplified before merging. Compact between
    batches so a large monument cannot exhaust a raw aggregate budget and hide
    the remaining hierarchy. Final geometry is still an approximate LOD.
    """
    def __init__(self):
        self.vertices = []
        self.triangles = []
        self.simplified = False

    def add(self, vertices, triangles):
        points, faces = simplify(vertices, triangles)
        reduced = len(faces) < len(triangles)
        # At most 30k triangles/90k referenced vertices between compactions.
        # Compact unreferenced source vertices too, including small meshes.
        used = sorted({i for face in faces for i in face})
        remap = {old: new for new, old in enumerate(used)}
        points = [points[i] for i in used]
        faces = [[remap[i] for i in face] for face in faces]
        if len(self.triangles) + len(faces) > 30000:
            self.finish()
        base = len(self.vertices)
        self.vertices.extend(points)
        self.triangles.extend([[base + i for i in face] for face in faces])
        self.simplified |= reduced

    def finish(self):
        before = len(self.triangles)
        self.vertices, self.triangles = simplify(self.vertices, self.triangles)
        self.simplified |= len(self.triangles) < before
        return self.vertices, self.triangles


def simplify(vertices, triangles, budget=1500):
    # Validate before the small-mesh fast path too. Unity data is installation
    # input; malformed indices must never wrap through Python negative indexing.
    if type(budget) is not int or not 1 <= budget <= 30000:
        raise ValueError('Invalid model LOD budget')
    if len(vertices)>600000 or len(triangles)>200000: raise ValueError('Raw model exceeds LOD budget')
    if any(len(v) != 3 or any(type(x) not in (int, float) or not math.isfinite(x)
                              or abs(x) > 10000 for x in v) for v in vertices):
        raise ValueError('Invalid model vertices')
    vertex_count = len(vertices)
    for face in triangles:
        if len(face) != 3:
            raise ValueError('Invalid model triangle indices')
        a, b, c = face
        if (type(a) is not int or type(b) is not int or type(c) is not int
                or not (0 <= a < vertex_count and 0 <= b < vertex_count and 0 <= c < vertex_count)):
            raise ValueError('Invalid model triangle indices')
    if not vertices or not triangles: return [], []
    if len(triangles)<=budget: return vertices,triangles
    lower=[min(v[i] for v in vertices) for i in range(3)]
    span=max(max(v[i] for v in vertices)-lower[i] for i in range(3))
    if not math.isfinite(span) or span<=0: return [],[]
    x_min, y_min, z_min = lower
    for divisions in (256, 128, 64, 32, 16, 8, 4, 2, 1):
        step = span / divisions
        cells = {}
        mapping = []
        for x, y, z in vertices:
            cell = (math.floor((x - x_min) / step),
                    math.floor((y - y_min) / step),
                    math.floor((z - z_min) / step))
            mapping.append(cells.setdefault(cell, len(cells)))

        faces = []
        seen = set()
        for a, b, c in triangles:
            a, b, c = mapping[a], mapping[b], mapping[c]
            if a == b or b == c or a == c:
                continue
            key = tuple(sorted((a, b, c)))
            if key in seen:
                continue
            seen.add(key)
            faces.append([a, b, c])
            if len(faces) > budget:
                break
        else:
            # Only the accepted grid needs centroids. Keep source vertex order
            # when summing so the floating-point output stays reproducible.
            used = sorted({i for face in faces for i in face})
            remap = {old: new for new, old in enumerate(used)}
            sums = [[0., 0., 0., 0] for _ in used]
            for index, (x, y, z) in zip(mapping, vertices):
                output_index = remap.get(index)
                if output_index is not None:
                    total = sums[output_index]
                    total[0] += x
                    total[1] += y
                    total[2] += z
                    total[3] += 1
            points = [[x / count, y / count, z / count] for x, y, z, count in sums]
            return points, [[remap[i] for i in face] for face in faces]
    raise ValueError('Cannot bound model LOD')
