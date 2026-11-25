using System.Buffers.Binary;
using System.Numerics;

namespace Mapper.Core;

/// <summary>Top-down terrain overview. Does not invent roads, prefabs or other absent world data.</summary>
public static class CompositeMapRenderer
{
    public static LayerPreview Render(byte[] heights, byte[] splat, byte[] biomes, int worldSize, CancellationToken token = default)
    {
        if (worldSize <= 0) throw new ArgumentOutOfRangeException(nameof(worldSize));
        var hr = Resolution(heights, 2); var sr = Resolution(splat, 8);
        var channels = biomes.Length % 5 == 0 && Square(biomes.Length / 5) ? 5 : 4;
        var br = Resolution(biomes, channels);
        var output = Math.Min(hr, 1024); var pixels = new byte[output * output * 4];
        Vector3[] groundColors = [new(120,108,78), new(224,233,229), new(180,169,125), new(132,135,121), new(114,129,79), new(72,98,65), new(148,147,127), new(132,122,96)];
        var light = Vector3.Normalize(new Vector3(-.5f, .8f, .3f));
        float Height(int x, int z) => RustTerrainEncoding.DecodeWorldHeight(BinaryPrimitives.ReadInt16LittleEndian(heights.AsSpan((Math.Clamp(z, 0, hr - 1) * hr + Math.Clamp(x, 0, hr - 1)) * 2)));
        var spacing = worldSize / (float)(hr - 1);
        for (var y = 0; y < output; y++)
        {
            token.ThrowIfCancellationRequested();
            for (var x = 0; x < output; x++)
            {
                var nx = x / (float)(output - 1); var nz = 1 - y / (float)(output - 1);
                var hx = (int)(nx * (hr - 1)); var hz = (int)(nz * (hr - 1)); var elevation = Height(hx, hz);
                Vector3 color;
                if (elevation <= 0)
                {
                    var depth = Math.Clamp(-elevation / 20, 0, 1);
                    color = Vector3.Lerp(new(86,137,139), new(19,61,72), depth);
                    if (elevation > -.8f) color = Vector3.Lerp(new(187,205,186), color, Math.Clamp(-elevation / .8f, 0, 1));
                }
                else
                {
                    var si = (int)(nz * (sr - 1)) * sr + (int)(nx * (sr - 1)); var total = 0f; color = Vector3.Zero;
                    for (var c = 0; c < 8; c++) { var w = splat[c * sr * sr + si] / 255f; color += groundColors[c] * w; total += w; }
                    color = total > 0 ? color / total : groundColors[4];
                    var bi = (int)(nz * (br - 1)) * br + (int)(nx * (br - 1));
                    var arid = biomes[bi] / 255f; var arctic = biomes[3 * br * br + bi] / 255f;
                    color = Vector3.Lerp(color, color * new Vector3(1.1f, 1.01f, .84f), arid * .55f);
                    color = Vector3.Lerp(color, groundColors[1], arctic * .72f);
                    var normal = Vector3.Normalize(new Vector3(Height(hx - 1, hz) - Height(hx + 1, hz), 2 * spacing, Height(hx, hz - 1) - Height(hx, hz + 1)));
                    var illumination = Math.Clamp(.55f + .55f * Vector3.Dot(normal, light), .4f, 1.1f);
                    color *= illumination;
                }
                var i = (y * output + x) * 4;
                pixels[i] = (byte)Math.Clamp(color.Z, 0, 255); pixels[i + 1] = (byte)Math.Clamp(color.Y, 0, 255); pixels[i + 2] = (byte)Math.Clamp(color.X, 0, 255); pixels[i + 3] = 255;
            }
        }
        return new(output, pixels);
    }

    private static int Resolution(byte[] data, int stride)
    {
        if (data.Length % stride != 0 || !Square(data.Length / stride)) throw new InvalidDataException("Invalid composite layer dimensions.");
        return (int)Math.Sqrt(data.Length / stride);
    }
    private static bool Square(int count) { var r = (int)Math.Sqrt(count); return r > 1 && r * r == count; }
}
