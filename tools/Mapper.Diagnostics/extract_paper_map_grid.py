"""Capture only the local PaperMap label atlas and daytime color settings."""
import argparse
import hashlib
import json
from pathlib import Path

from UnityPy.classes.PPtr import PPtr
from PIL import Image
from texture2ddecoder import decode_bc1
from metadata_object_reader import load_selected_objects


def grid_mips(texture):
    settings = texture.m_TextureSettings
    if (texture.m_Name, texture.m_Width, texture.m_Height, texture.m_MipCount,
            texture.m_TextureFormat, settings.m_FilterMode, settings.m_WrapU,
            settings.m_WrapV) != ("MapGrid", 512, 512, 10, 10, 1, 0, 0):
        raise ValueError("Unsupported PaperMap grid texture")
    data = texture.get_image_data()
    offset = 0
    patches = []
    for level in range(10):
        side = 512 >> level
        size = max(1, side // 4) ** 2 * 8
        red = Image.frombytes("RGBA", (side, side),
                             decode_bc1(data[offset:offset + size], side, side),
                             "raw", "BGRA").getchannel("R").tobytes()
        offset += size
        if side <= 9:
            patches.append(list(red))
            continue
        # The middle rows and columns repeat. Store their values once and
        # verify the compact representation against every decoded texel.
        indices = [0, 1, 2, 3, 4, side - 4, side - 3, side - 2, side - 1]
        rows = [bytes(red[y * side + x] for x in indices) for y in indices]
        expanded = [row[:4] + row[4:5] * (side - 8) + row[5:] for row in rows]
        if red != b"".join(expanded[:4] + [expanded[4]] * (side - 8) + expanded[5:]):
            raise ValueError("Unsupported PaperMap grid mip pattern")
        patches.append(list(b"".join(rows)))
    return patches


def extract(root, output):
    bundle = root / "Bundles/shared/content.bundle"
    # This reader only needs named materials and local Texture2D references.
    environment, stream = load_selected_objects(bundle, (21, 28))
    try:
        materials = [
            obj for asset in environment.assets for obj in asset.objects.values()
            if int(obj.type) == 21 and obj.peek_name() == "PaperMap"
        ]
        if len(materials) != 1:
            raise ValueError("Missing or ambiguous PaperMap material")
        material = materials[0]
        properties = material.read_typetree()["m_SavedProperties"]
        textures = dict(properties["m_TexEnvs"])
        color = dict(properties["m_Colors"])["_GridColor"]
        floats = dict(properties["m_Floats"])
        reference = textures["_GridFont"]["m_Texture"]
        if reference["m_FileID"] != 0:
            raise ValueError("PaperMap font requires a different content bundle")
        pointer = PPtr(**reference, assetsfile=material.assets_file)
        glyphs = pointer.deref().read()
        if glyphs.m_Name != "GridNumbers" or (glyphs.m_Width, glyphs.m_Height) != (4096, 128):
            raise ValueError("Unsupported PaperMap font atlas")
        alpha = glyphs.image.getchannel("A").tobytes()
        if len(alpha) != 4096 * 128 or not any(alpha):
            raise ValueError("Invalid PaperMap font alpha")
        grid_reference = textures["_GridTexture"]["m_Texture"]
        mips = None
        if grid_reference["m_FileID"] == 0:
            try:
                mips = grid_mips(PPtr(**grid_reference, assetsfile=material.assets_file).deref().read())
            except ValueError:
                pass  # Older texture encodings can still use the font and material.
        receipt = {
            "schema": 1, "alpha_sha256": hashlib.sha256(alpha).hexdigest(),
            "color": [color[c] for c in "rgb"],
            "line_opacity": floats["_GridOpacity"],
            "number_offset": floats["_NumberOffset"],
            "number_opacity": floats["_NumberOpacity"],
            "number_size": floats["_NumberSize"],
            "cells_per_1024": floats["_LineSpread"],
            "zoom": floats.get("_Zoom"),
        }
        if mips is not None:
            receipt["line_mips"] = mips
        output.mkdir()
        (output / "grid.alpha").write_bytes(alpha)
        (output / "grid.json").write_text(json.dumps(receipt, allow_nan=False), encoding="utf8")
    finally:
        stream.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="Rust client installation")
    parser.add_argument("output", type=Path, help="New directory for the grid capture")
    args = parser.parse_args()
    extract(args.root, args.output)
