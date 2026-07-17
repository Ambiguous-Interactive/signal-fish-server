extends Node

const CONFIG_KEY := "__FORTRESS_CONFIG"
const ROOM_KEY := "__FORTRESS_ROOM_READY"
const RESULT_KEY := "__FORTRESS_RESULT"

@onready var peer: Node = $FortressWasmPeer
var room_published := false
var result_published := false


func _ready() -> void:
	var config_json := JavaScriptBridge.eval(
		"JSON.stringify(globalThis.%s ?? null)" % CONFIG_KEY,
		true,
	)
	if typeof(config_json) != TYPE_STRING or config_json == "null":
		_publish_bridge_error("missing injected browser configuration")
		return
	var config = JSON.parse_string(config_json)
	if typeof(config) != TYPE_DICTIONARY:
		_publish_bridge_error("injected browser configuration is not an object")
		return
	var version_info := Engine.get_version_info()
	config["godot_runtime"] = {
		"major": version_info.get("major", -1),
		"minor": version_info.get("minor", -1),
		"patch": version_info.get("patch", -1),
		"status": version_info.get("status", ""),
		"build": version_info.get("build", ""),
		"hash": version_info.get("hash", ""),
		"string": version_info.get("string", ""),
	}
	if not peer.configure(JSON.stringify(config)):
		_publish_bridge_error("Rust fixture rejected browser configuration")


func _process(_delta: float) -> void:
	# Networking and Fortress progression live solely in the Rust Node's process
	# callback. This bridge only exports Rust-origin strings to the harness.
	if not room_published:
		var room_json: String = peer.take_room_json()
		if not room_json.is_empty():
			room_published = true
			JavaScriptBridge.eval("globalThis.%s = %s" % [ROOM_KEY, room_json], true)
			print("FORTRESS_WASM_ROOM ", room_json)
	if not result_published:
		var result_json: String = peer.take_report_json()
		if not result_json.is_empty():
			result_published = true
			JavaScriptBridge.eval("globalThis.%s = %s" % [RESULT_KEY, result_json], true)
			print("FORTRESS_WASM_RESULT ", result_json)


func _publish_bridge_error(message: String) -> void:
	if result_published:
		return
	result_published = true
	var result := {
		"schema_version": 1,
		"status": "complete",
		"runtime_error": message,
		"origin": "gdscript-bootstrap-error",
	}
	JavaScriptBridge.eval(
		"globalThis.%s = %s" % [RESULT_KEY, JSON.stringify(result)],
		true,
	)
