using System.Security.Cryptography;
using System.Text;

namespace Mapper.Core;

/// <summary>Matches UnityEngine.StringEx.ManifestHash used by Rust's StringPool.</summary>
public static class RustPrefabId
{
    public static uint FromPath(string prefabPath)
    {
        if (string.IsNullOrWhiteSpace(prefabPath)) return 0;
        var bytes = MD5.HashData(Encoding.UTF8.GetBytes(prefabPath.ToLowerInvariant()));
        return BitConverter.ToUInt32(bytes, 0);
    }

    public static bool IsKnownInInstallation(string prefabPath, string? rustNativePath)
    {
        if (string.IsNullOrWhiteSpace(rustNativePath)) return false;
        var data = new FileInfo(rustNativePath).Directory?.Parent?.Parent?.Parent;
        var manifest = data?.Parent is { } root ? Path.Combine(root.FullName, "Bundles", "AssetSceneManifest.json") : null;
        return manifest is not null && File.Exists(manifest) && File.ReadAllText(manifest).Contains(prefabPath, StringComparison.OrdinalIgnoreCase);
    }
}
