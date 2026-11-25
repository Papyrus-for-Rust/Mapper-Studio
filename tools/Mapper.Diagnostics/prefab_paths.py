"""Read exact TerrainPathConnect script identities and prefab-local connector poses."""
from __future__ import annotations
import math
from collections import OrderedDict
from prefab_geometry import IDENTITY, matrix, multiply, reference, MAX_NODES, MAX_DEPTH

MAX_SCRIPT_IDENTITIES = 4096


def read_path_object(objects, object_id):
    """Decode the object fields consumed by path and root-monument traversal."""
    obj = objects[object_id]
    kind = int(obj.type)
    if kind not in (1, 4, 224, 114):
        return kind, {}
    return kind, obj.read_typetree()


def component_readers(objects):
    """Readers scoped to one serialized asset and its script-reference namespace."""
    script_identities = OrderedDict()
    last_id, last_fields = None, None

    def read(object_id):
        nonlocal last_id, last_fields
        kind, fields = read_path_object(objects, object_id)
        last_id, last_fields = object_id, fields
        return kind, fields

    def script_type(object_id):
        fields = last_fields if last_id == object_id else read(object_id)[1]
        script = fields['m_Script']
        key = script['m_FileID'], script['m_PathID']
        if type(key[0]) is not int or key[0] < 0 or type(key[1]) is not int or key[1] == 0:
            raise ValueError('Invalid component script reference')
        if key not in script_identities:
            value = objects[object_id].read().m_Script.read_typetree()
            script_identities[key] = value['m_AssemblyName'], value['m_Namespace'], value['m_ClassName']
            if len(script_identities) > MAX_SCRIPT_IDENTITIES:
                script_identities.popitem(last=False)
        script_identities.move_to_end(key)
        return script_identities[key]

    return read, script_type


def inspect(root_id, read, script_type, *, preserve_order=False):
    """read(id)->(Unity class, fields); script_type(component id)->(assembly, namespace, class)."""
    result={"version":1,"space":"prefab-root-local-unity","complete":True,"connectors":[],"issues":[],"nodesVisited":0}
    def issue(oid,error):
        result["complete"]=False
        if len(result["issues"])<128:result["issues"].append({"objectId":oid,"reason":str(error)[:256]})
    def components(go):
        values=go["m_Component"]
        if not isinstance(values,list) or len(values)>256:raise ValueError("Excessive component count")
        return [reference(v["component"]) for v in values]
    seen=set()
    try:
        kind,go=read(root_id)
        if kind!=1:raise ValueError("Root is not a GameObject")
        roots=[oid for oid in components(go) if read(oid)[0] in (4,224)]
        if len(roots)!=1:raise ValueError("Expected exactly one prefab root Transform")
        root_fields=read(roots[0])[1]
        if any(abs(root_fields["m_LocalScale"][k]-1)>1e-6 for k in 'xyz'):
            raise ValueError("Nonunit root scale requires an instance-scale adapter")
        stack=[(roots[0],IDENTITY,True,0)]
        while stack:
            oid,parent,relative_active,depth=stack.pop()
            if oid in seen or len(seen)>=MAX_NODES or depth>MAX_DEPTH:raise ValueError("Cyclic or excessive hierarchy")
            seen.add(oid)
            kind,t=read(oid)
            if kind not in (4,224):raise ValueError("Child is not a Transform")
            gid=reference(t["m_GameObject"]);kind,go=read(gid)
            if kind!=1:raise ValueError("Transform owner is not a GameObject")
            ids=components(go)
            if oid not in ids:raise ValueError("Transform ownership mismatch")
            active=go["m_IsActive"]
            if type(active) not in (int,bool) or active not in (0,1):raise ValueError("Unknown hierarchy active state")
            active=relative_active and (depth==0 or bool(active))
            world=multiply(parent,IDENTITY if depth==0 else matrix(t))
            result["nodesVisited"]+=1
            for cid in ids:
                if cid == oid:
                    continue
                kind,fields=read(cid)
                if kind!=114:continue
                try:
                    identity=script_type(cid)
                    if identity!=("Assembly-CSharp","","TerrainPathConnect"):continue
                    if reference(fields["m_GameObject"])!=gid:raise ValueError("Connector ownership mismatch")
                    if type(fields.get("Type")) is not int:raise ValueError("Missing infrastructure enum")
                    if len(result["connectors"])>=256:raise ValueError("Excessive path connectors")
                    forward=[world[i][2] for i in range(3)]
                    length=math.sqrt(sum(v*v for v in forward))
                    if length<1e-8:raise ValueError("Degenerate connector forward axis")
                    result["connectors"].append({"objectId":cid,"infrastructureType":fields["Type"],
                        "position":[world[i][3] for i in range(3)],"forward":[v/length for v in forward],
                        "hierarchyActiveWithoutRoot":active})
                except (ValueError,KeyError,TypeError,OverflowError,FileNotFoundError) as error:issue(cid,error)
            children=t["m_Children"]
            if not isinstance(children,list) or len(children)>MAX_NODES:raise ValueError("Excessive transform children")
            for child in reversed(children):stack.append((reference(child),world,active,depth+1))
    except (ValueError,KeyError,TypeError,OverflowError) as error:issue(root_id,error)
    if not preserve_order:
        result["connectors"].sort(key=lambda c:c["objectId"])
    return result


def inspect_ordered(root_id, read, script_type):
    """Separate version for source consumers where the first sockets set an axis.

    Existing infrastructure metadata keeps its sorted version-1 representation.
    Unity GetComponentsInChildren order is root-first depth-first hierarchy order,
    retaining component order on each GameObject and including inactive children.
    """
    result = inspect(root_id, read, script_type, preserve_order=True)
    result['version'] = 2
    result['connectorOrder'] = 'root-first-depth-first-component-order'
    return result
