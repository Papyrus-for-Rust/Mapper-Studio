"""Strict source scene contracts for road-attached object stages."""
import math
from road_stages import activation, BASE, CURRENT_BASE

GROUPS=('Start','End','Side','Path')

def guid_reference(value):
    if isinstance(value,dict) and set(value)=={'guid'}:
        guid=value['guid']
        if isinstance(guid,str) and len(guid)==32 and all(c in '0123456789abcdef' for c in guid):return guid
    return None

def filter_data(fields):
    names={'SplatType','BiomeType','TopologyAny','TopologyAll','TopologyNot'}
    if not isinstance(fields,dict) or set(fields)!=names or any(type(fields[k]) is not int or not -2**31<=fields[k]<2**31 for k in names) or any(fields[k]==0 for k in ('SplatType','BiomeType','TopologyAny')):
        raise ValueError('Unknown road object spawn filter')
    return fields

def boolean(value):
    if type(value) not in (int,bool) or value not in (0,1):raise ValueError('Invalid road object activation')
    return bool(value)

def number(value,low,high):
    if type(value) not in (float,int) or not math.isfinite(value) or not low<=value<=high:
        raise ValueError('Invalid road object numeric setting')
    return value

def placement_policy_data(p):
    keys={'engine','maximum_hierarchy_exclusive','sample_step_fraction','endpoint_inset_fraction','trim_fraction','endpoint_offsets','anchor_mode','check_anchor_filter','seed_domain'}
    if not isinstance(p,dict) or set(p)!=keys or p['engine']!='papyrus-road-object-fit-1':raise ValueError('Unsupported road object placement policy')
    if type(p['maximum_hierarchy_exclusive']) is not int or not 1<=p['maximum_hierarchy_exclusive']<=16 or type(p['anchor_mode']) is not int or p['anchor_mode'] not in range(3) or type(p['check_anchor_filter']) is not bool:raise ValueError('Invalid road fitting modes')
    number(p['sample_step_fraction'],.01,1);number(p['endpoint_inset_fraction'],0,1);number(p['trim_fraction'],0,.5)
    offsets=p['endpoint_offsets']
    if not isinstance(offsets,list) or len(offsets)!=3:raise ValueError('Invalid endpoint placement order')
    for v in offsets:number(v,-4,4)
    if offsets[0]!=0 or offsets[1]==0 or offsets[2]==0 or offsets[1]==offsets[2]:raise ValueError('Invalid endpoint placement offsets')
    domain=p['seed_domain']
    if not isinstance(domain,str) or not 1<=len(domain)<=128 or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-' for c in domain):raise ValueError('Invalid road object seed domain')
    return p

def rule_data(group,fields):
    common={'Folder','Filter'}
    extra=({'Placement','AlignToNormal','HeightToTerrain','Offset'} if group in ('Start','End') else
           {'Alignment','Density','Distance','Offset','Side'} if group=='Side' else
           {'Alignment','Density','Distance','Dithering'} if group=='Path' else set())
    if not extra or not isinstance(fields,dict) or set(fields)!=common|extra:raise ValueError('Unknown road object rule layout')
    folder=fields['Folder']
    if not isinstance(folder,str) or len(folder)>256 or folder!=folder.strip() or folder!=folder.lower() or '\\' in folder or '\0' in folder or (folder and any(p in ('','.','..') for p in folder.split('/'))):
        raise ValueError('Invalid road object folder')
    out=dict(folder=folder,filter=filter_data(fields['Filter']),prefabs=[])
    if group in ('Start','End'):
        if type(fields['Placement']) is not int or fields['Placement'] not in (0,1):raise ValueError('Unknown road endpoint placement')
        out.update(placement=fields['Placement'],align_to_normal=boolean(fields['AlignToNormal']),height_to_terrain=boolean(fields['HeightToTerrain']),offset=number(fields['Offset'],-128,128))
    else:
        if type(fields['Alignment']) is not int or fields['Alignment'] not in range(4):raise ValueError('Unknown road object alignment')
        out.update(alignment=fields['Alignment'],density=number(fields['Density'],0,1),distance=number(fields['Distance'],.1,1000))
        if group=='Side':
            if type(fields['Side']) is not int or fields['Side'] not in range(4):raise ValueError('Unknown road object side')
            out.update(side=fields['Side'],offset=number(fields['Offset'],-128,128))
        else:out['dithering']=number(fields['Dithering'],0,128)
    return out

def stage_data(identity,fields,object_id,order,active,*,stage_layout='legacy'):
    if identity!=('Assembly-CSharp','','PlaceRoadObjects'):raise ValueError('Unknown road object stage')
    flags=activation(fields,set(GROUPS),stage_layout)
    if not isinstance(fields.get('Description'),str) or any(group not in fields for group in GROUPS):
        raise ValueError('Missing road object description or rule group')
    out=dict(object_id=object_id,order=order,name=fields['Description'],**flags,hierarchy_active=boolean(active))
    for group in GROUPS:
        rows=fields[group]
        if not isinstance(rows,list) or len(rows)>128:raise ValueError('Excessive road object rules')
        out[group.lower()]=[rule_data(group,r) for r in rows]
    return out

def workflow_data(identity,fields,object_id,order,active,*,stage_layout='legacy'):
    if stage_layout not in ('legacy','mode-description'):
        raise ValueError('Unsupported road object workflow layout')
    required={'Mode','Description','m_Enabled'}|({'Disabled'} if stage_layout=='legacy' else set())
    if not required.issubset(fields) or (stage_layout=='mode-description' and (BASE-CURRENT_BASE)&set(fields)):
        raise ValueError('Procedural workflow header differs from its source layout')
    if identity[:2]!=('Assembly-CSharp','') or not identity[2] or len(identity[2])>256:
        raise ValueError('Invalid procedural component identity')
    if type(fields['Mode']) is not int or not -2**31<=fields['Mode']<2**31:
        raise ValueError('Invalid procedural realm value')
    if not isinstance(fields['Description'],str) or len(fields['Description'])>1024 or '\0' in fields['Description']:
        raise ValueError('Invalid procedural description')
    if type(object_id) is not int or object_id==0 or type(order) is not int or order<0:
        raise ValueError('Invalid procedural stage index')
    return dict(object_id=object_id,order=order,kind=identity[2],description=fields['Description'],mode=fields['Mode'],disabled=boolean(fields['Disabled']) if stage_layout=='legacy' else None,component_enabled=boolean(fields['m_Enabled']),hierarchy_active=boolean(active))

def stages(root,include_workflow=False,*,stage_layout='legacy'):
    if stage_layout not in ('legacy','mode-description'):
        raise ValueError('Unsupported road object scene layout')
    from UnityPy.classes.PPtr import PPtr
    from lazy_unity_bundle import load
    env,stream=load(root/'Bundles/maps/maps.bundle')
    try:
        scenes=[a for a in env.assets if a.name=='BuildPlayer-Procedural Map']
        if len(scenes)!=1:raise ValueError('Missing exact procedural map scene')
        scene=scenes[0]
        def identity(obj):
            s=obj.read().m_Script.read_typetree()
            return s['m_AssemblyName'],s['m_Namespace'],s['m_ClassName']
        def ptr(ref):
            if ref['m_FileID']!=0:raise ValueError('External procedural hierarchy')
            return PPtr(**ref,assetsfile=scene).deref()
        setups=[o for o in scene.objects.values() if int(o.type)==114 and identity(o)==('Assembly-CSharp','','WorldSetup')]
        if len(setups)!=1:raise ValueError('Ambiguous WorldSetup')
        rows=[];workflow=[];seen=set()
        def walk(go,active=True,depth=0):
            if depth>128 or go.path_id in seen or len(seen)>=65536:raise ValueError('Invalid procedural hierarchy')
            seen.add(go.path_id);f=go.read_typetree();active=active and boolean(f['m_IsActive'])
            records=[ptr(c['component']) for c in f['m_Component']]
            for obj in records:
                if int(obj.type)!=114:continue
                ident=identity(obj)
                if ident[:2]!=('Assembly-CSharp',''):continue
                fields=obj.read_typetree();global_row=None
                header={'Mode','Description'}|({'Disabled'} if stage_layout=='legacy' else set())
                if header<=set(fields):
                    global_row=workflow_data(ident,fields,obj.path_id,len(workflow),active,stage_layout=stage_layout)
                    workflow.append(global_row)
                    if len(workflow)>512:raise ValueError('Excessive procedural workflow')
                if ident[2]=='PlaceRoadObjects':
                    if global_row is None:raise ValueError('Road object stage lacks procedural identity')
                    row=stage_data(ident,fields,obj.path_id,len(rows),active,stage_layout=stage_layout)
                    if include_workflow:row['scene_order']=global_row['order']
                    rows.append(row)
            transforms=[o.read_typetree() for o in records if int(o.type)==4]
            if len(transforms)!=1:raise ValueError('Ambiguous procedural transform')
            for child in transforms[0]['m_Children']:walk(ptr(ptr(child).read_typetree()['m_GameObject']),active,depth+1)
        walk(ptr(setups[0].read_typetree()['m_GameObject']))
        if not 1<=len(rows)<=32:raise ValueError('Unsupported road object stage roster')
        return (rows,dict(format='papyrus-procedural-order',schema=1 if stage_layout=='legacy' else 2,stages=workflow)) if include_workflow else rows
    finally:stream.close()
