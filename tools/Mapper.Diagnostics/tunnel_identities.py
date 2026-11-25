"""Resolve tunnel module and neighbour identities through the installed manifest."""
from extract_prefab_identities import capture as prefab_ids
from resolve_prefab_textures import guid_paths


def resolve(fields, prefabs):
    identities = prefab_ids(fields)
    required = set()
    for prefab in prefabs.values():
        for cell in prefab['cells']:
            for reference in cell['avoid_neighbours']:
                guid = reference.get('guid')
                if (set(reference) != {'guid'} or not isinstance(guid, str) or len(guid) != 32
                        or any(char not in '0123456789abcdefABCDEF' for char in guid)):
                    raise ValueError('Unsupported tunnel neighbour GUID')
                required.add(guid)
    paths = guid_paths(fields, required)
    if set(paths) != required:
        raise ValueError('Tunnel neighbour GUID is absent from the installed manifest')
    selected = {}
    for path in prefabs:
        if path not in identities:
            raise ValueError('Tunnel prefab has no installed resource identity')
        selected[path] = identities[path]
    neighbours = {}
    for guid, path in sorted(paths.items()):
        if path not in identities:
            raise ValueError('Tunnel neighbour GUID does not resolve to a pooled prefab')
        neighbours[guid] = dict(path=path, prefab_id=identities[path])
    return dict(format='mapper-tunnel-identities-1', prefabs=selected, neighbours=neighbours)


def capture(root, prefabs):
    from lazy_unity_bundle import load
    from resolve_prefab_textures import container_index, unique_asset
    environment, stream = load(root / 'Bundles/shared/content.bundle')
    try:
        manifest = unique_asset(container_index(environment), 'assets/manifest.asset')
        if manifest is None:
            raise ValueError('Missing exact GameManifest for tunnel identities')
        return resolve(manifest.read_typetree(), prefabs)
    finally:
        stream.close()
