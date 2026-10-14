extends SceneTree
const MessagePack = preload("res://messagepack.gd")
const Varint = preload("res://varint.gd")
var failed := false

func require(condition: bool, message: String) -> void:
	if not condition:
		failed = true
		printerr(message)

# Godot JSON stores all numbers as doubles. Restore the corpus integer fields
# before timing. Coordinates remain float64, including signed zero.
func normalize(value: Variant, coordinates := false) -> Variant:
	if value is Dictionary:
		for key in value:
			value[key] = normalize(value[key], key in ["position", "velocity"])
	elif value is Array:
		for i in value.size():
			value[i] = normalize(value[i], coordinates)
	elif value is float and not coordinates:
		return int(value)
	return value

func bits(value: Variant) -> Array:
	var result := []
	if value is Dictionary:
		for key in value:
			if key in ["position", "velocity"]:
				for number in value[key]:
					var b := PackedByteArray()
					b.resize(8)
					b.encode_double(0, number)
					result.append(b.hex_encode())
			else:
				result.append_array(bits(value[key]))
	elif value is Array:
		for item in value:
			result.append_array(bits(item))
	return result

func pack(value: Variant) -> PackedByteArray:
	var result: Dictionary = MessagePack.encode(value)
	require(result.status == null, "MessagePack encoding failed")
	return result.value

func unpack(bytes: PackedByteArray) -> Variant:
	var result: Dictionary = MessagePack.decode(bytes)
	require(result.status == null, "MessagePack decoding failed")
	return result.value

func samples(operation: Callable) -> Dictionary:
	var checksum := 0
	var iterations := 0
	var start := Time.get_ticks_usec()
	while Time.get_ticks_usec() - start < 1000000:
		var value: Variant = operation.call()
		checksum += value.size()
		iterations += 1
	iterations = maxi(1, iterations / 10)
	var times := []
	for sample in 10:
		start = Time.get_ticks_usec()
		for i in iterations:
			var value: Variant = operation.call()
			checksum += value.size()
		times.append(float(Time.get_ticks_usec() - start) * 1000.0 / iterations)
	return {"iterations": iterations, "samples_ns": times, "checksum": checksum}

func _initialize() -> void:
	var args := OS.get_cmdline_user_args()
	if args.size() < 2:
		printerr("Expected repository root and output path")
		quit(1)
		return
	var root: String = args[0]
	var output := FileAccess.open(args[1], FileAccess.WRITE)
	var check_only := "--check" in args
	var corpus := root.path_join("test-fixtures/encoding")
	var manifest: Dictionary = JSON.parse_string(FileAccess.get_file_as_string(corpus.path_join("manifest.json")))
	for fixture in manifest.fixtures:
		var path: String = corpus.path_join(fixture.file)
		var source_bytes := FileAccess.get_file_as_bytes(path)
		var hash := HashingContext.new()
		hash.start(HashingContext.HASH_SHA256)
		hash.update(source_bytes)
		require(source_bytes.size() == fixture.bytes, "Source byte count differs")
		require(hash.finish().hex_encode() == fixture.sha256, "Source hash differs")
		var source: Variant = normalize(JSON.parse_string(source_bytes.get_string_from_utf8()))
		var varint := Varint.new()
		for codec in ["msgpack-f32", "schema-varint"]:
			var encoder: Callable = pack.bind(source) if codec == "msgpack-f32" else varint.encode.bind(fixture.name, source)
			var wire: PackedByteArray = encoder.call()
			var decoder: Callable = unpack.bind(wire) if codec == "msgpack-f32" else varint.decode.bind(fixture.name, wire)
			var decoded: Variant = decoder.call()
			require(decoded == source, "Logical roundtrip differs: " + fixture.name)
			require(bits(decoded) == bits(source), "Float bits differ: " + fixture.name)
			require(encoder.call() == wire, "Wire is not deterministic")
			var record := {"language": "gdscript", "runtime": Engine.get_version_info().string,
				"fixture": fixture.name, "codec": codec, "bytes": wire.size(),
				"wire_base64": Marshalls.raw_to_base64(wire), "logical": decoded,
				"float_bits": bits(decoded), "encode": null, "decode": null}
			if not check_only and not failed:
				record.encode = samples(encoder)
				record.decode = samples(decoder)
			output.store_line(JSON.stringify(record))
	# Expose the library's float32 precision limit. Do not repair it in the study.
	var edge: Variant = unpack(pack(0.1))
	require(edge != 0.1, "Expected the pinned MessagePack float32 limitation")
	var varint := Varint.new()
	var input := [4294967295, 4294967295, -2147483648, 2147483647]
	require(varint.decode("input-state", varint.encode("input-state", input)) == input, "Integer boundary differs")
	var empty := {"tick": 0, "entities": [], "map": "空🎮"}
	require(varint.decode("snapshot-small", varint.encode("snapshot-small", empty)) == empty, "Empty or Unicode boundary differs")
	var entity := {"id": 0, "position": [-0.0, 0.1, 1.23456789012345], "velocity": [], "health": 0, "ammo": 0, "active": false}
	var floats := {"tick": 0, "entities": [entity], "map": ""}
	require(bits(varint.decode("snapshot-small", varint.encode("snapshot-small", floats))) == bits(floats), "Varint float boundary differs")
	output.close()
	quit(1 if failed else 0)
