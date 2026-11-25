"""Capture source-bound offshore selectors, quotas and distance bands.

No game code is executed. A capture does not validate placement, environment
checks, transforms, meshes, or complete world generation.
"""
import argparse
import hashlib
import json
from pathlib import Path
from extract_verified_prefabs import snapshot, fingerprint
from monument_stages import collect
from offshore_stages import stage_data
from extract_monument_stages import bounded, capture_sources, stage_layout
from offshore_prefabs import capture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--code-profile', type=Path)
    args = parser.parse_args()
    profile_raw = bounded(args.code_profile, 1024**2) if args.code_profile else None
    profile = json.loads(profile_raw) if profile_raw is not None else None
    layout = stage_layout(profile) if profile is not None else 'legacy'
    if profile is not None and layout != 'hierarchy':
        raise ValueError('Offshore code profile requires hierarchy activation')
    def sources():
        if profile is not None:
            return capture_sources(args.rust_root, profile)
        return snapshot(args.rust_root) + [fingerprint(args.rust_root, 'Bundles/maps/maps.bundle', 512*1024**2)]
    names = ('extract_offshore_stages.py', 'offshore_stages.py', 'monument_stages.py',
             'extract_verified_prefabs.py', 'lazy_unity_bundle.py',
             'extract_monument_stages.py', 'prefab_geometry.py', 'mapper_paths.py',
             'offshore_prefabs.py', 'monument_placement_attributes.py', 'prefab_paths.py')
    def script_hash():
        return hashlib.sha256(b''.join(n.encode()+b'\0'+Path(__file__).with_name(n).read_bytes() for n in names)).hexdigest()
    before, extractor = sources(), script_hash()
    programs = []
    collect(args.rust_root, programs, inventory_only=True, capture_fields=True)
    stages = [stage_data(p, layout=layout) for p in programs if p['identity'][2] == 'PlaceMonumentsOffshore']
    if not 1 <= len(stages) <= 64:
        raise ValueError('Unsupported offshore program roster')
    prefabs = capture(args.rust_root, stages)
    if before != sources() or extractor != script_hash():
        raise ValueError('Offshore source changed during capture')
    if args.code_profile and bounded(args.code_profile, 1024**2) != profile_raw:
        raise ValueError('Offshore code profile changed during capture')
    document = dict(format='papyrus-offshore-stage-capture', schema=1,
                    sources=before, extractor_sha256=extractor, stages=stages,
                    full_stage_execution_validated=False, prefabs=prefabs)
    if profile is not None:
        document.update(schema=2, activation=profile['activation'],
                        code_profile_sha256=hashlib.sha256(profile_raw).hexdigest())
    raw = json.dumps(document, indent=2, sort_keys=True, allow_nan=False)
    if len(raw.encode()) > 1024**2:
        raise ValueError('Offshore stage capture exceeds one MiB')
    with args.output.open('x', encoding='utf-8') as stream:
        stream.write(raw)
    print(f'Captured {len(stages)} offshore stage(s): {args.output}')


if __name__ == '__main__':
    main()
