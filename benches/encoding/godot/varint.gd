extends RefCounted
# Schema field order matches support/varint.rs. StreamPeer has no varint API.
var stream := StreamPeerBuffer.new()

func put_uvarint(value: int) -> void:
	assert(value >= 0 and value <= 4294967295)
	while value >= 128:
		stream.put_u8((value & 127) | 128)
		value >>= 7
	stream.put_u8(value)

func get_uvarint() -> int:
	var value := 0
	for shift in range(0, 35, 7):
		assert(stream.get_available_bytes() > 0, "Truncated varint")
		var byte := stream.get_u8()
		assert(shift < 28 or byte <= 15, "Varint overflow")
		value |= (byte & 127) << shift
		if byte < 128:
			return value
	assert(false, "Unterminated varint")
	return 0

func put_floats(values: Array) -> void:
	put_uvarint(values.size())
	for value in values:
		stream.put_double(value)

func get_floats() -> Array:
	var count := get_uvarint()
	assert(count <= stream.get_available_bytes() / 8)
	var values := []
	for i in count:
		values.append(stream.get_double())
	return values

func put_entities(values: Array) -> void:
	put_uvarint(values.size())
	for value in values:
		put_uvarint(value.id)
		put_floats(value.position)
		put_floats(value.velocity)
		put_uvarint(value.health)
		put_uvarint(value.ammo)
		stream.put_u8(int(value.active))

func get_entities() -> Array:
	var count := get_uvarint()
	var values := []
	for i in count:
		var value := {"id": get_uvarint(), "position": get_floats(),
			"velocity": get_floats(), "health": get_uvarint(), "ammo": get_uvarint()}
		value.active = stream.get_u8() == 1
		values.append(value)
	return values

func encode(name: String, value: Variant) -> PackedByteArray:
	stream = StreamPeerBuffer.new()
	stream.big_endian = false
	if name == "input-state":
		put_uvarint(value[0])
		put_uvarint(value[1])
		for i in [2, 3]:
			put_uvarint((int(value[i]) << 1) ^ (int(value[i]) >> 63))
	elif name.begins_with("snapshot"):
		put_uvarint(value.tick)
		put_entities(value.entities)
		var text: PackedByteArray = value.map.to_utf8_buffer()
		put_uvarint(text.size())
		stream.put_data(text)
	else:
		put_uvarint(value.tick)
		put_uvarint(value.base_tick)
		put_entities(value.spawned)
		put_uvarint(value.updated.size())
		for update in value.updated:
			put_uvarint(update.id)
			put_floats(update.position)
			put_uvarint(update.health)
			put_uvarint(update.animation)
		put_uvarint(value.removed.size())
		for id in value.removed:
			put_uvarint(id)
		put_uvarint(value.tiles.size())
		for tile in value.tiles:
			for field in tile:
				put_uvarint(field)
	return stream.data_array

func decode(name: String, bytes: PackedByteArray) -> Variant:
	stream = StreamPeerBuffer.new()
	stream.big_endian = false
	stream.data_array = bytes
	var value: Variant
	var tick := get_uvarint()
	if name == "input-state":
		value = [tick, get_uvarint()]
		for i in 2:
			var axis := get_uvarint()
			value.append((axis >> 1) ^ -(axis & 1))
	elif name.begins_with("snapshot"):
		value = {"tick": tick, "entities": get_entities()}
		var length := get_uvarint()
		value.map = stream.get_data(length)[1].get_string_from_utf8()
	else:
		value = {"tick": tick, "base_tick": get_uvarint(), "spawned": get_entities(),
			"updated": [], "removed": [], "tiles": []}
		var count := get_uvarint()
		for i in count:
			value.updated.append({"id": get_uvarint(), "position": get_floats(),
				"health": get_uvarint(), "animation": get_uvarint()})
		count = get_uvarint()
		for i in count:
			value.removed.append(get_uvarint())
		count = get_uvarint()
		for i in count:
			value.tiles.append([get_uvarint(), get_uvarint(), get_uvarint()])
	assert(stream.get_available_bytes() == 0, "Trailing varint bytes")
	return value
