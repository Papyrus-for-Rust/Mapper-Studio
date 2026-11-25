using System.Security.Cryptography;
using System.Text.Json;
using System.Text.Json.Serialization;

// Data-only, bounded profile contract; call signatures and buffer layout remain compiled ABI code.
internal sealed class NativeSettings
{
    public required uint loot_angle_millidegrees { get; init; }
    public required uint biome_angle_millidegrees { get; init; }
    public required uint[] terrain_weights { get; init; }
    public required uint[] biome_weights { get; init; }
    public required uint jungle_weight { get; init; }
}
internal sealed class NativeProfile
{
    public required string format { get; init; }
    public required uint schema { get; init; }
    public required string id { get; init; }
    public required string abi { get; init; }
    public required string native_sha256 { get; init; }
    public required NativeSettings settings { get; init; }

    public static NativeProfile Load(string path, string digest, string nativeHash)
    {
        using var file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read);
        if (file.Length > 16 * 1024) throw new InvalidDataException("Native profile exceeds 16 KiB");
        var bytes = new byte[(int)file.Length]; file.ReadExactly(bytes);
        if (Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant() != digest) throw new InvalidDataException("Native profile checksum mismatch");
        var profile = JsonSerializer.Deserialize<NativeProfile>(bytes, new JsonSerializerOptions { UnmappedMemberHandling = JsonUnmappedMemberHandling.Disallow }) ?? throw new InvalidDataException("Missing native profile");
        if (profile.format != "papyrus-native-profile" || profile.schema != 1 || profile.abi != "rust-native-five-biomes-1") throw new InvalidDataException("Unsupported native profile schema or ABI family");
        if (string.IsNullOrEmpty(profile.id) || profile.id.Length > 64 || profile.id.Any(c => !(c is >= 'a' and <= 'z' or >= '0' and <= '9' or '-'))
            || profile.native_sha256 != nativeHash || nativeHash.Length != 64 || nativeHash.Any(c => !(c is >= 'a' and <= 'f' or >= '0' and <= '9')))
            throw new InvalidDataException("Native profile identity does not match the selected library");
        var s = profile.settings ?? throw new InvalidDataException("Missing native settings");
        if (s.terrain_weights is not { Length: 3 } || s.biome_weights is not { Length: 4 }
            || s.terrain_weights.Concat(s.biome_weights).Any(w => w > 10000) || s.terrain_weights.Sum(w => (long)w) != 10000 || s.biome_weights.Sum(w => (long)w) != 10000
            || s.jungle_weight > 10000 || s.loot_angle_millidegrees > 360000 || s.biome_angle_millidegrees > 360000)
            throw new InvalidDataException("Native profile settings exceed the supported ABI contract");
        return profile;
    }
}
