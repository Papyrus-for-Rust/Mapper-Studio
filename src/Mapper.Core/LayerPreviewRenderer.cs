using System.Buffers.Binary;
namespace Mapper.Core;
public sealed record LayerPreview(int Size, byte[] Bgra);
public static class LayerPreviewRenderer
{
    public static LayerPreview Render(byte[] raw, string layer, CancellationToken token = default)
    {
        var channels = layer switch { "biomemap" => IsSquare(raw.Length / 5) && raw.Length % 5 == 0 ? 5 : 4, "splatmap" => 8, _ => 1 };
        var bytes = layer is "heightmap" or "watermap" or "terrainmap" ? 2 : layer == "topologymap" ? 4 : 1;
        var count = channels * bytes;
        if (raw.Length % count != 0 || !IsSquare(raw.Length / count)) throw new InvalidDataException("Layer dimensions are not square.");
        var resolution = (int)Math.Sqrt(raw.Length / count); var size = Math.Min(1024, resolution); var pixels = new byte[size * size * 4];
        (byte R, byte G, byte B)[] palette = layer == "biomemap" ? [(210,174,80), (74,145,74), (143,126,94), (225,239,246), (28,94,51)] : [(113,86,61), (225,239,246), (213,190,127), (110,112,115), (78,142,69), (39,90,48), (132,135,139), (149,136,116)];
        for (var y = 0; y < size; y++)
        {
            token.ThrowIfCancellationRequested();
            for (var x = 0; x < size; x++)
            {
                // Rust rows increase toward world +Z (north). Screen rows increase south.
                var sx = x * (resolution - 1) / Math.Max(1, size - 1); var sy = (size - 1 - y) * (resolution - 1) / Math.Max(1, size - 1);
                var source = sy * resolution + sx; var target = (y * size + x) * 4;
                (byte R, byte G, byte B) color;
                if (bytes == 2)
                {
                    var h = BinaryPrimitives.ReadInt16LittleEndian(raw.AsSpan(source * 2));
                    var left = BinaryPrimitives.ReadInt16LittleEndian(raw.AsSpan((sy * resolution + Math.Max(0, sx - 1)) * 2));
                    var shade = Math.Clamp(135 + (h - left) * 2, 45, 230);
                    color = h <= RustTerrainEncoding.SeaLevel ? ((byte)25, (byte)72, (byte)110) : ((byte)(shade * .72), (byte)(shade * .93), (byte)(shade * .55));
                }
                else if (bytes == 4)
                {
                    var flags = BinaryPrimitives.ReadUInt32LittleEndian(raw.AsSpan(source * 4));
                    color = TopologyColor(flags);
                }
                else if (channels == 1) color = (raw[source], raw[source], raw[source]);
                else { var best = 0; for (var c = 1; c < channels; c++) if (raw[c * resolution * resolution + source] > raw[best * resolution * resolution + source]) best = c; color = palette[best]; }
                pixels[target] = color.B; pixels[target + 1] = color.G; pixels[target + 2] = color.R; pixels[target + 3] = 255;
            }
        }
        return new(size, pixels);
    }
    private static (byte R, byte G, byte B) TopologyColor(uint flags)
    {
        // A topology cell commonly contains several masks. Pick the most useful one rather than
        // the lowest bit: Field is present through most dry land and otherwise hides all detail.
        if ((flags & (1u << 10)) != 0) return (244, 143, 44); // Monument
        if ((flags & (1u << 21)) != 0) return (215, 92, 50); // Building
        if ((flags & (1u << 14)) != 0) return (48, 184, 207); // River
        if ((flags & (1u << 16)) != 0) return (58, 125, 193); // Lake
        if ((flags & 128) != 0) return (25, 72, 110); // Ocean
        if ((flags & (1u << 11)) != 0) return (170, 95, 57); // Road
        if ((flags & 2) != 0) return (116, 120, 122); // Cliff
        if ((flags & (1u << 23)) != 0) return (126, 103, 145); // Mountain
        if ((flags & 32) != 0) return (48, 112, 65); // Forest
        if ((flags & 16) != 0) return (216, 190, 125); // Beach
        if ((flags & 8) != 0) return (168, 179, 112); // Beachside
        return (116, 142, 88); // Field/mainland
    }
    private static bool IsSquare(int count) { var r = (int)Math.Sqrt(count); return r > 1 && r * r == count; }
}
