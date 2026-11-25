namespace Mapper.Core;

/// <summary>Rust's signed-short normalized terrain coordinates, with world Y origin at -500 metres.</summary>
public static class RustTerrainEncoding
{
    public const float MinimumWorldY = -500;
    public const float VerticalRange = 1000;
    public const short NormalizedScale = 32766;
    public const short SeaLevel = NormalizedScale / 2;

    public static short EncodeWorldHeight(float metres)
    {
        if (!float.IsFinite(metres) || metres < MinimumWorldY || metres > MinimumWorldY + VerticalRange)
            throw new ArgumentOutOfRangeException(nameof(metres));
        return (short)((metres - MinimumWorldY) / VerticalRange * NormalizedScale + .5f);
    }

    public static float DecodeWorldHeight(short value) => MinimumWorldY + value / (float)NormalizedScale * VerticalRange;
}
