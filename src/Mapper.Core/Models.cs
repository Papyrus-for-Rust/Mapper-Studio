using System.Collections.ObjectModel;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Mapper.Core;

public enum JobState { Queued, Preparing, Generating, PostProcessing, Validating, Completed, Failed, Cancelled, Interrupted }
public enum ResourcePreset { FastSingle, Balanced, MaximumThroughput, Custom }
public enum MonumentPreference { NoPreference, Required, Blocked }
public enum GenerationBackend { RustDedicated = 0, CleanRoom = 1, Custom = 2 }

public sealed record MonumentPolicy(
    string PrefabPath,
    MonumentPreference Preference = MonumentPreference.NoPreference,
    int DesiredCount = 1,
    bool RelaxPlacementRules = false);

/// <summary>Settings for the deterministic automatic monument pass. A zero seed follows the world seed.</summary>
public sealed record MonumentGenerationSettings(
    bool Enabled = true,
    int Seed = 0,
    int TargetCount = 0,
    bool IncludeLargeMonuments = true);

public sealed record SchedulerSettings(
    ResourcePreset Preset = ResourcePreset.Balanced,
    int? MaximumWorkers = null,
    int? CpuRatePercent = null,
    long? MemoryLimitBytes = null);

public sealed record BiomePercentages(int Arid = 40, int Temperate = 15, int Tundra = 15, int Arctic = 30)
{
    public int Total => Arid + Temperate + Tundra + Arctic;
}

public sealed record RiverSettings(bool Enabled = true, int Count = 2, double Width = 8, int MaximumLength = 0);
public sealed record RoadSettings(bool Enabled = true, bool GenerateRingRoad = true, int BranchCount = 4, double Width = 12, int MaximumGradePercent = 12);
public sealed record ErosionSettings(bool Enabled = true);
public sealed record TerrainGenerationSettings(BiomePercentages Biomes, RiverSettings Rivers, RoadSettings Roads, ErosionSettings? Erosion = null);
/// <summary>Component indices from a verified Rust world scene. Rust passes World.Seed + index to each stage.</summary>
public sealed record NativeStageSeedOffsets(int Height = 0, int Biome = 0, int Topology = 0, int Splat = 0);

public sealed record GenerationProfile
{
    public const int CurrentSchemaVersion = 4;
    public int SchemaVersion { get; init; } = CurrentSchemaVersion;
    public string Name { get; init; } = "New profile";
    public int Seed { get; init; } = Random.Shared.Next(1, int.MaxValue);
    public int Size { get; init; } = 4000;
    public string NamingTemplate { get; init; } = "{Date}_{Size}_{Seed}";
    public SchedulerSettings Scheduler { get; init; } = new();
    public GenerationBackend Backend { get; init; } = GenerationBackend.Custom;
    /// <summary>Optional Rust client RustNative.dll used locally; never copied into an export.</summary>
    public string? NativeLibraryPath { get; init; }
    /// <summary>Optional verified component indices for the selected Rust build. Defaults keep the custom pipeline deterministic.</summary>
    public NativeStageSeedOffsets NativeStageSeedOffsets { get; init; } = new();
    public TerrainGenerationSettings Terrain { get; init; } = new(new(), new(false), new(false));
    public IReadOnlyList<MonumentPolicy> MonumentPolicies { get; init; } = [];
    public MonumentGenerationSettings Monuments { get; init; } = new();
    public IReadOnlyList<string> Transformations { get; init; } = [];
    public IReadOnlyList<string> CustomPackageIds { get; init; } = [];
    [JsonExtensionData] public IDictionary<string, JsonElement>? ExtensionData { get; init; }

    public IReadOnlyList<string> Validate()
    {
        var errors = new Collection<string>();
        if (SchemaVersion > CurrentSchemaVersion) errors.Add($"Profile schema {SchemaVersion} is newer than supported schema {CurrentSchemaVersion}.");
        if (Seed is < 1) errors.Add("Seed must be a positive 32-bit integer.");
        if (Size is < 1000 or > 6000) errors.Add("Map size must be between 1000 and 6000.");
        if (string.IsNullOrWhiteSpace(Name)) errors.Add("Profile name is required.");
        if (!string.IsNullOrWhiteSpace(NativeLibraryPath) && !File.Exists(NativeLibraryPath)) errors.Add("Configured RustNative.dll was not found.");
        if (string.IsNullOrWhiteSpace(NamingTemplate)) errors.Add("Naming template is required.");
        if (new[] { Terrain.Biomes.Arid, Terrain.Biomes.Temperate, Terrain.Biomes.Tundra, Terrain.Biomes.Arctic }.Any(value => value < 0 || value > 100)) errors.Add("Biome percentages must be between 0 and 100.");
        if (Scheduler.MaximumWorkers is <= 0 || Scheduler.CpuRatePercent is <= 0 or > 100 || Scheduler.MemoryLimitBytes is <= 0) errors.Add("Resource limits must be positive and CPU percentage at most 100.");
        if (Terrain.Biomes.Total != 100) errors.Add("Biome percentages must total 100%.");
        if (Terrain.Rivers.Count < 0 || Terrain.Rivers.Width <= 0 || Terrain.Rivers.MaximumLength < 0) errors.Add("River settings contain an invalid value.");
        if (Terrain.Roads.BranchCount < 0 || Terrain.Roads.Width <= 0 || Terrain.Roads.MaximumGradePercent is < 0 or > 100) errors.Add("Road settings contain an invalid value.");
        foreach (var policy in MonumentPolicies)
        {
            if (string.IsNullOrWhiteSpace(policy.PrefabPath)) errors.Add("Every monument policy needs a prefab path.");
            if (policy.DesiredCount < 0) errors.Add($"Desired count for {policy.PrefabPath} cannot be negative.");
        }
        if (Monuments.Seed < 0) errors.Add("Monument seed must be zero (follow map seed) or a positive 32-bit integer.");
        if (Monuments.TargetCount is < 0 or > 100) errors.Add("Automatic monument count must be between 0 and 100.");
        return errors;
    }
}

public sealed record JobArtifacts(string WorkingDirectory, string? VanillaMap = null, string? FinalMap = null, string? LogFile = null, string? PreviewFile = null);

public sealed record GenerationJob
{
    public Guid Id { get; init; } = Guid.NewGuid();
    public required GenerationProfile Profile { get; init; }
    public string? RustExecutable { get; init; }
    public string? RustBuild { get; init; }
    public JobState State { get; init; } = JobState.Queued;
    public string Stage { get; init; } = "Waiting";
    public double ProgressFraction { get; init; }
    public DateTimeOffset CreatedAt { get; init; } = DateTimeOffset.UtcNow;
    public DateTimeOffset? StartedAt { get; init; }
    public DateTimeOffset? FinishedAt { get; init; }
    public JobArtifacts? Artifacts { get; init; }
    public IReadOnlyList<string> Warnings { get; init; } = [];
    public string? Error { get; init; }
    public bool AllowExperimentalMapVersion { get; init; }
}
