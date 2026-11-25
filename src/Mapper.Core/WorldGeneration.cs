namespace Mapper.Core;

public sealed record GenerationLimits(int MaximumThreads, long? MemoryLimitBytes = null);
public sealed record WorldLayer(string Name, int Resolution, int Channels, byte[] Data);
public sealed record WorldVector(float X, float Y, float Z);
public sealed record WorldPath(string Name, float Width, IReadOnlyList<WorldVector> Nodes, bool Spline = true, bool Start = true, bool End = true, int Splat = 0, int Topology = 0);
public sealed record WorldPrefab(string Category, uint Id, WorldVector Position, WorldVector Rotation, WorldVector Scale, string? SourcePath = null);
public sealed record TerrainWorld(int Size, IReadOnlyList<WorldLayer> Layers, IReadOnlyList<WorldPath>? Paths = null, IReadOnlyList<WorldPrefab>? Prefabs = null);
public sealed record GenerationMetrics(string EngineVersion, int Seed, int Threads, IReadOnlyDictionary<string, double> StageMilliseconds, long PeakWorkingSetBytes);
public sealed record WorldGenerationResult(TerrainWorld World, GenerationMetrics Metrics);

public interface IWorldGenerator
{
    Task<WorldGenerationResult> GenerateAsync(GenerationProfile profile, GenerationLimits limits,
        IProgress<GenerationProgress>? progress = null, CancellationToken cancellationToken = default);
}

/// <summary>Optional generation path that also publishes a map from the same worker.</summary>
public interface IDirectMapGenerator : IWorldGenerator
{
    bool SupportsDirectMapExport { get; }
    Task<WorldGenerationResult> GenerateToMapAsync(GenerationProfile profile, GenerationLimits limits, string destination,
        IProgress<GenerationProgress>? progress = null, CancellationToken cancellationToken = default);
}

public static class ProfileMigration
{
    public static GenerationProfile ToCurrent(GenerationProfile profile)
    {
        if (profile.SchemaVersion > GenerationProfile.CurrentSchemaVersion)
            throw new NotSupportedException($"Profile schema {profile.SchemaVersion} is not supported.");
        return profile with { SchemaVersion = GenerationProfile.CurrentSchemaVersion, Backend = GenerationBackend.Custom };
    }

    public static IReadOnlyList<string> Warnings(GenerationProfile profile)
    {
        var warnings = new List<string> { "Terrain-only map: gameplay features are not generated in this engine version." };
        if (profile.Terrain.Rivers.Enabled || profile.Terrain.Roads.Enabled)
            warnings.Add("Saved road and river settings are preserved but not applied.");
        if (profile.Transformations.Count + profile.CustomPackageIds.Count > 0)
            warnings.Add("Saved transformation and package settings are preserved but not applied.");
        return warnings;
    }
}
