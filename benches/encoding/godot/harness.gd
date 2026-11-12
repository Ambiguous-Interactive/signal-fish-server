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

func warmup(operation: Callable) -> Dictionary:
	var checksum := 0
	var count := 0
	var start := Time.get_ticks_usec()
	while Time.get_ticks_usec() - start < 1000000:
		var value: Variant = operation.call()
		checksum += value.size()
		count += 1
	return {"iterations": maxi(1, count / 10), "warmup_iterations": count,
		"warmup_ns": (Time.get_ticks_usec() - start) * 1000,
		"samples_ns": [], "order_positions": [], "checksum": checksum}

func sample(operation: Callable, timing: Dictionary, position: int) -> void:
	var start := Time.get_ticks_usec()
	for i in timing.iterations:
		var value: Variant = operation.call()
		timing.checksum += value.size()
	timing.samples_ns.append(float(Time.get_ticks_usec() - start) * 1000.0 / timing.iterations)
	timing.order_positions.append(position)

func _initialize() -> void:
	var args := OS.get_cmdline_user_args()
	if args.size() < 2:
		printerr("Expected repository root and output path")
		quit(1)
		return
	var root: String = args[0]
	var output := FileAccess.open(args[1], FileAccess.WRITE)
	var check_only := "--check" in args
	var seed := 1524
	for argument in args:
		if argument.begins_with("--seed="):
			var text: String = argument.substr(7)
			require(text.is_valid_int() and not text.begins_with("-"), "Seed must be an unsigned 32-bit integer")
			seed = int(text)
			require(seed >= 0 and seed <= 4294967295, "Seed exceeds uint32")
	if failed:
		output.close()
		quit(1)
		return
	var corpus := root.path_join("test-fixtures/encoding")
	var manifest: Dictionary = JSON.parse_string(FileAccess.get_file_as_string(corpus.path_join("manifest.json")))
	for fixture_index in manifest.fixtures.size():
		var fixture: Dictionary = manifest.fixtures[fixture_index]
		var path: String = corpus.path_join(fixture.file)
		var source_bytes := FileAccess.get_file_as_bytes(path)
		var hash := HashingContext.new()
		hash.start(HashingContext.HASH_SHA256)
		hash.update(source_bytes)
		require(source_bytes.size() == fixture.bytes, "Source byte count differs")
		require(hash.finish().hex_encode() == fixture.sha256, "Source hash differs")
		var source: Variant = normalize(JSON.parse_string(source_bytes.get_string_from_utf8()))
		var varint := Varint.new()
		var prepared := []
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
				"float_bits": bits(decoded), "encode": null, "decode": null, "benchmark": null}
			prepared.append({"encoder": encoder, "decoder": decoder, "record": record})
		if not check_only and not failed:
			var benchmark := {"method": "seeded-alternating", "seed": seed,
				"warmup_ms": 1000, "warmup_order": [], "sample_orders": []}
			var ordered: Array = prepared.duplicate()
			if (seed + fixture_index) % 2 != 0:
				ordered.reverse()
			for item in ordered:
				benchmark.warmup_order.append(item.record.codec)
				item.record.encode = warmup(item.encoder)
				item.record.decode = warmup(item.decoder)
				item.record.benchmark = benchmark
			for index in 10:
				if index != 0:
					ordered.reverse()
				var executed := []
				for position in ordered.size():
					var item: Dictionary = ordered[position]
					executed.append(item.record.codec)
					sample(item.encoder, item.record.encode, position)
					sample(item.decoder, item.record.decode, position)
				benchmark.sample_orders.append(executed)
		for item in prepared:
			output.store_line(JSON.stringify(item.record))
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
