using System.Buffers.Binary;
using System.Diagnostics;
using System.Globalization;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text.Json;
using Mapper.Core;

// Only this disposable process loads proprietary native code. No game binaries are shipped.
internal static unsafe class Program
{
    [StructLayout(LayoutKind.Sequential)] private struct V3(float x, float y, float z) { public float X = x, Y = y, Z = z; }
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void Height(short* map, int res, V3 pos, V3 size, uint seed, float lootAngle, float t0, float t1, float t2, float biomeAngle, float arid, float temperate, float tundra, float arctic);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void Biome(byte* map, int res, V3 pos, V3 size, uint seed, float lootAngle, float t0, float t1, float t2, float biomeAngle, float arid, float temperate, float tundra, float arctic, float jungle, short* height, int heightRes);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void Topology(int* map, int res, V3 pos, V3 size, uint seed, float lootAngle, float t0, float t1, float t2, float biomeAngle, float arid, float temperate, float tundra, float arctic, short* height, int heightRes, byte* biome, int biomeRes);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void Splat(byte* map, int res, V3 pos, V3 size, uint seed, float lootAngle, float biomeAngle, short* height, int heightRes, byte* biome, int biomeRes, int* topology, int topologyRes);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate float Noise1D(float coordinate);
    private static T Export<T>(nint library, string name) where T : Delegate => Marshal.GetDelegateForFunctionPointer<T>(NativeLibrary.GetExport(library, name));

    private static Stream OpenTerrainOutput(string path)
    {
        var name = Environment.GetEnvironmentVariable("PAPYRUS_NATIVE_TERRAIN_PIPE");
        if (name is null) return new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None);
        if (!name.StartsWith("papyrus-terrain-", StringComparison.Ordinal) || name.Length > 80 ||
            name.Any(c => !char.IsAsciiLetterOrDigit(c) && c is not '-' and not '.'))
            throw new ArgumentException("Invalid native terrain pipe name");
        var pipe = new NamedPipeServerStream(name, PipeDirection.Out, 1, PipeTransmissionMode.Byte,
            PipeOptions.CurrentUserOnly | PipeOptions.Asynchronous, 0, 1 << 20);
        try
        {
            using var timeout = new CancellationTokenSource(TimeSpan.FromMinutes(2));
            pipe.WaitForConnectionAsync(timeout.Token).GetAwaiter().GetResult();
            return pipe;
        }
        catch
        {
            pipe.Dispose();
            throw;
        }
    }

    private static int Main(string[] args)
    {
        try { Run(args); return 0; }
        catch (Exception error) { Console.Error.WriteLine(error.Message); return 1; }
    }
    private static void Run(string[] args)
    {
        if (!OperatingSystem.IsWindows() || !Environment.Is64BitProcess) throw new PlatformNotSupportedException("Windows x64 required");
        if (args is ["--self-test"])
        {
            Console.WriteLine(JsonSerializer.Serialize(new { format = "papyrus-native-worker-self-test", schema = 1, platform = "win-x64", runtime = RuntimeInformation.FrameworkDescription, nativeLibraryLoaded = false }));
            return;
        }
        if (args.Length == 7 && args[0] == "--noise1d") { SampleNoise(args); return; }
        if (args.Length != 8 && (args.Length != 9 || args[8] != "--stage-previews")) throw new ArgumentException("Expected library, hash, output directory, size, terrain seed, biome seed, native profile and profile SHA-256, optionally --stage-previews");
        var live = args.Length == 9;
        var preview = live || Environment.GetEnvironmentVariable("PAPYRUS_NATIVE_TERRAIN_ONLY") != "1";
        var dll = Path.GetFullPath(args[0]); var expected = args[1]; var output = Path.GetFullPath(args[2]);
        var size = int.Parse(args[3], CultureInfo.InvariantCulture); var terrainSeed = uint.Parse(args[4], CultureInfo.InvariantCulture); var biomeSeed = uint.Parse(args[5], CultureInfo.InvariantCulture);
        if (size is < 1000 or > 6000 || terrainSeed is 0 or > int.MaxValue || biomeSeed is 0 or > int.MaxValue) throw new ArgumentException("Invalid recipe");
        var profile = NativeProfile.Load(args[6], args[7], expected);
        var settings = profile.settings;
        var loot = settings.loot_angle_millidegrees / 1000f; var angle = settings.biome_angle_millidegrees / 1000f;
        var tw = settings.terrain_weights.Select(w => w / 10000f).ToArray(); var bw = settings.biome_weights.Select(w => w / 10000f).ToArray(); var jungle = settings.jungle_weight / 10000f;
        // Deny writes/deletion while hashing and using this library, including a concurrent Steam update.
        using var source = new FileStream(dll, FileMode.Open, FileAccess.Read, FileShare.Read);
        if (source.Length > 64 * 1024 * 1024 || Convert.ToHexString(SHA256.HashData(source)).ToLowerInvariant() != expected) throw new InvalidDataException("RustNative changed since source selection");
        void Progress(int stage) => File.WriteAllText(Path.Combine(output, "progress.txt"), stage.ToString(CultureInfo.InvariantCulture));
        var timer = Stopwatch.StartNew(); var timings = new Dictionary<string, double>();
        var upper = (int)System.Numerics.BitOperations.RoundUpToPowerOf2((uint)size);
        var hr = Math.Min(4096, size - upper / 2 < upper - size ? upper / 2 : upper) + 1;
        var sr = Math.Min(2048, (int)System.Numerics.BitOperations.RoundUpToPowerOf2((uint)(size / 2)));
        var height = new short[hr * hr]; var biome = new byte[sr * sr * 5]; var topology = new int[sr * sr]; var splat = new byte[sr * sr * 8];
        var snapshotMilliseconds = 0d; var snapshotCount = 0;
        void Snapshot(int stage)
        {
            // Preview work is optional. Keep the last frame if earlier captures used the budget.
            if (!live || snapshotMilliseconds >= 75d) return;
            var clock = Stopwatch.StartNew();
            const int n = 257;
            var sampled = new short[n * n];
            for (var z = 0; z < n; z++) for (var x = 0; x < n; x++) sampled[z * n + x] = height[z * (hr - 1) / (n - 1) * hr + x * (hr - 1) / (n - 1)];
            byte[] Sample(byte[] layer, int channels)
            {
                var result = new byte[n * n * channels];
                for (var c = 0; c < channels; c++) for (var z = 0; z < n; z++) for (var x = 0; x < n; x++) result[c * n * n + z * n + x] = layer[c * sr * sr + z * (sr - 1) / (n - 1) * sr + x * (sr - 1) / (n - 1)];
                return result;
            }
            // Called only after a synchronous kernel returns. No concurrent native buffer reads.
            var image = CompositeMapRenderer.Render(MemoryMarshal.AsBytes(sampled.AsSpan()).ToArray(), Sample(splat, 8), Sample(biome, 5), size);
            var path = Path.Combine(output, $"stage-{stage}.bin");
            using (var writer = new BinaryWriter(new FileStream(path + ".tmp", FileMode.CreateNew)))
            {
                writer.Write("PNS1"u8); writer.Write(stage); writer.Write(n); writer.Write(image.Bgra);
            }
            File.Move(path + ".tmp", path);
            snapshotCount++; snapshotMilliseconds += clock.Elapsed.TotalMilliseconds;
        }
        var library = NativeLibrary.Load(dll);
        try
        {
            var gh = Export<Height>(library, "generate_height"); var gb = Export<Biome>(library, "generate_biome");
            var gt = Export<Topology>(library, "generate_topology"); var gs = Export<Splat>(library, "generate_splat");
            var origin = new V3(-size / 2f, -500, -size / 2f); var dimensions = new V3(size, 1000, size);
            // Versioned preview defaults, not recovered procedural scene ordering or official seed parity.
            fixed (short* h = height) fixed (byte* b = biome) fixed (int* t = topology) fixed (byte* s = splat)
            {
                Progress(1); timer.Restart(); gh(h, hr, origin, dimensions, terrainSeed, loot, tw[0], tw[1], tw[2], angle, bw[0], bw[1], bw[2], bw[3]); timings["height"] = timer.Elapsed.TotalMilliseconds;
                Snapshot(1);
                Progress(2); timer.Restart(); gb(b, sr, origin, dimensions, biomeSeed, loot, tw[0], tw[1], tw[2], angle, bw[0], bw[1], bw[2], bw[3], jungle, h, hr); timings["biome"] = timer.Elapsed.TotalMilliseconds;
                Snapshot(2);
                Progress(3); timer.Restart(); gt(t, sr, origin, dimensions, terrainSeed, loot, tw[0], tw[1], tw[2], angle, bw[0], bw[1], bw[2], bw[3], h, hr, b, sr); timings["topology"] = timer.Elapsed.TotalMilliseconds;
                Progress(4); timer.Restart(); gs(s, sr, origin, dimensions, terrainSeed, loot, angle, h, hr, b, sr, t, sr); timings["splat"] = timer.Elapsed.TotalMilliseconds;
                Snapshot(4);
            }
            var firstHeight = height[0];
            var varies = false;
            foreach (var value in height)
            {
                if ((uint)value > 32766u) throw new InvalidDataException("Invalid native heights");
                varies |= value != firstHeight;
            }
            if (!varies) throw new InvalidDataException("Invalid native heights");
            Progress(5);
            var fullPath = Path.Combine(output, "terrain.bin");
            timer.Restart();
            string fullTerrainSha256;
            using (var stream = OpenTerrainOutput(fullPath))
            using (var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256))
            {
                void WriteTerrain(ReadOnlySpan<byte> bytes)
                {
                    stream.Write(bytes);
                    hash.AppendData(bytes);
                }
                Span<byte> header = stackalloc byte[24];
                "PNF1"u8.CopyTo(header);
                BinaryPrimitives.WriteInt32LittleEndian(header[4..], size);
                BinaryPrimitives.WriteUInt32LittleEndian(header[8..], terrainSeed);
                BinaryPrimitives.WriteUInt32LittleEndian(header[12..], biomeSeed);
                BinaryPrimitives.WriteInt32LittleEndian(header[16..], hr);
                BinaryPrimitives.WriteInt32LittleEndian(header[20..], sr);
                WriteTerrain(header);
                WriteTerrain(MemoryMarshal.AsBytes(height.AsSpan()));
                WriteTerrain(biome);
                WriteTerrain(splat);
                WriteTerrain(MemoryMarshal.AsBytes(topology.AsSpan()));
                if (stream is NamedPipeServerStream pipe) pipe.WaitForPipeDrain();
                fullTerrainSha256 = Convert.ToHexString(hash.GetHashAndReset()).ToLowerInvariant();
            }
            timings["terrainWrite"] = timer.Elapsed.TotalMilliseconds;
            const int n = 513;
            string? heightSha256 = null;
            if (preview)
            {
                timer.Restart();
                // Sampled source heights and the north-up BGRA overview.
                var sampled = new short[n * n];
                for (var z = 0; z < n; z++) for (var x = 0; x < n; x++) sampled[z * n + x] = height[z * (hr - 1) / (n - 1) * hr + x * (hr - 1) / (n - 1)];
                byte[] Sample(byte[] layer, int channels)
                {
                    var result = new byte[n * n * channels];
                    for (var c = 0; c < channels; c++) for (var z = 0; z < n; z++) for (var x = 0; x < n; x++) result[c * n * n + z * n + x] = layer[c * sr * sr + z * (sr - 1) / (n - 1) * sr + x * (sr - 1) / (n - 1)];
                    return result;
                }
                var bytes = MemoryMarshal.AsBytes(sampled.AsSpan()).ToArray();
                var image = CompositeMapRenderer.Render(bytes, Sample(splat, 8), Sample(biome, 5), size);
                using var writer = new BinaryWriter(new FileStream(Path.Combine(output, "preview.bin"), FileMode.CreateNew));
                writer.Write("PNP1"u8); writer.Write(n); writer.Write(bytes); writer.Write(image.Bgra);
                heightSha256 = Convert.ToHexString(SHA256.HashData(bytes));
                timings["preview"] = timer.Elapsed.TotalMilliseconds;
            }
            File.WriteAllText(Path.Combine(output, "metrics.json"), JsonSerializer.Serialize(new { adapter = "papyrus-native-preview-1", sourceSha256 = expected, nativeProfileSha256 = args[7], nativeProfileId = profile.id, nativeAbi = profile.abi, size, terrainSeed, biomeSeed, heightResolution = hr, surfaceResolution = sr, previewResolution = preview ? n : 0, timings, snapshotCount, snapshotMilliseconds, fullTerrainSha256, heightSha256, peakWorkingSetBytes = Process.GetCurrentProcess().PeakWorkingSet64 }));
        }
        finally { NativeLibrary.Free(library); }
    }
    private static void SampleNoise(string[] args)
    {
        // This mode only evaluates a source-bound batch. It does not run terrain
        // generation or load native code into the workstation process.
        var dll = Path.GetFullPath(args[1]); var expected = args[2];
        _ = NativeProfile.Load(args[3], args[4], expected);
        byte[] request;
        using (var input = File.OpenRead(args[5]))
        {
            if (input.Length > 16 * 1024 * 1024) throw new InvalidDataException("Noise request exceeds 16 MiB");
            request = new byte[checked((int)input.Length)]; input.ReadExactly(request);
        }
        using var document = JsonDocument.Parse(request, new JsonDocumentOptions { MaxDepth = 3 });
        var root = document.RootElement;
        if (root.ValueKind != JsonValueKind.Object || root.EnumerateObject().Count() != 3
            || root.GetProperty("format").GetString() != "papyrus-native-noise1d-query"
            || root.GetProperty("schema").GetInt32() != 1) throw new InvalidDataException("Invalid noise request schema");
        var coordinates = root.GetProperty("coordinates");
        if (coordinates.ValueKind != JsonValueKind.Array || coordinates.GetArrayLength() is < 1 or > 1_000_000) throw new InvalidDataException("Noise batch exceeds supported bounds");
        var values = new float[coordinates.GetArrayLength()]; var index = 0;
        foreach (var coordinate in coordinates.EnumerateArray())
        {
            var value = coordinate.GetSingle();
            if (!float.IsFinite(value) || MathF.Abs(value) > 1_000_000f) throw new InvalidDataException("Invalid noise coordinate");
            values[index++] = value;
        }
        using var source = new FileStream(dll, FileMode.Open, FileAccess.Read, FileShare.Read);
        if (source.Length > 64 * 1024 * 1024 || Convert.ToHexString(SHA256.HashData(source)).ToLowerInvariant() != expected) throw new InvalidDataException("RustNative changed since noise source selection");
        var library = NativeLibrary.Load(dll);
        try
        {
            var noise = Export<Noise1D>(library, "snoise1_32");
            using var payload = new MemoryStream(checked(104 + values.Length * 4));
            using (var writer = new BinaryWriter(payload, System.Text.Encoding.UTF8, leaveOpen: true))
            {
                writer.Write("PNN1"u8); writer.Write(values.Length);
                writer.Write(SHA256.HashData(request));
                writer.Write(Convert.FromHexString(expected)); writer.Write(Convert.FromHexString(args[4]));
                foreach (var coordinate in values)
                {
                    var value = noise(coordinate);
                    if (!float.IsFinite(value)) throw new InvalidDataException("Native noise returned a non-finite value");
                    writer.Write(value);
                }
            }
            using var output = new FileStream(args[6], FileMode.CreateNew, FileAccess.Write, FileShare.None);
            var bytes = payload.GetBuffer().AsSpan(0, checked((int)payload.Length));
            // The parent reads this temporary file after worker exit and verifies
            // the checksum. Disposing the stream is sufficient for the handoff.
            output.Write(bytes);
            output.Write(SHA256.HashData(bytes));
        }
        finally { NativeLibrary.Free(library); }
    }
}
