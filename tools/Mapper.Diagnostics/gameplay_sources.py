"""Shared bounded verification of installation gameplay-code receipts."""
from extract_verified_prefabs import fingerprint


def verify_code(root, profile, *, label="Decoration"):
    receipts = profile.get('code_sources', [])
    allowed = {'GameAssembly.dll',
               'RustClient_Data/il2cpp_data/Metadata/global-metadata.dat',
               'RustClient_Data/Managed/Assembly-CSharp.dll',
               'RustDedicated_Data/Managed/Assembly-CSharp.dll'}
    if not isinstance(receipts, list) or len(receipts) > 2:
        raise ValueError(f"Invalid {label.lower()} gameplay receipt roster")
    seen = set()
    for receipt in receipts:
        if not isinstance(receipt, dict) or set(receipt) != {'relative_path', 'sha256', 'bytes'}:
            raise ValueError(f"Invalid {label.lower()} gameplay receipt fields")
        path, sha, size = receipt['relative_path'], receipt['sha256'], receipt['bytes']
        if (not isinstance(path, str) or path not in allowed or path in seen
                or type(size) is not int or not 0 < size <= 1024**3
                or not isinstance(sha, str) or len(sha) != 64
                or any(c not in '0123456789abcdef' for c in sha)):
            raise ValueError(f"Invalid {label.lower()} gameplay receipt path, digest or size")
        seen.add(path)
    for receipt in receipts:
        if fingerprint(root, receipt['relative_path'], 1024**3) != receipt:
            raise ValueError(f"{label} gameplay code differs from selected installation")
    return receipts
