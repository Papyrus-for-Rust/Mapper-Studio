using System.Security.Cryptography;

namespace Mapper.Core;

/// <summary>Expands a reproducible basic or advanced profile into queued job snapshots.</summary>
public sealed record BatchGenerationRequest(GenerationProfile Profile, int Count = 1, bool RandomizeSeeds = false)
{
    public IReadOnlyList<GenerationJob> Expand(Func<int>? randomSeed = null)
    {
        if (Count is < 1 or > 100) throw new ArgumentOutOfRangeException(nameof(Count), "Batch count must be between 1 and 100.");
        var errors = Profile.Validate(); if (errors.Count > 0) throw new ArgumentException(string.Join(Environment.NewLine, errors));
        if (!RandomizeSeeds && (long)Profile.Seed + Count - 1 > int.MaxValue) throw new ArgumentException("Seed range exceeds the supported maximum.");
        randomSeed ??= () => RandomNumberGenerator.GetInt32(1, int.MaxValue);
        var used = new HashSet<int>(); var jobs = new List<GenerationJob>(Count);
        for (var index = 0; index < Count; index++)
        {
            var seed = RandomizeSeeds ? NextUnique(randomSeed, used) : Profile.Seed + index;
            jobs.Add(new GenerationJob { Profile = Profile with { Seed = seed } });
        }
        return jobs;
    }

    private static int NextUnique(Func<int> next, ISet<int> used)
    {
        for (var attempts = 0; attempts < 1000; attempts++) { var candidate = next(); if (candidate > 0 && used.Add(candidate)) return candidate; }
        throw new InvalidOperationException("Could not choose a unique random seed for this batch.");
    }
}
