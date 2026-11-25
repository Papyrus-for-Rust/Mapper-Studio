using System.Text;
using System.Text.RegularExpressions;

namespace Mapper.Core;

public static partial class FileNameTemplate
{
    public static string Render(string template, GenerationProfile profile, Guid id, DateTimeOffset timestamp)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(template);
        var values = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase)
        {
            ["Date"] = timestamp.ToString("yyyy-MM-dd"), ["Size"] = profile.Size.ToString(),
            ["Seed"] = profile.Seed.ToString(), ["Type"] = "procedural", ["Id"] = id.ToString("N"),
            ["ShortId"] = id.ToString("N")[..8]
        };
        var rendered = TokenRegex().Replace(template, m => values.TryGetValue(m.Groups[1].Value, out var value) ? value : m.Value);
        var invalid = Path.GetInvalidFileNameChars().ToHashSet();
        var result = new StringBuilder(rendered.Length);
        foreach (var character in rendered) result.Append(invalid.Contains(character) ? '_' : character);
        var safe = result.ToString().Trim().TrimEnd('.');
        return string.IsNullOrWhiteSpace(safe) ? id.ToString("N") : safe;
    }

    public static string ResolveCollision(string directory, string baseName, string extension = ".map")
    {
        Directory.CreateDirectory(directory);
        var candidate = Path.Combine(directory, baseName + extension);
        for (var suffix = 2; File.Exists(candidate); suffix++) candidate = Path.Combine(directory, $"{baseName}_{suffix}{extension}");
        return candidate;
    }

    [GeneratedRegex("\\{(Date|Size|Seed|Type|Id|ShortId)\\}", RegexOptions.IgnoreCase | RegexOptions.CultureInvariant)]
    private static partial Regex TokenRegex();
}
