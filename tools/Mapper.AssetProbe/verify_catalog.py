"""Independent reference check for the native probe; not a runtime dependency."""
import hashlib
import json
import math
from pathlib import Path
import struct
import sys


def equal(a, b):
    if type(a) in (int, float) and type(b) in (int, float):
        if type(a) is int and type(b) is int:
            return a == b
        return (math.isfinite(a) and math.isfinite(b)
                and struct.pack('<f', a) == struct.pack('<f', b))
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(equal(x, y) for x, y in zip(a, b))
    if isinstance(a, dict) and isinstance(b, dict):
        return set(a) == set(b) and all(equal(a[k], b[k]) for k in a)
    return type(a) is type(b) and a == b


def exact_differences(a, b, path='metadata'):
    if isinstance(a, dict) and isinstance(b, dict):
        result = []
        for key in sorted(set(a) | set(b)):
            if key not in a or key not in b:
                result.append({'field': path+'/'+key, 'missing_key': True})
            else:
                result.extend(exact_differences(a[key], b[key], path+'/'+key))
        return result
    if isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            return [{'field': path, 'native_length': len(a), 'reference_length': len(b)}]
        return [entry for i, (x, y) in enumerate(zip(a, b))
                for entry in exact_differences(x, y, path+'/'+str(i))]
    # No geometry tolerance or float32 downcast: preserve reference f64 results.
    if type(a) is not type(b) or a != b:
        return [{'field': path, 'native': a, 'reference': b}]
    return []


def main():
    if len(sys.argv) != 4:
        raise ValueError('REFERENCE_CATALOG NATIVE_PROBE NEW_REPORT')
    raw = Path(sys.argv[1]).read_bytes()
    probe_raw = Path(sys.argv[2]).read_bytes()
    if max(len(raw), len(probe_raw)) > 16 * 1024**2:
        raise ValueError('Input exceeds verification budget')
    catalog, probe = json.loads(raw), json.loads(probe_raw)
    if probe['catalog_sha256'] != hashlib.sha256(raw).hexdigest():
        raise ValueError('Native probe used a different reference')
    differences = list(probe['issues'])
    differences += [{'missing': path} for path in probe['missing']]
    checked, fields_checked = set(), 0
    modifier_keys = ('size', 'offset', 'heightMap', 'waterMap', 'alphaMap', 'splatMask',
                     'biomeMask', 'topologyMask', 'radius', 'fade', 'autoCliffSplat',
                     'autoCliffTopology', 'removeExistingTopology', 'textureGuids')
    for group in ('modifiers', 'special'):
        for path, reference in catalog['metadata'][group].items():
            if 'assetScene' not in reference:
                continue
            checked.add(path)
            row = probe['roots'].get(path)
            if row is None:
                differences.append({'missing_reference_root': path})
                continue
            expected = {'bounds': reference.get('monumentBounds'),
                        'placement': reference.get('placement'), 'rotation': reference.get('rotation')}
            for field, value in expected.items():
                fields_checked += 1
                if not equal(row.get(field), value):
                    differences.append({'path': path, 'field': field, 'native': row.get(field), 'reference': value})
            if group == 'modifiers':
                for field in modifier_keys:
                    fields_checked += 1
                    value = row['modifier'].get(field)
                    if not equal(value, reference[field]):
                        differences.append({'path': path, 'field': field, 'native': value, 'reference': reference[field]})
    differences += [{'unexpected_root': path} for path in sorted(set(probe['roots']) - checked)]
    complete_metadata = 'catalog_metadata' in probe
    metadata_differences = (exact_differences(probe['catalog_metadata'], catalog['metadata'])
                            if complete_metadata else [])
    report = {'format': 'mapper-native-root-catalog-parity-1',
              'catalog_sha256': hashlib.sha256(raw).hexdigest(),
              'probe_sha256': hashlib.sha256(probe_raw).hexdigest(),
              'roots_checked': len(checked), 'field_groups_checked': fields_checked,
              'float_comparison': 'source f32 bits; integer masks compared exactly',
              'differences': differences, 'selected_root_metadata_matches': not differences,
              'complete_catalog_metadata_compared': complete_metadata,
              'complete_catalog_metadata_matches': complete_metadata and not metadata_differences,
              'complete_metadata_differences': metadata_differences,
              'scope': 'Root bounds, min size, priority/count, rotations, modifier settings and texture GUIDs. Excludes nested water geometry, deprecated texture object IDs, rules, pixels, models and full extraction parity.'}
    with Path(sys.argv[3]).open('x') as stream:
        json.dump(report, stream, indent=2)
    print(f"Checked {len(checked)} roots and {fields_checked} field groups; {len(differences)} root differences; {len(metadata_differences)} complete metadata differences")
    if differences or metadata_differences:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
