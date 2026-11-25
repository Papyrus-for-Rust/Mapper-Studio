"""Bounded numeric inspection of prefab-local water components. Never reads mesh buffers."""
from __future__ import annotations
import math

VERSION = 1
MAX_NODES = 65536
MAX_DEPTH = 128
MAX_RECORDS = 1024
MAX_OBJECTS = 262144
COLLIDERS = {64: "mesh", 65: "box", 135: "sphere", 136: "capsule", 154: "terrain"}
IDENTITY = ((1., 0., 0., 0.), (0., 1., 0., 0.), (0., 0., 1., 0.), (0., 0., 0., 1.))


def reference(value):
    if not isinstance(value, dict) or value.get("m_FileID") != 0 or type(value.get("m_PathID")) is not int or value["m_PathID"] == 0:
        raise ValueError("Missing or external object reference")
    return value["m_PathID"]


def vector(value, names="xyz"):
    result = [float(value[k]) for k in names]
    if any(not math.isfinite(v) or abs(v) > 1_000_000 for v in result):
        raise ValueError("Nonfinite or excessive transform")
    return result


def multiply(a, b):
    result = tuple(
        tuple(sum((row[0]*b[0][column], row[1]*b[1][column],
                   row[2]*b[2][column], row[3]*b[3][column]))
              for column in range(4))
        for row in a
    )
    if any(not math.isfinite(v) or abs(v) > 1_000_000 for row in result for v in row):
        raise ValueError("Hierarchy transform exceeds inspection bounds")
    return result


def matrix(fields):
    p = vector(fields["m_LocalPosition"])
    s = vector(fields["m_LocalScale"])
    q = vector(fields["m_LocalRotation"], "xyzw")
    norm = math.sqrt(sum(v*v for v in q))
    if abs(norm-1.) > 0.001:
        raise ValueError("Invalid transform quaternion")
    x, y, z, w = [v/norm for v in q]
    rotation = ((1-2*(y*y+z*z), 2*(x*y-z*w), 2*(x*z+y*w)),
                (2*(x*y+z*w), 1-2*(x*x+z*z), 2*(y*z-x*w)),
                (2*(x*z-y*w), 2*(y*z+x*w), 1-2*(x*x+y*y)))
    return tuple(tuple(rotation[i][j]*s[j] for j in range(3))+(p[i],) for i in range(3))+((0.,0.,0.,1.),)


def point(m, v):
    return [sum(m[i][j]*v[j] for j in range(3))+m[i][3] for i in range(3)]


def box(m, fields):
    size = vector(fields["m_Size"])
    if any(v <= 0 for v in size):
        raise ValueError("Invalid box dimensions")
    center = point(m, vector(fields["m_Center"]))
    axes = [[m[i][j]*size[j]/2 for i in range(3)] for j in range(3)]
    if any(not math.isfinite(v) or abs(v) > 1_000_000 for v in center+[v for axis in axes for v in axis]):
        raise ValueError("Excessive collider bounds")
    lengths = [math.sqrt(sum(v*v for v in a)) for a in axes]
    if any(v < 1e-8 for v in lengths):
        raise ValueError("Degenerate collider axes")
    # Preserve exact half-axis vectors, even if the hierarchy produces tilt or shear.
    vertical = abs(axes[1][0])+abs(axes[1][2]) <= lengths[1]*1e-6
    horizontal = abs(axes[0][1])+abs(axes[2][1]) <= max(lengths[0],lengths[2])*1e-6
    orthogonal = abs(sum(a*b for a,b in zip(axes[0],axes[2]))) <= lengths[0]*lengths[2]*1e-6
    return {"shape": "box", "center": center, "halfAxes": axes,
            "flatOrthogonal": vertical and horizontal and orthogonal}


def inspect(root_id, read, mesh_callback=None, *, class_id=None):
    """read(id) returns (Unity class id, serialized fields); refs must stay in this prefab tree."""
    result = {"version": VERSION, "space": "prefab-root-local-unity", "nodesVisited": 0,
              "waterBodies": [], "contributors": [], "issues": [], "complete": True}
    cache = {}
    def get(oid):
        if oid not in cache:
            if len(cache) >= MAX_OBJECTS:
                raise ValueError("Object count exceeds inspection bounds")
            cache[oid] = read(oid)
        return cache[oid]
    def object_type(oid):
        return get(oid)[0] if class_id is None else class_id(oid)
    def issue(oid, message):
        result["complete"] = False
        if len(result["issues"]) < 256:
            result["issues"].append({"objectId": oid, "reason": str(message)[:256]})
    def components(go):
        values = go["m_Component"]
        if not isinstance(values,list) or len(values) > 256:
            raise ValueError("Component count exceeds inspection bounds")
        return [reference(v["component"]) for v in values]
    def flag(value):
        if type(value) not in (bool,int) or value not in (0,1):
            raise ValueError("Unknown serialized boolean state")
        return bool(value)
    def enabled(fields):
        return flag(fields["m_Enabled"])
    def body_type(value):
        if type(value) is not int or value not in (1,2,4,8,16,32):
            raise ValueError("Unknown water body type requires an adapter update")
        return value
    nodes = {}
    try:
        root_type, root = get(root_id)
        if root_type != 1:
            raise ValueError("Prefab root is not a GameObject")
        roots = [oid for oid in components(root) if object_type(oid) in (4,224)]
        if len(roots) != 1:
            raise ValueError("Prefab needs exactly one root Transform")
        root_transform = get(roots[0])[1]
        if any(abs(s-1.) > 1e-6 for s in vector(root_transform["m_LocalScale"])):
            raise ValueError("Non-unit prefab root scale needs an instance-scale adapter")
        stack = [(roots[0], IDENTITY, True, True, 0)]
        seen = set()
        while stack:
            transform_id, parent, parent_active, parent_relative, depth = stack.pop()
            if transform_id in seen:
                raise ValueError("Cyclic or shared prefab hierarchy")
            if len(seen) >= MAX_NODES or depth > MAX_DEPTH:
                raise ValueError("Prefab hierarchy exceeds node/depth budget")
            seen.add(transform_id)
            kind, transform = get(transform_id)
            if kind not in (4,224):
                raise ValueError("Hierarchy child is not a Transform")
            go_id = reference(transform["m_GameObject"])
            kind, go = get(go_id)
            if kind != 1 or go_id in nodes:
                raise ValueError("Invalid or shared GameObject")
            local = IDENTITY if depth == 0 else matrix(transform)
            world = multiply(parent,local)
            if go["m_IsActive"] not in (0,1,False,True):
                raise ValueError("Unknown GameObject active state")
            active = parent_active and bool(go["m_IsActive"])
            relative_active = parent_relative and (depth == 0 or bool(go["m_IsActive"]))
            nodes[go_id] = (world,active,relative_active,components(go))
            result["nodesVisited"] += 1
            children = transform["m_Children"]
            if not isinstance(children,list) or len(children) > MAX_NODES:
                raise ValueError("Excessive child transform list")
            for child in reversed(children):
                stack.append((reference(child),world,active,relative_active,depth+1))
    except (ValueError,KeyError,TypeError,OverflowError) as error:
        issue(root_id,error)
        return result

    def collider(oid):
        kind, fields = get(oid)
        if kind not in COLLIDERS:
            raise ValueError("Water reference is not a supported collider class")
        go_id = reference(fields["m_GameObject"])
        if go_id not in nodes:
            raise ValueError("Collider lies outside the prefab hierarchy")
        world, active, relative_active, ids = nodes[go_id]
        if oid not in ids:
            raise ValueError("Collider reference contradicts its GameObject")
        geometry = box(world,fields) if kind == 65 else {"shape":COLLIDERS[kind]}
        if kind == 64 and mesh_callback is not None:
            mesh_callback(oid, fields, world)
        return {"objectId":oid,"active":active and enabled(fields),"componentEnabled":enabled(fields),"hierarchyActive":active,"hierarchyActiveWithoutRoot":relative_active,"geometry":geometry}

    for go_id, (_, active, relative_active, ids) in nodes.items():
        scripts = []
        for oid in ids:
            try:
                if object_type(oid) == 114:
                    _, fields = get(oid)
                    enabled(fields)  # Validate state, including initially disabled components.
                    scripts.append((oid,fields))
            except (ValueError,KeyError,TypeError,OverflowError) as error:
                issue(oid,error)
        water = [(oid,f) for oid,f in scripts if {"Type","Triggers","IsOcean","MaterialBlend"}.issubset(f)]
        additions = [(oid,f) for oid,f in scripts if "automaticallyAddTerrainTopology" in f]
        if len(water) > 1 or len(additions) > 1:
            issue(go_id,"Ambiguous water components")
            continue
        for oid, fields in water:
            try:
                if len(result["waterBodies"]) >= MAX_RECORDS or not isinstance(fields["Triggers"],list) or len(fields["Triggers"]) > MAX_RECORDS:
                    raise ValueError("Water body count exceeds inspection bounds")
                triggers = [collider(reference(ref)) for ref in fields["Triggers"]]
                if len({c["objectId"] for c in triggers})!=len(triggers) or sum(len(b["triggers"]) for b in result["waterBodies"])+len(triggers)>4096:
                    raise ValueError("Duplicate or excessive water trigger references")
                result["waterBodies"].append({"objectId":oid,"active":active and enabled(fields),"componentEnabled":enabled(fields),"hierarchyActive":active,"hierarchyActiveWithoutRoot":relative_active,"bodyType":body_type(fields["Type"]),"isOcean":flag(fields["IsOcean"]),"triggers":triggers})
            except (ValueError,KeyError,TypeError,OverflowError) as error:
                issue(oid,error)
        for oid, fields in additions:
            try:
                if len(result["contributors"]) >= MAX_RECORDS:
                    raise ValueError("Water contributor count exceeds inspection bounds")
                candidates = [cid for cid in ids if object_type(cid) in COLLIDERS]
                if len(candidates) != 1:
                    raise ValueError("AddToWaterMap needs one unambiguous collider")
                shape = collider(candidates[0])
                result["contributors"].append({"objectId":oid,"active":active and enabled(fields),"componentEnabled":enabled(fields),"hierarchyActive":active,"hierarchyActiveWithoutRoot":relative_active,"bodyType":body_type(water[0][1]["Type"]) if water else None,
                    "addTopology":flag(fields["automaticallyAddTerrainTopology"]),"collider":shape})
            except (ValueError,KeyError,TypeError,OverflowError) as error:
                issue(oid,error)
    return result
