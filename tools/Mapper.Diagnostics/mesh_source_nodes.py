"""Open the exact serialized node named by a mesh reference."""
from lazy_unity_bundle import load


def load_mesh_node(path, name):
    # A missing node in an earlier bundle is normal when searching installed sources.
    try:
        return load(path, nodes={name}, follow_dependencies=True)
    except ValueError as error:
        if str(error) != f"Requested UnityFS nodes are missing: {name}":
            raise
        return None, None
