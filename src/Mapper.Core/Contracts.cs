namespace Mapper.Core;

public enum RustServerPlatform { Windows, Linux }
public sealed record RustInstallation(string ExecutablePath, string RootDirectory, string BuildIdentifier, RustServerPlatform Platform, bool IsKnownBuild, IReadOnlyList<string> Warnings);
public sealed record GenerationProgress(string Stage, string Message, double? Fraction = null);
public sealed record ServerGenerationRequest(Guid JobId, int Seed, int Size, string WorkingDirectory, SchedulerSettings Scheduler);
public sealed record ServerGenerationResult(string MapPath, string LogPath, string BuildIdentifier, string? LayerDirectory = null);

public interface IRustServerRunner
{
    Task<RustInstallation> ValidateInstallationAsync(string executablePath, CancellationToken cancellationToken = default);
    Task<ServerGenerationResult> GenerateAsync(ServerGenerationRequest request, IProgress<GenerationProgress>? progress = null, CancellationToken cancellationToken = default);
}

public sealed record MapProbe(uint Version, long Length, bool IsSupported, string? Warning = null);
public sealed record MapDocument(uint Version, byte[] OriginalBytes, IReadOnlyDictionary<string, object>? SemanticData = null);
public sealed record MapValidationResult(bool IsValid, IReadOnlyList<string> Errors, IReadOnlyList<string> Warnings);

public interface IMapCodec
{
    ValueTask<MapProbe> ProbeAsync(string path, CancellationToken cancellationToken = default);
    ValueTask<MapDocument> LoadAsync(string path, bool allowExperimentalVersion, CancellationToken cancellationToken = default);
    ValueTask SaveAtomicAsync(MapDocument map, string destination, CancellationToken cancellationToken = default);
    ValueTask<MapValidationResult> ValidateAsync(string path, CancellationToken cancellationToken = default);
}

public sealed record TransformPreview(string Name, bool CanApply, IReadOnlyList<string> Changes, IReadOnlyList<string> Diagnostics);
public interface IMapTransform
{
    string Name { get; }
    IReadOnlyCollection<string> Dependencies { get; }
    IReadOnlyCollection<string> Conflicts { get; }
    ValueTask<TransformPreview> PreviewAsync(MapDocument map, GenerationProfile profile, CancellationToken cancellationToken = default);
    ValueTask<MapDocument> ApplyAsync(MapDocument map, GenerationProfile profile, CancellationToken cancellationToken = default);
}

public interface IJobRepository
{
    Task InitializeAsync(CancellationToken cancellationToken = default);
    Task UpsertAsync(GenerationJob job, CancellationToken cancellationToken = default);
    Task<IReadOnlyList<GenerationJob>> ListAsync(CancellationToken cancellationToken = default);
    Task MarkRunningJobsInterruptedAsync(CancellationToken cancellationToken = default);
}
