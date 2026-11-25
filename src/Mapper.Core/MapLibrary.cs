namespace Mapper.Core;

public sealed record MapLibraryItem(
    Guid JobId,
    string MapPath,
    string ThumbnailPath,
    string Name,
    int Seed,
    int Size,
    string EngineVersion,
    DateTimeOffset CreatedAt,
    bool Kept = false,
    int Rating = 0,
    IReadOnlyList<string>? Tags = null);

public interface IMapLibraryRepository
{
    Task UpsertAsync(MapLibraryItem item, CancellationToken cancellationToken = default);
    Task<IReadOnlyList<MapLibraryItem>> ListAsync(CancellationToken cancellationToken = default);
}
