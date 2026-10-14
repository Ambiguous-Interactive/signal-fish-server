using System.Diagnostics;
using System.Security.Cryptography;
using System.Text.Json;
using System.Text.Json.Nodes;
using Google.Protobuf;
using SignalFish.Encoding;

static class Study
{
    static uint U(JsonNode value, string key) => value[key]!.GetValue<uint>();
    static Entity EntityOf(JsonNode v)
    {
        var e = new Entity { Id = U(v, "id"), Health = U(v, "health"), Ammo = U(v, "ammo"), Active = v["active"]!.GetValue<bool>() };
        e.Position.Add(v["position"]!.AsArray().Select(n => n!.GetValue<double>()));
        e.Velocity.Add(v["velocity"]!.AsArray().Select(n => n!.GetValue<double>()));
        return e;
    }
    static IMessage Model(string name, JsonNode source)
    {
        if (name == "input-state") return new Input
        {
            Tick = source[0]!.GetValue<uint>(),
            Buttons = source[1]!.GetValue<uint>(),
            Horizontal = source[2]!.GetValue<int>(),
            Vertical = source[3]!.GetValue<int>()
        };
        if (name.StartsWith("snapshot", StringComparison.Ordinal))
        {
            var s = new Snapshot { Tick = U(source, "tick"), Map = source["map"]!.GetValue<string>() };
            s.Entities.Add(source["entities"]!.AsArray().Select(v => EntityOf(v!)));
            return s;
        }
        var d = new Delta { Tick = U(source, "tick"), BaseTick = U(source, "base_tick") };
        d.Spawned.Add(source["spawned"]!.AsArray().Select(v => EntityOf(v!)));
        foreach (var v in source["updated"]!.AsArray())
        {
            var update = new Update { Id = U(v!, "id"), Health = U(v!, "health"), Animation = U(v!, "animation") };
            update.Position.Add(v!["position"]!.AsArray().Select(n => n!.GetValue<double>()));
            d.Updated.Add(update);
        }
        d.Removed.Add(source["removed"]!.AsArray().Select(n => n!.GetValue<uint>()));
        d.Tiles.Add(source["tiles"]!.AsArray().Select(v => new Tile { X = v![0]!.GetValue<uint>(), Y = v[1]!.GetValue<uint>(), TileType = v[2]!.GetValue<uint>() }));
        return d;
    }
    static object EntityJson(Entity e) => new { id = e.Id, position = e.Position, velocity = e.Velocity, health = e.Health, ammo = e.Ammo, active = e.Active };
    static JsonNode Logical(IMessage message) => JsonSerializer.SerializeToNode(message switch
    {
        Input i => (object)new long[] { i.Tick, i.Buttons, i.Horizontal, i.Vertical },
        Snapshot s => new { tick = s.Tick, entities = s.Entities.Select(EntityJson), map = s.Map },
        Delta d => new
        {
            tick = d.Tick,
            base_tick = d.BaseTick,
            spawned = d.Spawned.Select(EntityJson),
            updated = d.Updated.Select(v => new { id = v.Id, position = v.Position, health = v.Health, animation = v.Animation }),
            removed = d.Removed,
            tiles = d.Tiles.Select(v => new uint[] { v.X, v.Y, v.TileType })
        },
        _ => throw new InvalidOperationException("Unknown message")
    })!;
    static IEnumerable<string> Bits(JsonNode? node)
    {
        if (node is JsonArray a) return a.SelectMany(Bits);
        if (node is JsonObject o) return o.SelectMany(pair => pair.Key is "position" or "velocity"
            ? pair.Value!.AsArray().Select(n => Convert.ToHexString(BitConverter.GetBytes(n!.GetValue<double>())).ToLowerInvariant())
            : Bits(pair.Value));
        return [];
    }
    static void Require(bool condition, string message) { if (!condition) throw new InvalidOperationException(message); }
    static object Samples(Func<object> operation)
    {
        long checksum = 0;
        void Consume(object value) { checksum = unchecked(checksum + (value is byte[] b ? b.Length : 1)); }
        var warm = Stopwatch.StartNew();
        int iterations = 0;
        do { Consume(operation()); iterations++; } while (warm.Elapsed.TotalMilliseconds < 1000);
        iterations = Math.Max(1, iterations / 10);
        var samples = new double[10];
        for (int sample = 0; sample < samples.Length; sample++)
        {
            long start = Stopwatch.GetTimestamp();
            for (int i = 0; i < iterations; i++) Consume(operation());
            samples[sample] = (Stopwatch.GetTimestamp() - start) * 1e9 / Stopwatch.Frequency / iterations;
        }
        return new { iterations, samples_ns = samples, checksum };
    }
    public static void Main(string[] args)
    {
        bool check = args.Contains("--check");
        string root = args.FirstOrDefault(a => !a.StartsWith("--", StringComparison.Ordinal)) ?? ".";
        string corpus = Path.Combine(root, "test-fixtures/encoding");
        var manifest = JsonNode.Parse(File.ReadAllText(Path.Combine(corpus, "manifest.json")))!;
        foreach (var f in manifest["fixtures"]!.AsArray())
        {
            string name = f!["name"]!.GetValue<string>();
            byte[] sourceBytes = File.ReadAllBytes(Path.Combine(corpus, f["file"]!.GetValue<string>()));
            Require(sourceBytes.Length == f["bytes"]!.GetValue<int>(), "Source byte count differs");
            Require(Convert.ToHexString(SHA256.HashData(sourceBytes)).ToLowerInvariant() == f["sha256"]!.GetValue<string>(), "Source hash differs");
            JsonNode source = JsonNode.Parse(sourceBytes)!;
            IMessage model = Model(name, source);
            Func<byte[], IMessage> decode = name == "input-state" ? b => Input.Parser.ParseFrom(b)
                : name.StartsWith("snapshot", StringComparison.Ordinal) ? b => Snapshot.Parser.ParseFrom(b) : b => Delta.Parser.ParseFrom(b);
            byte[] wire = model.ToByteArray();
            IMessage decoded = decode(wire);
            JsonNode logical = Logical(decoded);
            Require(JsonNode.DeepEquals(source, logical), $"Logical roundtrip differs: {name}");
            Require(Bits(source).SequenceEqual(Bits(logical)), "Float bits differ");
            Require(wire.SequenceEqual(decoded.ToByteArray()), "Wire is not deterministic");
            Console.WriteLine(JsonSerializer.Serialize(new
            {
                language = "csharp",
                runtime = System.Runtime.InteropServices.RuntimeInformation.FrameworkDescription,
                fixture = name,
                codec = "protobuf",
                bytes = wire.Length,
                wire_base64 = Convert.ToBase64String(wire),
                logical,
                float_bits = Bits(logical),
                encode = check ? null : Samples(() => model.ToByteArray()),
                decode = check ? null : Samples(() => decode(wire))
            }));
        }
        // Repeated doubles preserve signed zero and values not representable in float32.
        var edge = new Snapshot { Tick = uint.MaxValue, Map = "空🎮" };
        var entity = new Entity { Id = uint.MaxValue };
        entity.Position.Add(new double[] { -0.0, 0.1, double.Epsilon, double.MaxValue });
        edge.Entities.Add(entity);
        var copy = Snapshot.Parser.ParseFrom(edge.ToByteArray());
        Require(Bits(Logical(edge)).SequenceEqual(Bits(Logical(copy))), "Boundary float bits differ");
        Require(copy.Map == edge.Map && copy.Tick == edge.Tick, "Unicode or integer boundary differs");
        var input = new Input { Horizontal = int.MinValue, Vertical = int.MaxValue };
        Require(input.Equals(Input.Parser.ParseFrom(input.ToByteArray())), "Signed boundary differs");
    }
}
